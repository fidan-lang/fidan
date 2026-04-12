// fidan-runtime/src/ffi.rs
//
// C-ABI exported runtime functions for the Cranelift AOT backend.
//
// Every `#[unsafe(no_mangle)] pub extern "C" fn fdn_*` here is declared in
// `fidan-codegen-cranelift/src/aot.rs` (RuntimeDecls) and linked by the
// system linker.
//
// ## Ownership convention
//
// - `*mut FidanValue` parameters are BORROWED.  The callee NEVER drops them.
//   The AOT-generated code retains ownership and calls `fdn_drop` (via
//   `Instr::Drop`) when the local goes out of scope.
//
// - `*mut FidanValue` return values are OWNED.  Allocated by the callee; the
//   caller must eventually call `fdn_drop` on the returned pointer.
//
// - `fdn_drop(ptr)` is the only function that consumes (drops) its argument.
//
// This borrow-by-default convention is safe for Fidan's SSA-based MIR where
// a value may be read multiple times before its `Instr::Drop` fires.
//
// ## Object representation (Phase 11.1)
//
// Creating a full `FidanObject` at AOT runtime requires a symbol interner
// that is not available in the linked binary.  For Phase 11.1 we represent
// objects as `FidanValue::Dict` (string-keyed `FidanDict`) — transparent to
// the AOT-generated code because all field accesses go through
// `fdn_obj_{get,set}_field`.  Full class-table support is Phase 11.2.
//
// ## Temp-box leaks (Phase 11.1)
//
// `aot.rs` boxes scalars with `fdn_box_int/float/bool` and passes the
// result to a C-ABI call.  With borrow semantics the callee never frees these
// temporary boxes; they leak for the process lifetime.  Each leak is ≤ 32 B.
// Phase 11.2 will insert explicit `fdn_drop` calls for dead temporaries.

#![allow(clippy::missing_safety_doc)]
// In Rust 2024 edition, unsafe operations inside `unsafe fn` bodies require
// an explicit `unsafe { }` block.  We suppress this lint here because ffi.rs
// is pure FFI glue: every public function is already `unsafe extern "C"`, and
// adding inner unsafe blocks everywhere would add noise without safety benefit.
#![allow(unsafe_op_in_unsafe_fn)]

use crate::{
    FidanDict, FidanHashSet, FidanList, FidanString, OwnedRef, SharedRef,
    parallel::{FidanPending, ParallelArgs, ParallelCapture},
    stdlib,
    value::{FidanValue, FunctionId, display},
};
use fidan_config::{
    BuiltinSemantic, ReceiverBuiltinKind, ReceiverMethodOp, builtin_semantic, infer_receiver_member,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{
    cell::RefCell,
    io::{BufRead, BufWriter, IsTerminal, LineWriter, Write},
};

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Borrow a raw pointer as `&FidanValue` without taking ownership.
#[inline(always)]
unsafe fn borrow<'a>(ptr: *mut FidanValue) -> &'a FidanValue {
    debug_assert!(!ptr.is_null(), "fdn_*: null ptr");
    &*ptr
}

enum StdoutBuffer {
    Terminal(LineWriter<std::io::Stdout>),
    Redirected(BufWriter<std::io::Stdout>),
}

impl StdoutBuffer {
    fn new() -> Self {
        let stdout = std::io::stdout();
        if stdout.is_terminal() {
            Self::Terminal(LineWriter::new(stdout))
        } else {
            Self::Redirected(BufWriter::with_capacity(64 * 1024, stdout))
        }
    }
}

impl Write for StdoutBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Terminal(stdout) => stdout.write(buf),
            Self::Redirected(stdout) => stdout.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Terminal(stdout) => stdout.flush(),
            Self::Redirected(stdout) => stdout.flush(),
        }
    }
}

thread_local! {
    static STDOUT_BUFFER: RefCell<StdoutBuffer> = RefCell::new(StdoutBuffer::new());
}

fn with_stdout_buffer<R>(callback: impl FnOnce(&mut StdoutBuffer) -> R) -> R {
    STDOUT_BUFFER.with(|buffer| callback(&mut buffer.borrow_mut()))
}

fn flush_stdout_buffer() {
    with_stdout_buffer(|stdout| {
        let _ = stdout.flush();
    });
}

/// Allocate a new owned `FidanValue` and return its raw pointer.
#[inline(always)]
fn into_raw(v: FidanValue) -> *mut FidanValue {
    Box::into_raw(Box::new(v))
}

fn panic_missing_method(receiver: &FidanValue, method_name: &str) -> ! {
    let msg = format!(
        "no method `{}` found for `{}`",
        method_name,
        receiver.type_name()
    );
    let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
    unsafe { fdn_panic(msg_val) }
}

fn panic_runtime_message(message: impl Into<String>) -> ! {
    let message = message.into();
    let msg_val = into_raw(FidanValue::String(FidanString::new(&message)));
    unsafe { fdn_panic(msg_val) }
}

fn integer_display_len(value: i64) -> usize {
    if value == 0 {
        return 1;
    }
    let magnitude = value.unsigned_abs();
    let digits = magnitude.ilog10() as usize + 1;
    if value < 0 { digits + 1 } else { digits }
}

fn display_len_hint(value: &FidanValue) -> usize {
    match value {
        FidanValue::Integer(n) => integer_display_len(*n),
        FidanValue::Float(_) => 24,
        FidanValue::Boolean(true) => 4,
        FidanValue::Boolean(false) => 5,
        FidanValue::Handle(_) => 24,
        FidanValue::Nothing => 7,
        FidanValue::String(s) => s.len(),
        FidanValue::Function(_) | FidanValue::Closure { .. } => 16,
        FidanValue::Namespace(module) => module.len() + 9,
        FidanValue::StdlibFn(module, name) => module.len() + name.len() + 10,
        FidanValue::EnumType(name) => name.len() + 7,
        FidanValue::ClassType(name) => name.len() + 8,
        FidanValue::EnumVariant { tag, payload } => {
            tag.len() + if payload.is_empty() { 0 } else { 16 }
        }
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            integer_display_len(*start) + integer_display_len(*end) + if *inclusive { 3 } else { 2 }
        }
        FidanValue::Pending(_) | FidanValue::PendingTask(_) => 9,
        FidanValue::Shared(_) | FidanValue::WeakShared(_) => 16,
        FidanValue::List(list) => list.borrow().len().saturating_mul(8).saturating_add(2),
        FidanValue::Tuple(items) => items.len().saturating_mul(8).saturating_add(2),
        FidanValue::Dict(dict) => dict.borrow().len().saturating_mul(16).saturating_add(2),
        FidanValue::HashSet(set) => set.borrow().len().saturating_mul(8).saturating_add(10),
        FidanValue::Object(_) => 16,
    }
}

/// Coerce to `FidanString` if possible, otherwise stringify.
#[inline]
fn is_truthy(v: &FidanValue) -> bool {
    match v {
        FidanValue::Nothing => false,
        FidanValue::Boolean(b) => *b,
        FidanValue::Integer(n) => *n != 0,
        FidanValue::Float(f) => *f != 0.0,
        FidanValue::String(s) => !s.is_empty(),
        FidanValue::List(l) => !l.borrow().is_empty(),
        FidanValue::Dict(d) => !d.borrow().is_empty(),
        FidanValue::HashSet(s) => !s.borrow().is_empty(),
        _ => true,
    }
}

/// Build a `String` from a raw UTF-8 pointer + length (copy only).
unsafe fn str_from_raw(bytes: *const u8, len: i64) -> String {
    let slice = std::slice::from_raw_parts(bytes, len as usize);
    std::str::from_utf8_unchecked(slice).to_owned()
}

// ── Boxing ─────────────────────────────────────────────────────────────────────
// All functions in this section allocate a new owned FidanValue and return it.

#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_int(v: i64) -> *mut FidanValue {
    into_raw(FidanValue::Integer(v))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_float(v: f64) -> *mut FidanValue {
    into_raw(FidanValue::Float(v))
}

/// `v` is `0` (false) or non-zero (true).
#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_bool(v: i8) -> *mut FidanValue {
    into_raw(FidanValue::Boolean(v != 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_handle(v: usize) -> *mut FidanValue {
    into_raw(FidanValue::Handle(v))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_nothing() -> *mut FidanValue {
    into_raw(FidanValue::Nothing)
}

/// Copy `len` valid UTF-8 bytes starting at `bytes` into a new owned string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_box_str(bytes: *const u8, len: i64) -> *mut FidanValue {
    let slice = std::slice::from_raw_parts(bytes, len as usize);
    let s = std::str::from_utf8_unchecked(slice);
    into_raw(FidanValue::String(FidanString::new(s)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_box_fn_ref(fn_id: i64) -> *mut FidanValue {
    into_raw(FidanValue::Function(FunctionId(fn_id as u32)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_box_namespace(bytes: *const u8, len: i64) -> *mut FidanValue {
    let s = str_from_raw(bytes, len);
    into_raw(FidanValue::Namespace(Arc::from(s.as_str())))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_box_stdlib_fn(
    mod_bytes: *const u8,
    mod_len: i64,
    fn_bytes: *const u8,
    fn_len: i64,
) -> *mut FidanValue {
    let module = str_from_raw(mod_bytes, mod_len);
    let name = str_from_raw(fn_bytes, fn_len);
    into_raw(FidanValue::StdlibFn(
        Arc::from(module.as_str()),
        Arc::from(name.as_str()),
    ))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_box_enum_type(bytes: *const u8, len: i64) -> *mut FidanValue {
    let s = str_from_raw(bytes, len);
    into_raw(FidanValue::EnumType(Arc::from(s.as_str())))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_box_class_type(bytes: *const u8, len: i64) -> *mut FidanValue {
    let s = str_from_raw(bytes, len);
    into_raw(FidanValue::ClassType(Arc::from(s.as_str())))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_make_shared(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::Shared(SharedRef::new(borrow(ptr).clone())))
}

// ── Ownership ──────────────────────────────────────────────────────────────────

/// Clone: borrows `ptr`, returns a new owned copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_clone(ptr: *mut FidanValue) -> *mut FidanValue {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    into_raw(borrow(ptr).clone())
}

/// Drop: the ONLY function that consumes its argument.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_drop(ptr: *mut FidanValue) {
    if ptr.is_null() {
        return;
    }
    drop(Box::from_raw(ptr));
}

// ── Unboxing scalars ───────────────────────────────────────────────────────────
// These functions BORROW the pointer and return a scalar copy.
// The caller is still responsible for dropping the value via fdn_drop.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_unbox_int(ptr: *mut FidanValue) -> i64 {
    match borrow(ptr) {
        FidanValue::Integer(n) => *n,
        FidanValue::Float(f) => *f as i64,
        FidanValue::Boolean(b) => *b as i64,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_unbox_float(ptr: *mut FidanValue) -> f64 {
    match borrow(ptr) {
        FidanValue::Float(f) => *f,
        FidanValue::Integer(n) => *n as f64,
        _ => 0.0,
    }
}

/// Returns `0` (false) or `1` (true).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_unbox_bool(ptr: *mut FidanValue) -> i8 {
    match borrow(ptr) {
        FidanValue::Boolean(b) => *b as i8,
        FidanValue::Integer(n) => (*n != 0) as i8,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_unbox_handle(ptr: *mut FidanValue) -> usize {
    match borrow(ptr) {
        FidanValue::Handle(h) => *h,
        FidanValue::Integer(n) => (*n).max(0) as usize,
        _ => 0,
    }
}

// ── Truthiness ─────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_truthy(ptr: *mut FidanValue) -> i8 {
    is_truthy(borrow(ptr)) as i8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_is_nothing(ptr: *mut FidanValue) -> i8 {
    matches!(borrow(ptr), FidanValue::Nothing) as i8
}

/// Returns a new owned pointer: either a clone of `lhs` (if non-nothing) or a
/// clone of `rhs`.  Both inputs are borrowed; caller still owns them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_null_coalesce(
    lhs: *mut FidanValue,
    rhs: *mut FidanValue,
) -> *mut FidanValue {
    let l = borrow(lhs);
    if matches!(l, FidanValue::Nothing) {
        into_raw(borrow(rhs).clone())
    } else {
        into_raw(l.clone())
    }
}

// ── Dynamic arithmetic ─────────────────────────────────────────────────────────
// All borrow their args and return a new owned allocation.

macro_rules! numeric_binop {
    ($name:ident, $int_op:expr, $float_op:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
            let av = borrow(a);
            let bv = borrow(b);
            let result = match (av, bv) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                    FidanValue::Integer($int_op(*x, *y))
                }
                (FidanValue::Float(x), FidanValue::Float(y)) => {
                    FidanValue::Float($float_op(*x, *y))
                }
                (FidanValue::Integer(x), FidanValue::Float(y)) => {
                    FidanValue::Float($float_op(*x as f64, *y))
                }
                (FidanValue::Float(x), FidanValue::Integer(y)) => {
                    FidanValue::Float($float_op(*x, *y as f64))
                }
                _ => FidanValue::Nothing,
            };
            into_raw(result)
        }
    };
}

macro_rules! int_only_binop {
    ($name:ident, $op:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
            let av = borrow(a);
            let bv = borrow(b);
            let result = match (av, bv) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                    FidanValue::Integer($op(*x, *y))
                }
                _ => FidanValue::Nothing,
            };
            into_raw(result)
        }
    };
}

// fdn_dyn_add is written by hand rather than via macro so it can also handle
// the String + String case (string concatenation).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_add(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    let av = borrow(a);
    let bv = borrow(b);
    match (av, bv) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => {
            into_raw(FidanValue::Integer(x.wrapping_add(*y)))
        }
        (FidanValue::Float(x), FidanValue::Float(y)) => into_raw(FidanValue::Float(x + y)),
        (FidanValue::Integer(x), FidanValue::Float(y)) => {
            into_raw(FidanValue::Float(*x as f64 + y))
        }
        (FidanValue::Float(x), FidanValue::Integer(y)) => {
            into_raw(FidanValue::Float(x + *y as f64))
        }
        // String concatenation: `s1 + s2`
        (FidanValue::String(sa), FidanValue::String(sb)) => {
            into_raw(FidanValue::String(sa.append(sb)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

numeric_binop!(
    fdn_dyn_sub,
    |x: i64, y: i64| x.wrapping_sub(y),
    |x: f64, y: f64| x - y
);
numeric_binop!(
    fdn_dyn_mul,
    |x: i64, y: i64| x.wrapping_mul(y),
    |x: f64, y: f64| x * y
);
numeric_binop!(
    fdn_dyn_div,
    |x: i64, y: i64| if y == 0 { 0 } else { x / y },
    |x: f64, y: f64| x / y
);
numeric_binop!(
    fdn_dyn_rem,
    |x: i64, y: i64| if y == 0 { 0 } else { x % y },
    |x: f64, y: f64| x % y
);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_pow(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    let av = borrow(a);
    let bv = borrow(b);
    let result = match (av, bv) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => {
            if *y >= 0 {
                FidanValue::Integer(x.wrapping_pow(*y as u32))
            } else {
                FidanValue::Float((*x as f64).powi(*y as i32))
            }
        }
        (FidanValue::Float(x), FidanValue::Float(y)) => FidanValue::Float(x.powf(*y)),
        (FidanValue::Integer(x), FidanValue::Float(y)) => FidanValue::Float((*x as f64).powf(*y)),
        (FidanValue::Float(x), FidanValue::Integer(y)) => FidanValue::Float(x.powi(*y as i32)),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_concat(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    let av = borrow(a);
    let bv = borrow(b);
    match (av, bv) {
        (FidanValue::String(sa), FidanValue::String(sb)) => {
            into_raw(FidanValue::String(sa.append(sb)))
        }
        _ => into_raw(FidanValue::String(FidanString::new(&format!(
            "{}{}",
            display(av),
            display(bv)
        )))),
    }
}

// ── Dynamic comparisons ────────────────────────────────────────────────────────

macro_rules! cmp_binop {
    ($name:ident, $int_cmp:expr, $float_cmp:expr, $str_cmp:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(a: *mut FidanValue, b: *mut FidanValue) -> i8 {
            let av = borrow(a);
            let bv = borrow(b);
            let result: bool = match (av, bv) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => $int_cmp(x, y),
                (FidanValue::Float(x), FidanValue::Float(y)) => $float_cmp(x, y),
                (FidanValue::Integer(x), FidanValue::Float(y)) => $float_cmp(&(*x as f64), y),
                (FidanValue::Float(x), FidanValue::Integer(y)) => $float_cmp(x, &(*y as f64)),
                (FidanValue::String(sa), FidanValue::String(sb)) => {
                    $str_cmp(sa.as_str(), sb.as_str())
                }
                (FidanValue::Boolean(x), FidanValue::Boolean(y)) => {
                    $int_cmp(&(*x as i64), &(*y as i64))
                }
                (FidanValue::Nothing, FidanValue::Nothing) => (stringify!($name) == "fdn_dyn_eq"),
                (
                    FidanValue::EnumVariant {
                        tag: ta,
                        payload: pa,
                    },
                    FidanValue::EnumVariant {
                        tag: tb,
                        payload: pb,
                    },
                ) => {
                    let eq = ta == tb
                        && pa.len() == pb.len()
                        && pa.iter().zip(pb.iter()).all(|(a, b)| values_equal(a, b));
                    if stringify!($name) == "fdn_dyn_eq" {
                        eq
                    } else if stringify!($name) == "fdn_dyn_ne" {
                        !eq
                    } else {
                        false
                    }
                }
                // One operand is Nothing, the other is not: for eq→false, ne→true, ordering→false.
                (FidanValue::Nothing, _) | (_, FidanValue::Nothing) => {
                    stringify!($name) == "fdn_dyn_ne"
                }
                _ => false,
            };
            result as i8
        }
    };
}

cmp_binop!(
    fdn_dyn_eq,
    |x: &i64, y: &i64| x == y,
    |x: &f64, y: &f64| x == y,
    |x: &str, y: &str| x == y
);
cmp_binop!(
    fdn_dyn_ne,
    |x: &i64, y: &i64| x != y,
    |x: &f64, y: &f64| x != y,
    |x: &str, y: &str| x != y
);
cmp_binop!(
    fdn_dyn_lt,
    |x: &i64, y: &i64| x < y,
    |x: &f64, y: &f64| x < y,
    |x: &str, y: &str| x < y
);
cmp_binop!(
    fdn_dyn_le,
    |x: &i64, y: &i64| x <= y,
    |x: &f64, y: &f64| x <= y,
    |x: &str, y: &str| x <= y
);
cmp_binop!(
    fdn_dyn_gt,
    |x: &i64, y: &i64| x > y,
    |x: &f64, y: &f64| x > y,
    |x: &str, y: &str| x > y
);
cmp_binop!(
    fdn_dyn_ge,
    |x: &i64, y: &i64| x >= y,
    |x: &f64, y: &f64| x >= y,
    |x: &str, y: &str| x >= y
);

// ── Dynamic logical / unary ────────────────────────────────────────────────────

/// Short-circuit `and`: returns a clone of `a` if falsy, else a clone of `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_and(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    if is_truthy(borrow(a)) {
        into_raw(borrow(b).clone())
    } else {
        into_raw(borrow(a).clone())
    }
}

/// Short-circuit `or`: returns a clone of `a` if truthy, else a clone of `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_or(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    if is_truthy(borrow(a)) {
        into_raw(borrow(a).clone())
    } else {
        into_raw(borrow(b).clone())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_not(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::Boolean(!is_truthy(borrow(ptr))))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_neg(ptr: *mut FidanValue) -> *mut FidanValue {
    let result = match borrow(ptr) {
        FidanValue::Integer(n) => FidanValue::Integer(n.wrapping_neg()),
        FidanValue::Float(f) => FidanValue::Float(-f),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

// ── Bit operations ─────────────────────────────────────────────────────────────

int_only_binop!(fdn_dyn_bit_xor, |a: i64, b: i64| a ^ b);
int_only_binop!(fdn_dyn_bit_and, |a: i64, b: i64| a & b);
int_only_binop!(fdn_dyn_bit_or, |a: i64, b: i64| a | b);
int_only_binop!(fdn_dyn_shl, |a: i64, b: i64| a.wrapping_shl(b as u32));
int_only_binop!(fdn_dyn_shr, |a: i64, b: i64| a.wrapping_shr(b as u32));

// ── Range construction ─────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn fdn_make_range(start: i64, end: i64, inclusive: i8) -> *mut FidanValue {
    into_raw(FidanValue::Range {
        start,
        end,
        inclusive: inclusive != 0,
    })
}

// ── Built-in functions ─────────────────────────────────────────────────────────

/// Print then newline.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_println(ptr: *mut FidanValue) {
    with_stdout_buffer(|stdout| {
        let _ = crate::value::write_display_io(stdout, borrow(ptr));
        let _ = stdout.write_all(b"\n");
    });
}

/// Print multiple values space-separated, then newline.
/// `ptrs` is an array of `n` `*mut FidanValue` pointers.  Borrows each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_print_many(ptrs: *const *mut FidanValue, n: i64) {
    with_stdout_buffer(|stdout| {
        for i in 0..n as usize {
            if i > 0 {
                let _ = stdout.write_all(b" ");
            }
            let _ = crate::value::write_display_io(stdout, borrow(*ptrs.add(i)));
        }
        let _ = stdout.write_all(b"\n");
    });
}

/// Print without newline.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_print(ptr: *mut FidanValue) {
    with_stdout_buffer(|stdout| {
        let _ = crate::value::write_display_io(stdout, borrow(ptr));
    });
}

/// Read a line from stdin, optionally printing a UTF-8 prompt.  Borrows `prompt`.
/// Returns a new owned `String` value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_input(prompt: *mut FidanValue) -> *mut FidanValue {
    let pv = borrow(prompt);
    flush_stdout_buffer();
    if !matches!(pv, FidanValue::Nothing) {
        with_stdout_buffer(|stdout| {
            let _ = crate::value::write_display_io(stdout, pv);
            let _ = stdout.flush();
        });
    }
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    if line.ends_with('\n') {
        line.pop();
    }
    if line.ends_with('\r') {
        line.pop();
    }
    into_raw(FidanValue::String(FidanString::new(&line)))
}

/// Return the length of a string / list / dict / range.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_len(ptr: *mut FidanValue) -> i64 {
    match borrow(ptr) {
        FidanValue::String(s) => s.len() as i64,
        FidanValue::List(l) => l.borrow().len() as i64,
        FidanValue::Dict(d) => d.borrow().len() as i64,
        FidanValue::HashSet(s) => s.borrow().len() as i64,
        FidanValue::Tuple(items) => items.len() as i64,
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            let diff = end - start;
            (if *inclusive { diff + 1 } else { diff }).max(0)
        }
        _ => 0,
    }
}

/// Panic with a message.  Borrows `ptr`.  Does not return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_panic(ptr: *mut FidanValue) -> ! {
    eprintln!("panic: {}", display(borrow(ptr)));
    std::process::exit(1);
}

/// Assert `cond != 0`; panic with `msg` on failure.  Borrows `msg`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert(cond: i64, msg: *mut FidanValue) {
    if cond == 0 {
        eprintln!("assertion failed: {}", display(borrow(msg)));
        std::process::exit(1);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_eq(lhs: *mut FidanValue, rhs: *mut FidanValue) {
    let left = borrow(lhs).clone();
    let right = borrow(rhs).clone();
    if !values_equal(&left, &right) {
        let msg = format!("assertEq failed: {} != {}", display(&left), display(&right));
        let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
        fdn_throw_unhandled(msg_val);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_ne(lhs: *mut FidanValue, rhs: *mut FidanValue) {
    let left = borrow(lhs).clone();
    let right = borrow(rhs).clone();
    if values_equal(&left, &right) {
        let msg = format!("assertNe failed: both are {}", display(&left));
        let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
        fdn_throw_unhandled(msg_val);
    }
}

/// Return the runtime type name as a new owned `String`.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_type_name(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::String(FidanString::new(
        borrow(ptr).type_name(),
    )))
}

/// Stringify via `display` and return a new owned `String`.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_string(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::String(FidanString::new(&display(borrow(ptr)))))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_integer(ptr: *mut FidanValue) -> *mut FidanValue {
    let result = match borrow(ptr) {
        FidanValue::Integer(n) => FidanValue::Integer(*n),
        FidanValue::Float(f) => FidanValue::Integer(*f as i64),
        FidanValue::Boolean(b) => FidanValue::Integer(if *b { 1 } else { 0 }),
        FidanValue::String(s) => s
            .as_str()
            .parse::<i64>()
            .map(FidanValue::Integer)
            .unwrap_or(FidanValue::Nothing),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_float(ptr: *mut FidanValue) -> *mut FidanValue {
    let result = match borrow(ptr) {
        FidanValue::Float(f) => FidanValue::Float(*f),
        FidanValue::Integer(n) => FidanValue::Float(*n as f64),
        FidanValue::Boolean(b) => FidanValue::Float(if *b { 1.0 } else { 0.0 }),
        FidanValue::String(s) => s
            .as_str()
            .parse::<f64>()
            .map(FidanValue::Float)
            .unwrap_or(FidanValue::Nothing),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_boolean(ptr: *mut FidanValue) -> *mut FidanValue {
    let result = match borrow(ptr) {
        FidanValue::Boolean(b) => FidanValue::Boolean(*b),
        FidanValue::Integer(n) => FidanValue::Boolean(*n != 0),
        FidanValue::Float(f) => FidanValue::Boolean(*f != 0.0),
        FidanValue::String(s) => FidanValue::Boolean(!s.as_str().is_empty()),
        FidanValue::Nothing => FidanValue::Boolean(false),
        _ => FidanValue::Boolean(true),
    };
    into_raw(result)
}

/// Check `certain` parameter invariant — `val` must not be `nothing`.  Borrows `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_certain_check(
    val: *mut FidanValue,
    name_bytes: *const u8,
    name_len: i64,
) {
    if matches!(borrow(val), FidanValue::Nothing) {
        let name = str_from_raw(name_bytes, name_len);
        eprintln!(
            "type error: parameter `{}` is `certain` but received `nothing`",
            name
        );
        std::process::exit(1);
    }
}

// ── Slice ──────────────────────────────────────────────────────────────────────

/// Slice `obj[start..end step step]`.
/// `start`, `end`, `step` may be `Nothing` (meaning "use default").
/// `inclusive` is 1 if the upper bound is inclusive (`...`), 0 otherwise.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_slice(
    obj: *mut FidanValue,
    start: *mut FidanValue,
    end: *mut FidanValue,
    inclusive: i8,
    step: *mut FidanValue,
) -> *mut FidanValue {
    let to_opt_i64 = |ptr: *mut FidanValue| -> Option<i64> {
        match borrow(ptr) {
            FidanValue::Integer(n) => Some(*n),
            _ => None,
        }
    };
    let step_i = to_opt_i64(step).unwrap_or(1);
    if step_i == 0 {
        eprintln!("panic: slice step cannot be zero");
        std::process::exit(1);
    }
    let start_raw = to_opt_i64(start);
    let end_raw = to_opt_i64(end);
    let inc = inclusive != 0;

    match borrow(obj).clone() {
        FidanValue::List(r) => {
            let list = r.borrow();
            let len = list.len() as i64;
            let norm = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
            let si = start_raw
                .map(norm)
                .unwrap_or(if step_i > 0 { 0 } else { len - 1 });
            let ei = end_raw
                .map(|e| {
                    let n = norm(e);
                    if inc { n + 1 } else { n }
                })
                .unwrap_or(if step_i > 0 { len } else { -1 });
            let mut out = FidanList::new();
            let mut idx = si;
            while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                if let Some(v) = list.get(idx as usize) {
                    out.append(v.clone());
                }
                idx += step_i;
            }
            into_raw(FidanValue::List(OwnedRef::new(out)))
        }
        FidanValue::String(s) => {
            let str_ref = s.as_str().to_owned();
            let len = str_ref.chars().count() as i64;
            let norm = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
            let si = start_raw
                .map(norm)
                .unwrap_or(if step_i > 0 { 0 } else { len - 1 });
            let ei = end_raw
                .map(|e| {
                    let n = norm(e);
                    if inc { n + 1 } else { n }
                })
                .unwrap_or(if step_i > 0 { len } else { -1 });
            if step_i == 1 && si >= 0 && ei >= si {
                let out: String = str_ref
                    .chars()
                    .skip(si as usize)
                    .take((ei - si) as usize)
                    .collect();
                return into_raw(FidanValue::String(FidanString::new(&out)));
            }
            let chars: Vec<char> = str_ref.chars().collect();
            let mut out = String::new();
            let mut idx = si;
            while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                if let Some(c) = chars.get(idx as usize) {
                    out.push(*c);
                }
                idx += step_i;
            }
            into_raw(FidanValue::String(FidanString::new(&out)))
        }
        FidanValue::Range {
            start: rs,
            end: re,
            inclusive: ri,
        } => {
            let range_len = if ri {
                (re - rs + 1).max(0)
            } else {
                (re - rs).max(0)
            };
            let norm = |i: i64| {
                if i < 0 {
                    (range_len + i).max(0)
                } else {
                    i.min(range_len)
                }
            };
            let si = start_raw
                .map(norm)
                .unwrap_or(if step_i > 0 { 0 } else { range_len - 1 });
            let ei = end_raw
                .map(|e| {
                    let n = norm(e);
                    if inc { n + 1 } else { n }
                })
                .unwrap_or(if step_i > 0 { range_len } else { -1 });
            let mut out = FidanList::new();
            let mut idx = si;
            while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                if idx >= 0 && idx < range_len {
                    out.append(FidanValue::Integer(rs + idx));
                }
                idx += step_i;
            }
            into_raw(FidanValue::List(OwnedRef::new(out)))
        }
        other => {
            eprintln!("panic: cannot slice `{}`", other.type_name());
            std::process::exit(1);
        }
    }
}

// ── List ───────────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn fdn_list_new() -> *mut FidanValue {
    into_raw(FidanValue::List(OwnedRef::new(FidanList::new())))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_tuple_pack(
    values_ptr: *const *mut FidanValue,
    values_count: i64,
) -> *mut FidanValue {
    let mut values = Vec::with_capacity(values_count.max(0) as usize);
    if values_count > 0 && !values_ptr.is_null() {
        for i in 0..values_count as usize {
            values.push(borrow(*values_ptr.add(i)).clone());
        }
    }
    into_raw(FidanValue::Tuple(values))
}

/// Append a clone of `val` to `list`.  Borrows both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_push(list: *mut FidanValue, val: *mut FidanValue) {
    if let FidanValue::List(l) = borrow(list) {
        l.borrow_mut().append(borrow(val).clone());
    }
}

/// Get the element at `idx` (negative indexing supported).
/// Returns a new owned clone, or `nothing` on out-of-bounds.  Borrows `list`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_get(
    list: *mut FidanValue,
    idx: *mut FidanValue,
) -> *mut FidanValue {
    let idx_val = borrow(idx);
    let result = match borrow(list) {
        FidanValue::List(l) => {
            if let FidanValue::Integer(n) = idx_val {
                let b = l.borrow();
                let i = if *n < 0 {
                    (b.len() as i64 + n).max(0) as usize
                } else {
                    *n as usize
                };
                b.get(i).cloned().unwrap_or(FidanValue::Nothing)
            } else {
                FidanValue::Nothing
            }
        }
        FidanValue::Tuple(items) => {
            if let FidanValue::Integer(n) = idx_val {
                let len = items.len() as i64;
                let i = if *n < 0 { len + n } else { *n };
                if i < 0 {
                    FidanValue::Nothing
                } else {
                    items
                        .get(i as usize)
                        .cloned()
                        .unwrap_or(FidanValue::Nothing)
                }
            } else {
                FidanValue::Nothing
            }
        }
        FidanValue::HashSet(set) => {
            if let FidanValue::Integer(n) = idx_val {
                set.borrow()
                    .value_at_sorted_index(*n)
                    .unwrap_or(FidanValue::Nothing)
            } else {
                FidanValue::Nothing
            }
        }
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            if let FidanValue::Integer(n) = idx_val {
                let len = (if *inclusive {
                    end - start + 1
                } else {
                    end - start
                })
                .max(0);
                let i = if *n < 0 { (len + n).max(0) } else { *n };
                if i < len {
                    FidanValue::Integer(start + i)
                } else {
                    FidanValue::Nothing
                }
            } else {
                FidanValue::Nothing
            }
        }
        FidanValue::Dict(d) => d
            .borrow()
            .get(idx_val)
            .ok()
            .flatten()
            .cloned()
            .unwrap_or(FidanValue::Nothing),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

/// Set `list[idx]` to a clone of `val`.  Borrows both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_set(
    list: *mut FidanValue,
    idx: *mut FidanValue,
    val: *mut FidanValue,
) {
    let idx_val = borrow(idx);
    match borrow(list) {
        FidanValue::List(l) => {
            if let FidanValue::Integer(n) = idx_val {
                let mut b = l.borrow_mut();
                let len = b.len() as i64;
                let i = if *n < 0 {
                    (len + n).max(0) as usize
                } else {
                    *n as usize
                };
                b.set_at(i, borrow(val).clone());
            }
        }
        FidanValue::Dict(d) => {
            let _ = d.borrow_mut().insert(idx_val.clone(), borrow(val).clone());
        }
        _ => {}
    }
}

/// Return the number of elements.  Borrows `list`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_len(list: *mut FidanValue) -> i64 {
    match borrow(list) {
        FidanValue::List(l) => l.borrow().len() as i64,
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => (if *inclusive {
            end - start + 1
        } else {
            end - start
        })
        .max(0),
        _ => 0,
    }
}

/// Concatenate two lists into a new owned list.  Borrows both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_concat(
    a: *mut FidanValue,
    b: *mut FidanValue,
) -> *mut FidanValue {
    let mut result = FidanList::new();
    if let FidanValue::List(la) = borrow(a) {
        for item in la.borrow().iter() {
            result.append(item.clone());
        }
    }
    if let FidanValue::List(lb) = borrow(b) {
        for item in lb.borrow().iter() {
            result.append(item.clone());
        }
    }
    into_raw(FidanValue::List(OwnedRef::new(result)))
}

// ── Dict ───────────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn fdn_dict_new() -> *mut FidanValue {
    into_raw(FidanValue::Dict(OwnedRef::new(FidanDict::new())))
}

/// Get the value associated with `key`.  Returns a new owned clone or `nothing`.
/// Borrows both `dict` and `key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_get(
    dict: *mut FidanValue,
    key: *mut FidanValue,
) -> *mut FidanValue {
    let result = if let FidanValue::Dict(d) = borrow(dict) {
        d.borrow()
            .get(borrow(key))
            .ok()
            .flatten()
            .cloned()
            .unwrap_or(FidanValue::Nothing)
    } else {
        FidanValue::Nothing
    };
    into_raw(result)
}

/// Insert a clone of `val` under `key`.  Borrows `dict`, `key`, and `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_set(
    dict: *mut FidanValue,
    key: *mut FidanValue,
    val: *mut FidanValue,
) {
    if let FidanValue::Dict(d) = borrow(dict) {
        let _ = d
            .borrow_mut()
            .insert(borrow(key).clone(), borrow(val).clone());
    }
}

/// Return the number of entries.  Borrows `dict`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_len(dict: *mut FidanValue) -> i64 {
    match borrow(dict) {
        FidanValue::Dict(d) => d.borrow().len() as i64,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_contains_key(dict: *mut FidanValue, key: *mut FidanValue) -> i8 {
    match borrow(dict) {
        FidanValue::Dict(d) => i8::from(d.borrow().get(borrow(key)).ok().flatten().is_some()),
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_remove(dict: *mut FidanValue, key: *mut FidanValue) {
    if let FidanValue::Dict(d) = borrow(dict) {
        let _ = d.borrow_mut().remove(borrow(key));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_keys(dict: *mut FidanValue) -> *mut FidanValue {
    match borrow(dict) {
        FidanValue::Dict(d) => {
            let mut list = FidanList::new();
            for (key, _) in d.borrow().iter() {
                list.append(key.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_values(dict: *mut FidanValue) -> *mut FidanValue {
    match borrow(dict) {
        FidanValue::Dict(d) => {
            let mut list = FidanList::new();
            for (_, value) in d.borrow().iter() {
                list.append(value.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_entries(dict: *mut FidanValue) -> *mut FidanValue {
    match borrow(dict) {
        FidanValue::Dict(d) => {
            let mut list = FidanList::new();
            for (key, value) in d.borrow().iter() {
                let mut pair = FidanList::new();
                pair.append(key.clone());
                pair.append(value.clone());
                list.append(FidanValue::List(OwnedRef::new(pair)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

// ── Object ─────────────────────────────────────────────────────────────────────
// Phase 11.1: objects are backed by FidanDict (string-keyed).

/// Allocate a new empty object.  `class_bytes`/`class_len` are the class name
/// (used for display / future class-table lookup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_obj_new(class_bytes: *const u8, class_len: i64) -> *mut FidanValue {
    let mut d = FidanDict::new();
    if !class_bytes.is_null() && class_len > 0 {
        let class_name = str_from_raw(class_bytes, class_len);
        let _ = d.insert(
            FidanValue::String(FidanString::new("__class__")),
            FidanValue::String(FidanString::new(&class_name)),
        );
    }
    into_raw(FidanValue::Dict(OwnedRef::new(d)))
}

/// Get a field value.  Returns a new owned clone or `nothing`.  Borrows `obj`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_obj_get_field(
    obj: *mut FidanValue,
    field_bytes: *const u8,
    field_len: i64,
) -> *mut FidanValue {
    let field_name = str_from_raw(field_bytes, field_len);
    let result = match borrow(obj) {
        FidanValue::Dict(d) => d
            .borrow()
            .get(&FidanValue::String(FidanString::new(&field_name)))
            .ok()
            .flatten()
            .cloned()
            .unwrap_or(FidanValue::Nothing),
        FidanValue::EnumVariant { tag, .. } if field_name == "tag" => {
            FidanValue::String(FidanString::new(tag))
        }
        FidanValue::EnumType(_) => {
            // `Direction.North` — return a unit variant with the field name as tag.
            FidanValue::EnumVariant {
                tag: Arc::from(field_name.as_str()),
                payload: vec![],
            }
        }
        FidanValue::Namespace(_) => {
            // `bundle.math` — field access on a namespace returns a sub-namespace.
            // This supports re-export chaining: `bundle.math.sqrt(x)`.
            FidanValue::Namespace(Arc::from(field_name.as_str()))
        }
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

/// Set a field value.  Stores a clone of `val`.  Borrows `obj` and `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_obj_set_field(
    obj: *mut FidanValue,
    field_bytes: *const u8,
    field_len: i64,
    val: *mut FidanValue,
) {
    let field_name = str_from_raw(field_bytes, field_len);
    if let FidanValue::Dict(d) = borrow(obj) {
        let _ = d.borrow_mut().insert(
            FidanValue::String(FidanString::new(&field_name)),
            borrow(val).clone(),
        );
    }
}

/// Invoke a method on an object.  Borrows `obj`; args are each borrowed once.
/// Dispatches string, list, dict, and range receiver methods directly.
/// Falls back to dict-field lookup for user-defined objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_obj_invoke(
    obj: *mut FidanValue,
    method_bytes: *const u8,
    method_len: i64,
    args_ptr: *const *mut FidanValue,
    args_count: i64,
) -> *mut FidanValue {
    let method_name = str_from_raw(method_bytes, method_len);
    let recv = borrow(obj);

    // Load extra args (not including the receiver)
    let extra: Vec<FidanValue> = (0..args_count as usize)
        .map(|i| borrow(*args_ptr.add(i)).clone())
        .collect();

    match recv {
        FidanValue::Namespace(ns_name) => {
            // Try stdlib first. dispatch_stdlib_inline returns None for unrecognised
            // module names, so adding a new stdlib module only requires updating that
            // one function — no separate list here to keep in sync.
            if let Some(v) = dispatch_stdlib_inline(ns_name.as_ref(), &method_name, extra.clone()) {
                return v;
            }
            // User namespace: look up method_name in the name table and call through FN_TABLE.
            if let Some(name_table) = FN_NAME_TABLE.get()
                && let Ok(guard) = name_table.lock()
                && let Some(&idx) = guard.get(method_name.as_str())
            {
                drop(guard);
                // Build arg pointers
                let mut arg_ptrs: Vec<*mut FidanValue> =
                    extra.iter().map(|v| into_raw(v.clone())).collect();
                let result = call_trampoline_by_idx(idx, &arg_ptrs);
                for p in arg_ptrs.drain(..) {
                    drop(Box::from_raw(p));
                }
                return result;
            }
            eprintln!(
                "AOT: no function `{}` in user namespace `{}`",
                method_name, ns_name
            );
            into_raw(FidanValue::Nothing)
        }
        FidanValue::String(s) => dispatch_string_method(s.clone(), &method_name, extra),
        FidanValue::List(l) => dispatch_list_method(l, &method_name, extra),
        FidanValue::Dict(d) => {
            // Check for a user-defined method stored as `__method__<name>` in the dict.
            let method_key =
                FidanValue::String(FidanString::new(&format!("__method__{}", method_name)));
            if let Ok(Some(method_fn)) = d.borrow().get(&method_key).map(|value| value.cloned()) {
                // Build call-arg list: self (obj ptr, borrowed) + original arg ptrs.
                let mut call_ptrs: Vec<*mut FidanValue> =
                    Vec::with_capacity(1 + args_count as usize);
                call_ptrs.push(obj); // self
                for i in 0..args_count as usize {
                    call_ptrs.push(*args_ptr.add(i));
                }
                let method_fn_ptr = into_raw(method_fn);
                let result =
                    fdn_call_dynamic(method_fn_ptr, call_ptrs.as_ptr(), call_ptrs.len() as i64);
                drop(Box::from_raw(method_fn_ptr));
                return result;
            }
            dispatch_dict_method(d, &method_name, extra)
        }
        FidanValue::HashSet(s) => dispatch_hashset_method(s, &method_name, extra),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => dispatch_range_method(*start, *end, *inclusive, &method_name, extra),
        FidanValue::Shared(sr) => {
            match infer_receiver_member(ReceiverBuiltinKind::Shared, method_name.as_str())
                .map(|info| info.canonical_name)
            {
                Some("get") => into_raw(sr.0.lock().unwrap().clone()),
                Some("set") => {
                    let val = extra.into_iter().next().unwrap_or(FidanValue::Nothing);
                    *sr.0.lock().unwrap() = val;
                    into_raw(FidanValue::Nothing)
                }
                Some("weak") => into_raw(FidanValue::WeakShared(sr.downgrade())),
                _ => panic_missing_method(recv, &method_name),
            }
        }
        FidanValue::WeakShared(ws) => {
            match infer_receiver_member(ReceiverBuiltinKind::WeakShared, method_name.as_str())
                .map(|info| info.canonical_name)
            {
                Some("upgrade") => into_raw(
                    ws.upgrade()
                        .map(FidanValue::Shared)
                        .unwrap_or(FidanValue::Nothing),
                ),
                Some("isAlive") => into_raw(FidanValue::Boolean(ws.is_alive())),
                _ => panic_missing_method(recv, &method_name),
            }
        }
        _ => panic_missing_method(recv, &method_name),
    }
}

// ── String method dispatch ─────────────────────────────────────────────────────

fn as_str_val(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_owned(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => {
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                f.to_string()
            }
        }
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => "nothing".to_owned(),
        other => display(other),
    }
}

fn async_wait_any_result(index: i64, value: FidanValue) -> FidanValue {
    stdlib::async_std::wait_any_result(index, value)
}

fn async_timeout_result(completed: bool, value: FidanValue) -> FidanValue {
    stdlib::async_std::timeout_result(completed, value)
}

fn resolve_async_value_owned(value: FidanValue) -> Result<FidanValue, String> {
    match value {
        FidanValue::Pending(pending) => pending.try_join(),
        other => Ok(other),
    }
}

fn try_take_async_value_ready(value: &FidanValue) -> Option<Result<FidanValue, String>> {
    match value {
        FidanValue::Pending(pending) => pending.try_take_ready(),
        other => Some(Ok(other.clone())),
    }
}

fn dispatch_string_method(s: FidanString, method: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    let str_val = s.as_str().to_owned();
    let Some(method) =
        infer_receiver_member(ReceiverBuiltinKind::String, method).map(|info| info.canonical_name)
    else {
        eprintln!("AOT: string method not found: .{}()", method);
        return into_raw(FidanValue::Nothing);
    };
    match method {
        // ── Case ──────────────────────────────────────────────────────────────
        "lower" => into_raw(FidanValue::String(FidanString::new(
            &str_val.to_lowercase(),
        ))),
        "upper" => into_raw(FidanValue::String(FidanString::new(
            &str_val.to_uppercase(),
        ))),
        "capitalize" => {
            let mut chars = str_val.chars();
            let capitalized = match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            };
            into_raw(FidanValue::String(FidanString::new(&capitalized)))
        }

        // ── Trim ──────────────────────────────────────────────────────────────
        "trim" => into_raw(FidanValue::String(FidanString::new(str_val.trim()))),
        "trimStart" => into_raw(FidanValue::String(FidanString::new(str_val.trim_start()))),
        "trimEnd" => into_raw(FidanValue::String(FidanString::new(str_val.trim_end()))),

        // ── Length ────────────────────────────────────────────────────────────
        "len" => into_raw(FidanValue::Integer(str_val.chars().count() as i64)),
        "byteLen" => into_raw(FidanValue::Integer(str_val.len() as i64)),
        "isEmpty" => into_raw(FidanValue::Boolean(str_val.is_empty())),

        // ── Split / lines ─────────────────────────────────────────────────────
        "split" => {
            let sep = args
                .first()
                .map(as_str_val)
                .unwrap_or_else(|| " ".to_owned());
            let mut list = FidanList::new();
            for part in str_val.split(sep.as_str()) {
                list.append(FidanValue::String(FidanString::new(part)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "lines" => {
            let mut list = FidanList::new();
            for line in str_val.lines() {
                list.append(FidanValue::String(FidanString::new(line)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "chars" => {
            let mut list = FidanList::new();
            for c in str_val.chars() {
                list.append(FidanValue::String(FidanString::new(&c.to_string())));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }

        // ── Join (receiver is separator) ──────────────────────────────────────
        "join" => {
            let collection = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            match collection {
                FidanValue::List(l) => {
                    let items: Vec<String> = l.borrow().iter().map(as_str_val).collect();
                    into_raw(FidanValue::String(FidanString::new(&items.join(&str_val))))
                }
                _ => into_raw(FidanValue::String(FidanString::new(""))),
            }
        }

        // ── Search ────────────────────────────────────────────────────────────
        "contains" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::Boolean(str_val.contains(pat.as_str())))
        }
        "startsWith" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::Boolean(str_val.starts_with(pat.as_str())))
        }
        "endsWith" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::Boolean(str_val.ends_with(pat.as_str())))
        }
        "indexOf" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            match str_val.find(pat.as_str()) {
                Some(i) => into_raw(FidanValue::Integer(i as i64)),
                None => into_raw(FidanValue::Integer(-1)),
            }
        }
        "lastIndexOf" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            match str_val.rfind(pat.as_str()) {
                Some(i) => into_raw(FidanValue::Integer(i as i64)),
                None => into_raw(FidanValue::Integer(-1)),
            }
        }

        // ── Mutation / transformation ─────────────────────────────────────────
        "replace" => {
            let from = args.first().map(as_str_val).unwrap_or_default();
            let to = args.get(1).map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::String(FidanString::new(
                &str_val.replace(from.as_str(), to.as_str()),
            )))
        }
        "replaceAll" => {
            let from = args.first().map(as_str_val).unwrap_or_default();
            let to = args.get(1).map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::String(FidanString::new(
                &str_val.replace(from.as_str(), to.as_str()),
            )))
        }
        "replaceFirst" => {
            let from = args.first().map(as_str_val).unwrap_or_default();
            let to = args.get(1).map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::String(FidanString::new(&str_val.replacen(
                from.as_str(),
                to.as_str(),
                1,
            ))))
        }
        "repeat" => {
            let n = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0)
                .max(0) as usize;
            into_raw(FidanValue::String(FidanString::new(&str_val.repeat(n))))
        }
        "reverse" => {
            let rev: String = str_val.chars().rev().collect();
            into_raw(FidanValue::String(FidanString::new(&rev)))
        }

        // ── Indexing / slicing ────────────────────────────────────────────────
        "charAt" => {
            let idx = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            match str_val.chars().nth(idx.max(0) as usize) {
                Some(c) => into_raw(FidanValue::String(FidanString::new(&c.to_string()))),
                None => into_raw(FidanValue::String(FidanString::new(""))),
            }
        }
        "substring" => {
            let start = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0)
                .max(0) as usize;
            let chars: Vec<char> = str_val.chars().collect();
            let end = args
                .get(1)
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .map(|e| (e.max(0) as usize).min(chars.len()))
                .unwrap_or(chars.len());
            let sliced: String = chars[start.min(chars.len())..end].iter().collect();
            into_raw(FidanValue::String(FidanString::new(&sliced)))
        }

        // ── Parsing ───────────────────────────────────────────────────────────
        "toInt" => match str_val.trim().parse::<i64>() {
            Ok(n) => into_raw(FidanValue::Integer(n)),
            Err(_) => into_raw(FidanValue::Nothing),
        },
        "toFloat" => match str_val.trim().parse::<f64>() {
            Ok(f) => into_raw(FidanValue::Float(f)),
            Err(_) => into_raw(FidanValue::Nothing),
        },
        "toBool" => {
            let b = matches!(str_val.trim().to_lowercase().as_str(), "true" | "1" | "yes");
            into_raw(FidanValue::Boolean(b))
        }
        "toString" => into_raw(FidanValue::String(FidanString::new(&str_val))),

        // ── Padding ───────────────────────────────────────────────────────────
        "padStart" => {
            let total = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n as usize)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let pad_char = args
                .get(1)
                .map(as_str_val)
                .unwrap_or_else(|| " ".to_owned());
            let pad_ch = pad_char.chars().next().unwrap_or(' ');
            let len = str_val.chars().count();
            let padded = if total > len {
                let padding: String = std::iter::repeat_n(pad_ch, total - len).collect();
                padding + &str_val
            } else {
                str_val
            };
            into_raw(FidanValue::String(FidanString::new(&padded)))
        }
        "padEnd" => {
            let total = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n as usize)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let pad_char = args
                .get(1)
                .map(as_str_val)
                .unwrap_or_else(|| " ".to_owned());
            let pad_ch = pad_char.chars().next().unwrap_or(' ');
            let len = str_val.chars().count();
            let padded = if total > len {
                let padding: String = std::iter::repeat_n(pad_ch, total - len).collect();
                str_val + &padding
            } else {
                str_val
            };
            into_raw(FidanValue::String(FidanString::new(&padded)))
        }

        // ── Bytes / char codes ────────────────────────────────────────────────
        "bytes" => {
            let mut list = FidanList::new();
            for b in str_val.bytes() {
                list.append(FidanValue::Integer(b as i64));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "charCode" => {
            let code = str_val.chars().next().map(|c| c as i64).unwrap_or(0);
            into_raw(FidanValue::Integer(code))
        }
        // ── format (string.format with {} placeholders) ────────────────────
        "format" => {
            let mut result = str_val.clone();
            for arg in &args {
                if let Some(pos) = result.find("{}") {
                    result.replace_range(pos..pos + 2, &as_str_val(arg));
                }
            }
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

// ── List method dispatch ───────────────────────────────────────────────────────

fn dispatch_list_method(
    list: &OwnedRef<FidanList>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    let Some(method) =
        infer_receiver_member(ReceiverBuiltinKind::List, method).map(|info| info.canonical_name)
    else {
        eprintln!("AOT: list method not found: .{}()", method);
        return into_raw(FidanValue::Nothing);
    };
    match method {
        "len" => into_raw(FidanValue::Integer(list.borrow().len() as i64)),
        "isEmpty" => into_raw(FidanValue::Boolean(list.borrow().is_empty())),
        "append" => {
            let val = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            list.borrow_mut().append(val);
            into_raw(FidanValue::Nothing)
        }
        "pop" => {
            let mut b = list.borrow_mut();
            let len = b.len();
            if len == 0 {
                into_raw(FidanValue::Nothing)
            } else {
                // Clone last element then truncate
                let last = b.get(len - 1).cloned().unwrap_or(FidanValue::Nothing);
                // Rebuild without last element
                let mut new_list = FidanList::new();
                for i in 0..len - 1 {
                    new_list.append(b.get(i).cloned().unwrap_or(FidanValue::Nothing));
                }
                *b = new_list;
                into_raw(last)
            }
        }
        "first" => into_raw(list.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing)),
        "last" => {
            let b = list.borrow();
            let len = b.len();
            into_raw(
                b.get(len.saturating_sub(1))
                    .cloned()
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "get" => {
            let idx = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let b = list.borrow();
            let i = if idx < 0 {
                (b.len() as i64 + idx).max(0) as usize
            } else {
                idx as usize
            };
            into_raw(b.get(i).cloned().unwrap_or(FidanValue::Nothing))
        }
        "contains" => {
            let target = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = list.borrow();
            let found = b.iter().any(|v| values_equal(v, &target));
            into_raw(FidanValue::Boolean(found))
        }
        "indexOf" => {
            let target = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = list.borrow();
            let idx = b
                .iter()
                .position(|v| values_equal(v, &target))
                .map(|i| i as i64)
                .unwrap_or(-1);
            into_raw(FidanValue::Integer(idx))
        }
        "reverse" => {
            let mut b = list.borrow_mut();
            let mut items: Vec<FidanValue> = b.iter().cloned().collect();
            items.reverse();
            let mut new_list = FidanList::new();
            for v in items {
                new_list.append(v);
            }
            *b = new_list;
            into_raw(FidanValue::Nothing)
        }
        "reversed" => {
            let b = list.borrow();
            let mut items: Vec<FidanValue> = b.iter().cloned().collect();
            items.reverse();
            let mut new_list = FidanList::new();
            for v in items {
                new_list.append(v);
            }
            into_raw(FidanValue::List(OwnedRef::new(new_list)))
        }
        "sort" => {
            let mut b = list.borrow_mut();
            let mut items: Vec<FidanValue> = b.iter().cloned().collect();
            items.sort_by(compare_values);
            let mut new_list = FidanList::new();
            for v in items {
                new_list.append(v);
            }
            *b = new_list;
            into_raw(FidanValue::Nothing)
        }
        "join" => {
            let sep = args.first().map(as_str_val).unwrap_or_default();
            let b = list.borrow();
            let items: Vec<String> = b.iter().map(as_str_val).collect();
            into_raw(FidanValue::String(FidanString::new(&items.join(&sep))))
        }
        "slice" => {
            let start = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0)
                .max(0) as usize;
            let b = list.borrow();
            let end = args
                .get(1)
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .map(|e| (e.max(0) as usize).min(b.len()))
                .unwrap_or(b.len());
            let mut new_list = FidanList::new();
            for i in start.min(b.len())..end {
                if let Some(v) = b.get(i) {
                    new_list.append(v.clone());
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(new_list)))
        }
        "flatten" => {
            let b = list.borrow();
            let mut new_list = FidanList::new();
            for v in b.iter() {
                match v {
                    FidanValue::List(inner) => {
                        for item in inner.borrow().iter() {
                            new_list.append(item.clone());
                        }
                    }
                    other => new_list.append(other.clone()),
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(new_list)))
        }
        "extend" => {
            let other = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            match other {
                FidanValue::List(other_l) => {
                    let mut b = list.borrow_mut();
                    for v in other_l.borrow().iter() {
                        b.append(v.clone());
                    }
                    into_raw(FidanValue::Nothing)
                }
                _ => into_raw(FidanValue::Nothing),
            }
        }
        "toString" => {
            let b = list.borrow();
            let items: Vec<String> = b.iter().map(display).collect();
            into_raw(FidanValue::String(FidanString::new(&format!(
                "[{}]",
                items.join(", ")
            ))))
        }
        "forEach" => {
            // forEach(callback) — calls callback(item) for every element.
            if let Some(callback) = args.into_iter().next() {
                let cb_ptr = into_raw(callback);
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                for item in items {
                    let item_ptr = into_raw(item);
                    let call_args = [item_ptr];
                    unsafe {
                        let result = fdn_call_dynamic(cb_ptr, call_args.as_ptr(), 1);
                        if !result.is_null() {
                            drop(Box::from_raw(result));
                        }
                        drop(Box::from_raw(item_ptr));
                    }
                }
                unsafe { drop(Box::from_raw(cb_ptr)) };
            }
            into_raw(FidanValue::Nothing)
        }

        "map" => {
            // map(fn) — returns a new list with fn applied to each element.
            if let Some(callback) = args.into_iter().next() {
                let cb_ptr = into_raw(callback);
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let mut new_list = FidanList::new();
                for item in items {
                    let item_ptr = into_raw(item);
                    let call_args = [item_ptr];
                    let result_ptr = unsafe { fdn_call_dynamic(cb_ptr, call_args.as_ptr(), 1) };
                    let mapped = if result_ptr.is_null() {
                        FidanValue::Nothing
                    } else {
                        unsafe {
                            let v = borrow(result_ptr).clone();
                            drop(Box::from_raw(result_ptr));
                            v
                        }
                    };
                    new_list.append(mapped);
                    unsafe { drop(Box::from_raw(item_ptr)) };
                }
                unsafe { drop(Box::from_raw(cb_ptr)) };
                into_raw(FidanValue::List(OwnedRef::new(new_list)))
            } else {
                into_raw(FidanValue::List(OwnedRef::new(FidanList::new())))
            }
        }

        "filter" => {
            // filter(predicate) — returns elements for which predicate is truthy.
            if let Some(predicate) = args.into_iter().next() {
                let pred_ptr = into_raw(predicate);
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let mut new_list = FidanList::new();
                for item in items {
                    let item_ptr = into_raw(item.clone());
                    let call_args = [item_ptr];
                    let result_ptr = unsafe { fdn_call_dynamic(pred_ptr, call_args.as_ptr(), 1) };
                    let keep = if result_ptr.is_null() {
                        false
                    } else {
                        unsafe {
                            let v = is_truthy(borrow(result_ptr));
                            drop(Box::from_raw(result_ptr));
                            v
                        }
                    };
                    unsafe { drop(Box::from_raw(item_ptr)) };
                    if keep {
                        new_list.append(item);
                    }
                }
                unsafe { drop(Box::from_raw(pred_ptr)) };
                into_raw(FidanValue::List(OwnedRef::new(new_list)))
            } else {
                into_raw(FidanValue::List(OwnedRef::new(FidanList::new())))
            }
        }

        "find" => {
            // find(value) — returns the index of the first element equal to value.
            let target = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = list.borrow();
            let idx = b
                .iter()
                .position(|v| values_equal(v, &target))
                .map(|i| i as i64)
                .unwrap_or(-1);
            if idx >= 0 {
                into_raw(FidanValue::Integer(idx))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }

        "firstWhere" => {
            // firstWhere(predicate) — returns the first element for which predicate is truthy.
            let result = if let Some(predicate) = args.into_iter().next() {
                let pred_ptr = into_raw(predicate);
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let mut found = None;
                for item in items {
                    let item_ptr = into_raw(item.clone());
                    let call_args = [item_ptr];
                    let result_ptr = unsafe { fdn_call_dynamic(pred_ptr, call_args.as_ptr(), 1) };
                    let matched = if result_ptr.is_null() {
                        false
                    } else {
                        unsafe {
                            let v = is_truthy(borrow(result_ptr));
                            drop(Box::from_raw(result_ptr));
                            v
                        }
                    };
                    unsafe { drop(Box::from_raw(item_ptr)) };
                    if matched {
                        found = Some(item);
                        break;
                    }
                }
                unsafe { drop(Box::from_raw(pred_ptr)) };
                found.unwrap_or(FidanValue::Nothing)
            } else {
                FidanValue::Nothing
            };
            into_raw(result)
        }

        "remove" => {
            // remove(index) — removes the element at the given index, returns it.
            let idx_val = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let mut b = list.borrow_mut();
            let len = b.len() as i64;
            let i = if idx_val < 0 {
                (len + idx_val).max(0) as usize
            } else {
                idx_val as usize
            };
            if i < b.len() {
                let removed = b.get(i).cloned().unwrap_or(FidanValue::Nothing);
                let mut new_list = FidanList::new();
                for j in 0..b.len() {
                    if j != i {
                        new_list.append(b.get(j).cloned().unwrap_or(FidanValue::Nothing));
                    }
                }
                *b = new_list;
                into_raw(removed)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }

        "reduce" => {
            // reduce(fn) or reduce(fn, initial) — fold left over the list.
            let mut iter = args.into_iter();
            let callback_val = iter.next();
            let initial = iter.next();
            if let Some(callback) = callback_val {
                let cb_ptr = into_raw(callback);
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let has_initial = initial.is_some();
                let mut acc = initial
                    .unwrap_or_else(|| items.first().cloned().unwrap_or(FidanValue::Nothing));
                let start = if !has_initial { 1 } else { 0 };
                for item in items.into_iter().skip(start) {
                    let acc_ptr = into_raw(acc);
                    let item_ptr = into_raw(item);
                    let call_args = [acc_ptr, item_ptr];
                    let result_ptr = unsafe { fdn_call_dynamic(cb_ptr, call_args.as_ptr(), 2) };
                    let new_acc = if result_ptr.is_null() {
                        FidanValue::Nothing
                    } else {
                        unsafe {
                            let v = borrow(result_ptr).clone();
                            drop(Box::from_raw(result_ptr));
                            v
                        }
                    };
                    unsafe {
                        drop(Box::from_raw(acc_ptr));
                        drop(Box::from_raw(item_ptr));
                    }
                    acc = new_acc;
                }
                unsafe { drop(Box::from_raw(cb_ptr)) };
                into_raw(acc)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }

        _ => into_raw(FidanValue::Nothing),
    }
}

// ── Dict method dispatch ───────────────────────────────────────────────────────

fn dispatch_dict_method(
    dict: &OwnedRef<FidanDict>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    let Some(operation) =
        infer_receiver_member(ReceiverBuiltinKind::Dict, method).and_then(|info| info.operation)
    else {
        eprintln!("AOT: dict method not found: .{}()", method);
        return into_raw(FidanValue::Nothing);
    };
    match operation {
        ReceiverMethodOp::Len => into_raw(FidanValue::Integer(dict.borrow().len() as i64)),
        ReceiverMethodOp::IsEmpty => into_raw(FidanValue::Boolean(dict.borrow().is_empty())),
        ReceiverMethodOp::Get => {
            if let Some(key_val) = args.first() {
                into_raw(
                    dict.borrow()
                        .get(key_val)
                        .ok()
                        .flatten()
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Set => {
            if let (Some(k), Some(v)) = (args.first(), args.get(1)) {
                let _ = dict.borrow_mut().insert(k.clone(), v.clone());
            }
            into_raw(FidanValue::Nothing)
        }
        ReceiverMethodOp::Contains => {
            if let Some(key_val) = args.first() {
                into_raw(FidanValue::Boolean(
                    dict.borrow().get(key_val).ok().flatten().is_some(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        ReceiverMethodOp::Remove => {
            if let Some(key_val) = args.first() {
                let _ = dict.borrow_mut().remove(key_val);
            }
            into_raw(FidanValue::Nothing)
        }
        ReceiverMethodOp::Keys => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (key, _) in b.iter() {
                list.append(key.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Values => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (_, val) in b.iter() {
                list.append(val.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Entries => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (k, v) in b.iter() {
                let mut pair = FidanList::new();
                pair.append(k.clone());
                pair.append(v.clone());
                list.append(FidanValue::List(OwnedRef::new(pair)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::ToString => into_raw(FidanValue::String(FidanString::new(&display(
            &FidanValue::Dict(dict.clone()),
        )))),
        _ => into_raw(FidanValue::Nothing),
    }
}

fn dispatch_hashset_method(
    set: &OwnedRef<FidanHashSet>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    let Some(operation) =
        infer_receiver_member(ReceiverBuiltinKind::HashSet, method).and_then(|info| info.operation)
    else {
        eprintln!("AOT: hashset method not found: .{}()", method);
        return into_raw(FidanValue::Nothing);
    };

    match operation {
        ReceiverMethodOp::Len => into_raw(FidanValue::Integer(set.borrow().len() as i64)),
        ReceiverMethodOp::IsEmpty => into_raw(FidanValue::Boolean(set.borrow().is_empty())),
        ReceiverMethodOp::Insert => {
            if let Some(value) = args.first() {
                let _ = set.borrow_mut().insert(value.clone());
            }
            into_raw(FidanValue::Nothing)
        }
        ReceiverMethodOp::Remove => {
            if let Some(value) = args.first() {
                let _ = set.borrow_mut().remove(value);
            }
            into_raw(FidanValue::Nothing)
        }
        ReceiverMethodOp::Contains => {
            if let Some(value) = args.first() {
                into_raw(FidanValue::Boolean(
                    set.borrow().contains(value).unwrap_or(false),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        ReceiverMethodOp::ToList => {
            let mut list = FidanList::new();
            for value in set.borrow().values_sorted() {
                list.append(value);
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Union => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                into_raw(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().union(&other.borrow()),
                )))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Intersect => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                into_raw(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().intersection(&other.borrow()),
                )))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Diff => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                into_raw(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().difference(&other.borrow()),
                )))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::ToString => into_raw(FidanValue::String(FidanString::new(&display(
            &FidanValue::HashSet(set.clone()),
        )))),
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_insert(set: *mut FidanValue, value: *mut FidanValue) {
    if let FidanValue::HashSet(inner) = borrow(set) {
        let _ = inner.borrow_mut().insert(borrow(value).clone());
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_remove(set: *mut FidanValue, value: *mut FidanValue) {
    if let FidanValue::HashSet(inner) = borrow(set) {
        let _ = inner.borrow_mut().remove(borrow(value));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_contains(set: *mut FidanValue, value: *mut FidanValue) -> i8 {
    match borrow(set) {
        FidanValue::HashSet(inner) => {
            i8::from(inner.borrow().contains(borrow(value)).unwrap_or(false))
        }
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_to_list(set: *mut FidanValue) -> *mut FidanValue {
    match borrow(set) {
        FidanValue::HashSet(inner) => {
            let mut list = FidanList::new();
            for value in inner.borrow().values_sorted() {
                list.append(value);
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_union(
    set: *mut FidanValue,
    other: *mut FidanValue,
) -> *mut FidanValue {
    match (borrow(set), borrow(other)) {
        (FidanValue::HashSet(lhs), FidanValue::HashSet(rhs)) => into_raw(FidanValue::HashSet(
            OwnedRef::new(lhs.borrow().union(&rhs.borrow())),
        )),
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_intersect(
    set: *mut FidanValue,
    other: *mut FidanValue,
) -> *mut FidanValue {
    match (borrow(set), borrow(other)) {
        (FidanValue::HashSet(lhs), FidanValue::HashSet(rhs)) => into_raw(FidanValue::HashSet(
            OwnedRef::new(lhs.borrow().intersection(&rhs.borrow())),
        )),
        _ => into_raw(FidanValue::Nothing),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_hashset_diff(
    set: *mut FidanValue,
    other: *mut FidanValue,
) -> *mut FidanValue {
    match (borrow(set), borrow(other)) {
        (FidanValue::HashSet(lhs), FidanValue::HashSet(rhs)) => into_raw(FidanValue::HashSet(
            OwnedRef::new(lhs.borrow().difference(&rhs.borrow())),
        )),
        _ => into_raw(FidanValue::Nothing),
    }
}

// ── Range method dispatch ──────────────────────────────────────────────────────

fn dispatch_range_method(
    start: i64,
    end: i64,
    inclusive: bool,
    method: &str,
    _args: Vec<FidanValue>,
) -> *mut FidanValue {
    match method {
        "len" | "length" | "size" | "count" => {
            let diff = end - start;
            let len = (if inclusive { diff + 1 } else { diff }).max(0);
            into_raw(FidanValue::Integer(len))
        }
        "toList" | "to_list" | "collect" => {
            let real_end = if inclusive { end + 1 } else { end };
            let mut list = FidanList::new();
            for i in start..real_end {
                list.append(FidanValue::Integer(i));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "contains" => {
            // covered by for-loop iteration; just return nothing for method form
            into_raw(FidanValue::Nothing)
        }
        _ => {
            eprintln!("AOT: range method not found: .{}()", method);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── Value comparison helpers ───────────────────────────────────────────────────

fn values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x == y,
        (FidanValue::Float(x), FidanValue::Float(y)) => x == y,
        (FidanValue::Integer(x), FidanValue::Float(y)) => (*x as f64) == *y,
        (FidanValue::Float(x), FidanValue::Integer(y)) => *x == (*y as f64),
        (FidanValue::Boolean(x), FidanValue::Boolean(y)) => x == y,
        (FidanValue::String(x), FidanValue::String(y)) => x.as_str() == y.as_str(),
        (FidanValue::Nothing, FidanValue::Nothing) => true,
        (FidanValue::List(lhs), FidanValue::List(rhs)) => {
            let lhs = lhs.borrow();
            let rhs = rhs.borrow();
            lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs.iter())
                    .all(|(left, right)| values_equal(left, right))
        }
        (FidanValue::Tuple(lhs), FidanValue::Tuple(rhs)) => {
            lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs.iter())
                    .all(|(left, right)| values_equal(left, right))
        }
        (FidanValue::Dict(lhs), FidanValue::Dict(rhs)) => {
            let lhs = lhs.borrow();
            let rhs = rhs.borrow();
            lhs.len() == rhs.len()
                && lhs.iter().all(|(key, left)| {
                    rhs.get(key)
                        .ok()
                        .flatten()
                        .is_some_and(|right| values_equal(left, right))
                })
        }
        (FidanValue::HashSet(lhs), FidanValue::HashSet(rhs)) => {
            let lhs = lhs.borrow();
            let rhs = rhs.borrow();
            lhs.len() == rhs.len() && lhs.iter().all(|value| rhs.contains(value).unwrap_or(false))
        }
        (FidanValue::Function(lhs), FidanValue::Function(rhs)) => lhs == rhs,
        (
            FidanValue::Closure {
                fn_id: lhs,
                captured: left,
            },
            FidanValue::Closure {
                fn_id: rhs,
                captured: right,
            },
        ) => {
            lhs == rhs
                && left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| values_equal(left, right))
        }
        (FidanValue::Namespace(lhs), FidanValue::Namespace(rhs)) => lhs == rhs,
        (FidanValue::StdlibFn(lhs_mod, lhs_name), FidanValue::StdlibFn(rhs_mod, rhs_name)) => {
            lhs_mod == rhs_mod && lhs_name == rhs_name
        }
        (FidanValue::ClassType(lhs), FidanValue::ClassType(rhs)) => lhs == rhs,
        (FidanValue::EnumType(lhs), FidanValue::EnumType(rhs)) => lhs == rhs,
        (FidanValue::Object(lhs), FidanValue::Object(rhs)) => lhs.identity() == rhs.identity(),
        (FidanValue::Shared(lhs), FidanValue::Shared(rhs)) => lhs.identity() == rhs.identity(),
        (FidanValue::WeakShared(lhs), FidanValue::WeakShared(rhs)) => {
            lhs.identity() == rhs.identity()
        }
        (FidanValue::Pending(lhs), FidanValue::Pending(rhs)) => lhs.identity() == rhs.identity(),
        (FidanValue::PendingTask(lhs), FidanValue::PendingTask(rhs)) => lhs == rhs,
        (
            FidanValue::Range {
                start: lhs_start,
                end: lhs_end,
                inclusive: lhs_inclusive,
            },
            FidanValue::Range {
                start: rhs_start,
                end: rhs_end,
                inclusive: rhs_inclusive,
            },
        ) => lhs_start == rhs_start && lhs_end == rhs_end && lhs_inclusive == rhs_inclusive,
        (
            FidanValue::EnumVariant {
                tag: ta,
                payload: pa,
            },
            FidanValue::EnumVariant {
                tag: tb,
                payload: pb,
            },
        ) => {
            ta == tb
                && pa.len() == pb.len()
                && pa.iter().zip(pb.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

fn compare_values(a: &FidanValue, b: &FidanValue) -> std::cmp::Ordering {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x.cmp(y),
        (FidanValue::Float(x), FidanValue::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (FidanValue::Integer(x), FidanValue::Float(y)) => (*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal),
        (FidanValue::Float(x), FidanValue::Integer(y)) => x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal),
        (FidanValue::String(x), FidanValue::String(y)) => x.as_str().cmp(y.as_str()),
        _ => std::cmp::Ordering::Equal,
    }
}

// ── Enum ───────────────────────────────────────────────────────────────────────

/// Construct an enum variant.  Borrows all payload pointers (clones elements).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_enum_variant(
    tag_bytes: *const u8,
    tag_len: i64,
    payload_ptr: *const *mut FidanValue,
    payload_count: i64,
) -> *mut FidanValue {
    let tag: Arc<str> = Arc::from(str_from_raw(tag_bytes, tag_len).as_str());
    let mut payload = Vec::with_capacity(payload_count as usize);
    for i in 0..payload_count as usize {
        let p = *payload_ptr.add(i);
        payload.push(borrow(p).clone());
    }
    into_raw(FidanValue::EnumVariant { tag, payload })
}

/// Check whether the variant's tag equals the expected string.  Borrows `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_enum_tag_check(
    val: *mut FidanValue,
    tag_bytes: *const u8,
    tag_len: i64,
) -> i8 {
    let expected = str_from_raw(tag_bytes, tag_len);
    match borrow(val) {
        FidanValue::EnumVariant { tag, .. } => (tag.as_ref() == expected.as_str()) as i8,
        _ => 0,
    }
}

/// Return a new owned clone of the payload element at `index`.  Borrows `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_enum_payload(val: *mut FidanValue, index: i64) -> *mut FidanValue {
    let result = match borrow(val) {
        FidanValue::EnumVariant { payload, .. } => payload
            .get(index as usize)
            .cloned()
            .unwrap_or(FidanValue::Nothing),
        _ => FidanValue::Nothing,
    };
    into_raw(result)
}

// ── Stdlib dispatch ────────────────────────────────────────────────────────────

/// Dispatch a stdlib function call.  All arg pointers are borrowed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_stdlib_call(
    mod_bytes: *const u8,
    mod_len: i64,
    fn_bytes: *const u8,
    fn_len: i64,
    args_ptr: *const *mut FidanValue,
    args_count: i64,
) -> *mut FidanValue {
    let module = str_from_raw(mod_bytes, mod_len);
    let func = str_from_raw(fn_bytes, fn_len);
    let args: Vec<FidanValue> = (0..args_count as usize)
        .map(|i| borrow(*args_ptr.add(i)).clone())
        .collect();
    dispatch_stdlib_inline(&module, &func, args).unwrap_or_else(|| {
        eprintln!("AOT stdlib: unknown module '{}'", module);
        into_raw(FidanValue::Nothing)
    })
}

/// Attempt to dispatch a stdlib call.  Returns `None` when the module name is
/// not recognised, so the caller can fall through to user-namespace lookup
/// without keeping a second copy of the module list.
/// To add a new stdlib module, add one arm here — nothing else needs changing.
fn dispatch_builtin_inline(func: &str, args: Vec<FidanValue>) -> Option<*mut FidanValue> {
    if let Some(semantic) = builtin_semantic(func) {
        return match semantic {
            BuiltinSemantic::Print => {
                let parts: Vec<String> = args.iter().map(display).collect();
                println!("{}", parts.join(" "));
                Some(into_raw(FidanValue::Nothing))
            }
            BuiltinSemantic::Eprint => {
                let parts: Vec<String> = args.iter().map(display).collect();
                eprintln!("{}", parts.join(" "));
                Some(into_raw(FidanValue::Nothing))
            }
            BuiltinSemantic::Input => {
                let prompt = args.first().map(display).unwrap_or_default();
                if !prompt.is_empty() {
                    use std::io::Write;
                    print!("{}", prompt);
                    let _ = std::io::stdout().flush();
                }
                let stdin = std::io::stdin();
                let mut line = String::new();
                stdin.lock().read_line(&mut line).ok()?;
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Some(into_raw(FidanValue::String(FidanString::new(&line))))
            }
            BuiltinSemantic::String => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::String(FidanString::new(&display(
                    &value,
                )))))
            }
            BuiltinSemantic::Integer => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                let converted = match &value {
                    FidanValue::Integer(n) => FidanValue::Integer(*n),
                    FidanValue::Float(f) => FidanValue::Integer(*f as i64),
                    FidanValue::Boolean(b) => FidanValue::Integer(if *b { 1 } else { 0 }),
                    FidanValue::String(s) => s
                        .as_str()
                        .parse::<i64>()
                        .map(FidanValue::Integer)
                        .unwrap_or(FidanValue::Nothing),
                    _ => FidanValue::Nothing,
                };
                Some(into_raw(converted))
            }
            BuiltinSemantic::Float => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                let converted = match &value {
                    FidanValue::Float(f) => FidanValue::Float(*f),
                    FidanValue::Integer(n) => FidanValue::Float(*n as f64),
                    FidanValue::String(s) => s
                        .as_str()
                        .parse::<f64>()
                        .map(FidanValue::Float)
                        .unwrap_or(FidanValue::Nothing),
                    _ => FidanValue::Nothing,
                };
                Some(into_raw(converted))
            }
            BuiltinSemantic::Boolean => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::Boolean(value.truthy())))
            }
            BuiltinSemantic::Len => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                let length = match &value {
                    FidanValue::String(s) => s.len() as i64,
                    FidanValue::List(list) => list.borrow().len() as i64,
                    FidanValue::Dict(dict) => dict.borrow().len() as i64,
                    FidanValue::HashSet(set) => set.borrow().len() as i64,
                    FidanValue::Tuple(tuple) => tuple.len() as i64,
                    FidanValue::Range {
                        start,
                        end,
                        inclusive,
                    } => {
                        if *inclusive {
                            (end - start + 1).max(0)
                        } else {
                            (end - start).max(0)
                        }
                    }
                    _ => return Some(into_raw(FidanValue::Nothing)),
                };
                Some(into_raw(FidanValue::Integer(length)))
            }
            BuiltinSemantic::Type => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::String(FidanString::new(
                    value.type_name(),
                ))))
            }
            BuiltinSemantic::HashSetConstructor => {
                let source = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                let set = match source {
                    FidanValue::Nothing => FidanHashSet::new(),
                    FidanValue::List(list) => {
                        FidanHashSet::from_values(list.borrow().iter().cloned())
                            .unwrap_or_else(|err| panic_runtime_message(err.to_string()))
                    }
                    FidanValue::HashSet(existing) => existing.borrow().clone(),
                    other => panic_runtime_message(format!(
                        "hashset(items) expects a list or hashset, got {}",
                        other.type_name()
                    )),
                };
                Some(into_raw(FidanValue::HashSet(OwnedRef::new(set))))
            }
            BuiltinSemantic::SharedConstructor => {
                let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::Shared(SharedRef::new(inner))))
            }
            BuiltinSemantic::WeakSharedConstructor => {
                let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                match inner {
                    FidanValue::Shared(shared) => {
                        Some(into_raw(FidanValue::WeakShared(shared.downgrade())))
                    }
                    FidanValue::WeakShared(weak) => Some(into_raw(FidanValue::WeakShared(weak))),
                    _ => Some(into_raw(FidanValue::Nothing)),
                }
            }
            BuiltinSemantic::Assert | BuiltinSemantic::AssertEq | BuiltinSemantic::AssertNe => {
                Some(dispatch_test(func, args))
            }
        };
    }

    match func {
        "str" => {
            let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(into_raw(FidanValue::String(FidanString::new(&display(
                &value,
            )))))
        }
        "int" => {
            let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let converted = match &value {
                FidanValue::Integer(n) => FidanValue::Integer(*n),
                FidanValue::Float(f) => FidanValue::Integer(*f as i64),
                FidanValue::Boolean(b) => FidanValue::Integer(if *b { 1 } else { 0 }),
                FidanValue::String(s) => s
                    .as_str()
                    .parse::<i64>()
                    .map(FidanValue::Integer)
                    .unwrap_or(FidanValue::Nothing),
                _ => FidanValue::Nothing,
            };
            Some(into_raw(converted))
        }
        "bool" => {
            let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(into_raw(FidanValue::Boolean(value.truthy())))
        }
        "assertEq" | "assertNe" => Some(dispatch_test(func, args)),
        _ => None,
    }
}

fn dispatch_stdlib_inline(
    module: &str,
    func: &str,
    args: Vec<FidanValue>,
) -> Option<*mut FidanValue> {
    match module {
        "__builtin__" => dispatch_builtin_inline(func, args),
        "math" => Some(dispatch_math(func, args)),
        "string" => Some(dispatch_string_fn(func, args)),
        "io" => Some(dispatch_io(func, args)),
        "json" => Some(dispatch_json(func, args)),
        "collections" => Some(dispatch_collections(func, args)),
        "async" => Some(dispatch_async(func, args)),
        "env" => Some(dispatch_env(func, args)),
        "regex" => Some(dispatch_regex(func, args)),
        "time" => Some(dispatch_time(func, args)),
        "parallel" => Some(dispatch_parallel(func, args)),
        "test" => Some(dispatch_test(func, args)),
        _ => None,
    }
}

// ── math module ───────────────────────────────────────────────────────────────

fn dispatch_math(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::math::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

// ── string module (free-function API) ─────────────────────────────────────────

fn dispatch_string_fn(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::string::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

fn runtime_error_to_exception_ptr(
    prefix: &str,
    code: fidan_diagnostics::DiagCode,
    message: String,
) -> *mut FidanValue {
    into_raw(FidanValue::String(FidanString::new(&format!(
        "{prefix} [{code}]: {message}"
    ))))
}

// ── io module ─────────────────────────────────────────────────────────────────
fn dispatch_io(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::io::dispatch_result(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(err)) => unsafe {
            let exn = runtime_error_to_exception_ptr("error", err.code, err.message);
            fdn_store_exception(exn);
            drop(Box::from_raw(exn));
            into_raw(FidanValue::Nothing)
        },
        None => into_raw(FidanValue::Nothing),
    }
}

// ── json module ───────────────────────────────────────────────────────────────
fn dispatch_json(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::json::dispatch_result(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(err)) => unsafe {
            let exn = runtime_error_to_exception_ptr("error", err.code, err.message);
            fdn_store_exception(exn);
            drop(Box::from_raw(exn));
            into_raw(FidanValue::Nothing)
        },
        None => into_raw(FidanValue::Nothing),
    }
}

// ── collections module ────────────────────────────────────────────────────────
fn dispatch_collections(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::collections::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

// ── env module ────────────────────────────────────────────────────────────────
fn dispatch_env(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::env::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

// ── regex module ──────────────────────────────────────────────────────────────
fn dispatch_regex(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::regex::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

// ── parallel module ───────────────────────────────────────────────────────────
// These run sequentially in AOT. Behaviour matches the interpreter contract.
fn dispatch_parallel(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match func {
        "parallelMap" | "parallel_map" => {
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            let mut result = FidanList::new();
            for item in list {
                let item_ptr = into_raw(item);
                let mapped =
                    unsafe { fdn_call_dynamic(fn_ptr, &item_ptr as *const *mut FidanValue, 1) };
                if !mapped.is_null() {
                    result.append(unsafe { (*mapped).clone() });
                    unsafe { drop(Box::from_raw(mapped)) };
                } else {
                    result.append(FidanValue::Nothing);
                }
                unsafe { drop(Box::from_raw(item_ptr)) };
            }
            unsafe { drop(Box::from_raw(fn_ptr)) };
            into_raw(FidanValue::List(OwnedRef::new(result)))
        }
        "parallelFilter" | "parallel_filter" => {
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            let mut result = FidanList::new();
            for item in list {
                let item_ptr = into_raw(item.clone());
                let test =
                    unsafe { fdn_call_dynamic(fn_ptr, &item_ptr as *const *mut FidanValue, 1) };
                let keep = if !test.is_null() {
                    let keep = matches!(unsafe { &*test }, FidanValue::Boolean(true));
                    unsafe { drop(Box::from_raw(test)) };
                    keep
                } else {
                    false
                };
                if keep {
                    result.append(item);
                }
                unsafe { drop(Box::from_raw(item_ptr)) };
            }
            unsafe { drop(Box::from_raw(fn_ptr)) };
            into_raw(FidanValue::List(OwnedRef::new(result)))
        }
        "parallelForEach" | "parallel_for_each" | "parallelEach" => {
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            for item in list {
                let item_ptr = into_raw(item);
                let result =
                    unsafe { fdn_call_dynamic(fn_ptr, &item_ptr as *const *mut FidanValue, 1) };
                if !result.is_null() {
                    unsafe { drop(Box::from_raw(result)) };
                }
                unsafe { drop(Box::from_raw(item_ptr)) };
            }
            unsafe { drop(Box::from_raw(fn_ptr)) };
            into_raw(FidanValue::Nothing)
        }
        "parallelReduce" | "parallel_reduce" => {
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let initial = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let fn_val = args.get(2).cloned().unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            let mut acc = initial;
            for item in list {
                let acc_ptr = into_raw(acc);
                let item_ptr = into_raw(item);
                let call_args = [acc_ptr, item_ptr];
                let result = unsafe { fdn_call_dynamic(fn_ptr, call_args.as_ptr(), 2) };
                acc = if !result.is_null() {
                    let value = unsafe { (*result).clone() };
                    unsafe { drop(Box::from_raw(result)) };
                    value
                } else {
                    FidanValue::Nothing
                };
                unsafe { drop(Box::from_raw(acc_ptr)) };
                unsafe { drop(Box::from_raw(item_ptr)) };
            }
            unsafe { drop(Box::from_raw(fn_ptr)) };
            into_raw(acc)
        }
        _ => {
            eprintln!("AOT stdlib parallel: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── test module ───────────────────────────────────────────────────────────────
fn dispatch_test(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::test_runner::dispatch(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(msg)) => {
            eprintln!("Test failed: {}", msg);
            let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
            unsafe { fdn_throw_unhandled(msg_val) }
        }
        None => into_raw(FidanValue::Nothing),
    }
}

// ── time module ───────────────────────────────────────────────────────────────
fn dispatch_time(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::time::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

fn dispatch_async(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::async_std::dispatch(func, args) {
        Some(stdlib::async_std::AsyncDispatch::Value(value)) => into_raw(value),
        Some(stdlib::async_std::AsyncDispatch::Op(op)) => match op {
            stdlib::async_std::AsyncOp::Sleep { ms } => {
                into_raw(FidanValue::Pending(FidanPending::sleep(ms)))
            }
            stdlib::async_std::AsyncOp::Ready { value } => {
                into_raw(FidanValue::Pending(FidanPending::ready_result(Ok(value))))
            }
            stdlib::async_std::AsyncOp::Gather { values } => {
                let captures = values
                    .into_iter()
                    .map(|value| ParallelCapture(value.parallel_capture()))
                    .collect::<Vec<_>>();
                let pending =
                    FidanPending::defer_fallible(ParallelArgs::from_captures(captures), |bundle| {
                        let mut out = FidanList::new();
                        for value in bundle.into_vec() {
                            out.append(resolve_async_value_owned(value)?);
                        }
                        Ok(FidanValue::List(OwnedRef::new(out)))
                    });
                into_raw(FidanValue::Pending(pending))
            }
            stdlib::async_std::AsyncOp::WaitAny { values } => {
                let captures = values
                    .into_iter()
                    .map(|value| ParallelCapture(value.parallel_capture()))
                    .collect::<Vec<_>>();
                let pending =
                    FidanPending::defer_fallible(ParallelArgs::from_captures(captures), |bundle| {
                        let values = bundle.into_vec();
                        if values.is_empty() {
                            return Ok(async_wait_any_result(-1, FidanValue::Nothing));
                        }
                        loop {
                            for (index, value) in values.iter().enumerate() {
                                if let Some(result) = try_take_async_value_ready(value) {
                                    return result.map(|resolved| {
                                        async_wait_any_result(index as i64, resolved)
                                    });
                                }
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    });
                into_raw(FidanValue::Pending(pending))
            }
            stdlib::async_std::AsyncOp::Timeout { handle, ms } => {
                let pending = FidanPending::defer_fallible(
                    ParallelArgs::from_captures([ParallelCapture(handle.parallel_capture())]),
                    move |bundle| {
                        let handle = bundle
                            .into_vec()
                            .into_iter()
                            .next()
                            .unwrap_or(FidanValue::Nothing);
                        let deadline = Instant::now()
                            .checked_add(Duration::from_millis(ms))
                            .unwrap_or_else(Instant::now);
                        loop {
                            if let Some(result) = try_take_async_value_ready(&handle) {
                                return result.map(|resolved| async_timeout_result(true, resolved));
                            }
                            if Instant::now() >= deadline {
                                return Ok(async_timeout_result(false, FidanValue::Nothing));
                            }
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            std::thread::sleep(remaining.min(Duration::from_millis(1)));
                        }
                    },
                );
                into_raw(FidanValue::Pending(pending))
            }
        },
        None => into_raw(FidanValue::Nothing),
    }
}

// ── parallel iter ──────────────────────────────────────────────────────────────

/// Runtime implementation of `parallel for`: call `body_fn` (by FN_TABLE index)
/// once per element in `collection`, passing `[item, env_args...]` to the trampoline.
/// Called from AOT-generated code for `Instr::ParallelIter`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_parallel_iter_seq(
    collection: *mut FidanValue,
    fn_idx: i64,
    env_arr: *const *mut FidanValue,
    env_cnt: i64,
) {
    let coll = borrow(collection).clone();
    let env_slice: &[*mut FidanValue] = if env_cnt > 0 && !env_arr.is_null() {
        std::slice::from_raw_parts(env_arr, env_cnt as usize)
    } else {
        &[]
    };
    let items: Option<Vec<FidanValue>> = match coll {
        FidanValue::List(list_ref) => Some(list_ref.borrow().iter().cloned().collect()),
        FidanValue::Tuple(items) => Some(items),
        FidanValue::HashSet(set_ref) => Some(set_ref.borrow().values_sorted()),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            let mut items = Vec::new();
            if inclusive {
                for n in start..=end {
                    items.push(FidanValue::Integer(n));
                }
            } else {
                for n in start..end {
                    items.push(FidanValue::Integer(n));
                }
            }
            Some(items)
        }
        _ => None,
    };

    if let Some(items) = items {
        let env_caps: Vec<ParallelCapture> = env_slice
            .iter()
            .map(|ptr| ParallelCapture(borrow(*ptr).parallel_capture()))
            .collect();
        let first_exception: std::sync::Arc<std::sync::Mutex<Option<ParallelCapture>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));

        std::thread::scope(|scope| {
            for item in items {
                let mut caps = Vec::with_capacity(1 + env_caps.len());
                caps.push(ParallelCapture(item.parallel_capture()));
                caps.extend(
                    env_caps
                        .iter()
                        .map(|cap| ParallelCapture(cap.0.parallel_capture())),
                );
                let err_slot = std::sync::Arc::clone(&first_exception);
                scope.spawn(move || {
                    let result =
                        call_trampoline_owned(fn_idx as usize, ParallelArgs(caps).into_vec());
                    if fdn_has_exception() != 0 {
                        let exn_ptr = fdn_catch_exception();
                        let exn_cap = ParallelCapture(borrow(exn_ptr).parallel_capture());
                        drop(Box::from_raw(exn_ptr));
                        let mut slot = err_slot.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(exn_cap);
                        }
                    }
                    drop(result);
                });
            }
        });

        if let Some(exn_cap) = first_exception.lock().unwrap().take() {
            let exn_ptr = into_raw(exn_cap.into_inner());
            fdn_store_exception(exn_ptr);
            drop(Box::from_raw(exn_ptr));
        }
    }
}

unsafe fn build_parallel_args_from_ptrs(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> ParallelArgs {
    let args_slice: &[*mut FidanValue] = if args_cnt > 0 && !args_ptr.is_null() {
        std::slice::from_raw_parts(args_ptr, args_cnt as usize)
    } else {
        &[]
    };

    ParallelArgs::from_captures(
        args_slice
            .iter()
            .map(|ptr| ParallelCapture(borrow(*ptr).parallel_capture())),
    )
}

unsafe fn call_trampoline_owned(fn_idx: usize, values: Vec<FidanValue>) -> FidanValue {
    let mut arg_ptrs: Vec<*mut FidanValue> = values.into_iter().map(into_raw).collect();
    let result_ptr = call_trampoline_by_idx(fn_idx, &arg_ptrs);
    for ptr in arg_ptrs.drain(..) {
        drop(Box::from_raw(ptr));
    }

    if result_ptr.is_null() {
        FidanValue::Nothing
    } else {
        *Box::from_raw(result_ptr)
    }
}

unsafe fn call_dynamic_owned(function_value: FidanValue, args: Vec<FidanValue>) -> FidanValue {
    let function_ptr = into_raw(function_value);
    let mut arg_ptrs: Vec<*mut FidanValue> = args.into_iter().map(into_raw).collect();
    let result_ptr = fdn_call_dynamic(function_ptr, arg_ptrs.as_ptr(), arg_ptrs.len() as i64);
    drop(Box::from_raw(function_ptr));
    for ptr in arg_ptrs.drain(..) {
        drop(Box::from_raw(ptr));
    }

    if result_ptr.is_null() {
        FidanValue::Nothing
    } else {
        *Box::from_raw(result_ptr)
    }
}

unsafe fn call_method_owned(
    receiver: FidanValue,
    method_name: &str,
    args: Vec<FidanValue>,
) -> FidanValue {
    let receiver_ptr = into_raw(receiver);
    let method_bytes = method_name.as_bytes();
    let mut arg_ptrs: Vec<*mut FidanValue> = args.into_iter().map(into_raw).collect();
    let result_ptr = fdn_obj_invoke(
        receiver_ptr,
        method_bytes.as_ptr(),
        method_bytes.len() as i64,
        arg_ptrs.as_ptr(),
        arg_ptrs.len() as i64,
    );
    drop(Box::from_raw(receiver_ptr));
    for ptr in arg_ptrs.drain(..) {
        drop(Box::from_raw(ptr));
    }

    if result_ptr.is_null() {
        FidanValue::Nothing
    } else {
        *Box::from_raw(result_ptr)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_expr(
    fn_idx: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = build_parallel_args_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::defer_fallible(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, bundle.into_vec());
        if fdn_has_exception() != 0 {
            let exn_ptr = fdn_catch_exception();
            let message = display(borrow(exn_ptr));
            drop(Box::from_raw(exn_ptr));
            Err(message)
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_task(
    fn_idx: i64,
    name_bytes: *const u8,
    name_len: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let task_name = str_from_raw(name_bytes, name_len);
    let args = build_parallel_args_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::spawn_fallible(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, bundle.into_vec());
        if fdn_has_exception() != 0 {
            let exn_ptr = fdn_catch_exception();
            let message = display(borrow(exn_ptr));
            drop(Box::from_raw(exn_ptr));
            Err(format!("task `{task_name}` failed: {message}"))
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_concurrent(
    fn_idx: i64,
    _name_bytes: *const u8,
    _name_len: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = build_parallel_args_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::defer_fallible(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, bundle.into_vec());
        if fdn_has_exception() != 0 {
            let exn_ptr = fdn_catch_exception();
            let message = display(borrow(exn_ptr));
            drop(Box::from_raw(exn_ptr));
            Err(message)
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_dynamic(
    function_or_receiver: *mut FidanValue,
    method_bytes: *const u8,
    method_len: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let mut captures = vec![ParallelCapture(
        borrow(function_or_receiver).parallel_capture(),
    )];
    captures.extend(
        (0..args_cnt as usize)
            .map(|i| ParallelCapture(borrow(*args_ptr.add(i)).parallel_capture())),
    );
    let args = ParallelArgs::from_captures(captures);
    let method_name =
        (!method_bytes.is_null() && method_len > 0).then(|| str_from_raw(method_bytes, method_len));
    let pending = FidanPending::defer_fallible(args, move |bundle: ParallelArgs| {
        let mut values = bundle.into_vec();
        let first = values.remove(0);
        let result = if let Some(ref method_name) = method_name {
            call_method_owned(first, method_name, values)
        } else {
            call_dynamic_owned(first, values)
        };
        if fdn_has_exception() != 0 {
            let exn_ptr = fdn_catch_exception();
            let message = display(borrow(exn_ptr));
            drop(Box::from_raw(exn_ptr));
            Err(message)
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_pending_join(handle: *mut FidanValue) -> *mut FidanValue {
    match borrow(handle) {
        FidanValue::Pending(pending) => match pending.try_join() {
            Ok(value) => into_raw(value),
            Err(message) => {
                let exception = into_raw(FidanValue::String(FidanString::new(&message)));
                fdn_store_exception(exception);
                drop(Box::from_raw(exception));
                into_raw(FidanValue::Nothing)
            }
        },
        other => into_raw(other.clone()),
    }
}

// ── String interpolation ──────────────────────────────────────────────────────
/// Each `FidanValue` in `parts_ptr` is BORROWED (not dropped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_str_interp(
    parts_ptr: *const *mut FidanValue,
    count: i64,
) -> *mut FidanValue {
    let mut capacity = 0usize;
    for i in 0..count as usize {
        let p = *parts_ptr.add(i);
        capacity = capacity.saturating_add(display_len_hint(borrow(p)));
    }
    let mut result = String::with_capacity(capacity);
    for i in 0..count as usize {
        let p = *parts_ptr.add(i);
        crate::value::display_into(&mut result, borrow(p));
    }
    into_raw(FidanValue::String(FidanString::new(&result)))
}

// ── Exception handling ─────────────────────────────────────────────────────────

thread_local! {
    static EXCEPTION_VALUE: std::cell::RefCell<Option<*mut FidanValue>> =
        const { std::cell::RefCell::new(None) };
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_push_catch(_catch_id: i64) {}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_pop_catch() {}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_has_exception() -> i8 {
    EXCEPTION_VALUE.with(|e| if e.borrow().is_some() { 1 } else { 0 })
}

/// Store a thrown exception value in thread-local storage so the catch block
/// can retrieve it via `fdn_catch_exception`.  Called by the AOT throw path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_store_exception(val: *mut FidanValue) {
    let cloned = into_raw(borrow(val).clone());
    EXCEPTION_VALUE.with(|e| {
        *e.borrow_mut() = Some(cloned);
    });
}

/// Legacy throw stub — used when there is no enclosing catch handler.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_throw(val: *mut FidanValue) -> ! {
    eprintln!("unhandled exception: {}", display(borrow(val)));
    std::process::exit(1);
}

/// Called when `throw` has no enclosing catch in the current function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_throw_unhandled(val: *mut FidanValue) -> ! {
    eprintln!("unhandled exception: {}", display(borrow(val)));
    std::process::exit(1);
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_catch_exception() -> *mut FidanValue {
    EXCEPTION_VALUE.with(|e| {
        e.borrow_mut()
            .take()
            .unwrap_or_else(|| into_raw(FidanValue::Nothing))
    })
}

// ── Closures ───────────────────────────────────────────────────────────────────

/// Box a closure value that carries both an fn-id and its captured variables.
/// `captures_ptr` points to an array of `captures_cnt` *borrowed* `FidanValue`
/// pointers — they are cloned here so the boxed closure owns its captures.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_make_closure(
    fn_id: i64,
    captures_ptr: *const *mut FidanValue,
    captures_cnt: i64,
) -> *mut FidanValue {
    let captured: Vec<FidanValue> = (0..captures_cnt as usize)
        .map(|i| borrow(*captures_ptr.add(i)).clone())
        .collect();
    into_raw(FidanValue::Closure {
        fn_id: FunctionId(fn_id as u32),
        captured,
    })
}

// ── Dynamic function dispatch table ───────────────────────────────────────────
//
// Each compiled Fidan function gets a "trampoline" with the uniform signature
//   extern "C" fn(args_ptr: *const *mut FidanValue, args_cnt: i64) -> *mut FidanValue
// The AOT init code registers every trampoline via `fdn_fn_table_set`.
// `fdn_call_dynamic` looks up the correct trampoline and calls it, unwrapping
// closures so their captured values are prepended to the call arguments.

type FnTrampoline = unsafe extern "C" fn(*const *mut FidanValue, i64) -> *mut FidanValue;

static FN_TABLE: std::sync::OnceLock<std::sync::Mutex<Vec<Option<FnTrampoline>>>> =
    std::sync::OnceLock::new();

/// Name → trampoline-table index, populated by AOT-generated startup code.
static FN_NAME_TABLE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, usize>>,
> = std::sync::OnceLock::new();

/// Register a function name → table index mapping (called from AOT startup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_fn_name_register(name_bytes: *const u8, name_len: i64, idx: i64) {
    let name = unsafe { str_from_raw(name_bytes, name_len) }.to_string();
    let table =
        FN_NAME_TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut guard) = table.lock() {
        guard.insert(name, idx as usize);
    }
}

/// Allocate the global function table with `count` slots (all initialised to None).
/// Called once from the generated `fdn_init` before any user code runs.
#[unsafe(no_mangle)]
pub extern "C" fn fdn_fn_table_init(count: i64) {
    let _ = FN_TABLE.set(std::sync::Mutex::new(vec![None; count as usize]));
}

/// Register the trampoline for function index `idx`.
/// `ptr` is the raw function pointer cast to `usize`.
#[unsafe(no_mangle)]
pub extern "C" fn fdn_fn_table_set(idx: i64, ptr: usize) {
    if let Some(table) = FN_TABLE.get()
        && let Ok(mut guard) = table.lock()
    {
        let i = idx as usize;
        if i < guard.len() {
            // SAFETY: the AOT compiler guarantees `ptr` is a valid trampoline.
            guard[i] = Some(unsafe { std::mem::transmute::<usize, FnTrampoline>(ptr) });
        }
    }
}

/// Call a trampoline by its table index, with the given argument pointers (borrowed).
unsafe fn call_trampoline_by_idx(idx: usize, arg_ptrs: &[*mut FidanValue]) -> *mut FidanValue {
    let trampoline = FN_TABLE
        .get()
        .and_then(|t| t.lock().ok())
        .and_then(|g| g.get(idx).copied().flatten());
    match trampoline {
        Some(t) => t(arg_ptrs.as_ptr(), arg_ptrs.len() as i64),
        None => {
            eprintln!("AOT: call_trampoline_by_idx: no trampoline for idx {}", idx);
            into_raw(FidanValue::Nothing)
        }
    }
}

/// Call a dynamic function value.
/// `fn_val` is a borrowed `FidanValue` that is either `Function(id)` or
/// `Closure { fn_id, captured }`.  Extra call-site arguments are in
/// `args_ptr[0..args_cnt]` (also borrowed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_call_dynamic(
    fn_val: *mut FidanValue,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let fv = borrow(fn_val);

    // Stdlib functions are called directly without going through the fn table.
    if let FidanValue::StdlibFn(module, name) = fv {
        let args: Vec<FidanValue> = (0..args_cnt as usize)
            .map(|i| borrow(*args_ptr.add(i)).clone())
            .collect();
        return dispatch_stdlib_inline(module, name, args).unwrap_or_else(|| {
            eprintln!("AOT: fdn_call_dynamic: unknown stdlib module '{}'", module);
            into_raw(FidanValue::Nothing)
        });
    }

    let (fn_id, captured): (u32, &[FidanValue]) = match fv {
        FidanValue::Function(FunctionId(id)) => (*id, &[]),
        FidanValue::Closure {
            fn_id: FunctionId(id),
            captured,
        } => (*id, captured.as_slice()),
        _ => {
            eprintln!(
                "AOT: fdn_call_dynamic: not a callable value ({})",
                fv.type_name()
            );
            return into_raw(FidanValue::Nothing);
        }
    };

    let trampoline = FN_TABLE
        .get()
        .and_then(|t| t.lock().ok())
        .and_then(|g| g.get(fn_id as usize).copied().flatten());

    let Some(trampoline) = trampoline else {
        eprintln!("AOT: fdn_call_dynamic: no trampoline for fn_id {}", fn_id);
        return into_raw(FidanValue::Nothing);
    };

    // Build the unified args array: captured values first, then call-site args.
    // All values must be boxed since the trampoline expects *mut FidanValue.
    let call_site: Vec<*mut FidanValue> =
        (0..args_cnt as usize).map(|i| *args_ptr.add(i)).collect();

    if captured.is_empty() {
        // Fast path: no captures, reuse the existing pointer array.
        trampoline(call_site.as_ptr(), call_site.len() as i64)
    } else {
        // Slow path: prepend captured values (boxed as temporaries).
        let mut boxed_caps: Vec<*mut FidanValue> =
            captured.iter().map(|v| into_raw(v.clone())).collect();
        let all_len = boxed_caps.len() + call_site.len();
        boxed_caps.extend_from_slice(&call_site);
        let result = trampoline(boxed_caps.as_ptr(), all_len as i64);
        // Drop the temporary boxes for captures (call-site pointers are borrowed — not ours).
        for ptr in boxed_caps.iter().take(captured.len()) {
            drop(Box::from_raw(*ptr));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_exception() -> Option<FidanValue> {
        unsafe {
            if fdn_has_exception() == 0 {
                None
            } else {
                let ptr = fdn_catch_exception();
                Some(*Box::from_raw(ptr))
            }
        }
    }

    #[test]
    fn tuple_pack_and_index_round_trip() {
        unsafe {
            let first = into_raw(FidanValue::Integer(7));
            let second = into_raw(FidanValue::String(FidanString::new("value")));
            let values = [first, second];

            let tuple = fdn_tuple_pack(values.as_ptr(), values.len() as i64);
            match borrow(tuple) {
                FidanValue::Tuple(items) => {
                    assert_eq!(items.len(), 2);
                    assert!(matches!(items.first(), Some(FidanValue::Integer(7))));
                }
                other => panic!("expected tuple, got {:?}", other),
            }

            let zero = into_raw(FidanValue::Integer(0));
            let one = into_raw(FidanValue::Integer(1));
            let first_value = fdn_list_get(tuple, zero);
            let second_value = fdn_list_get(tuple, one);

            assert!(matches!(borrow(first_value), FidanValue::Integer(7)));
            match borrow(second_value) {
                FidanValue::String(text) => assert_eq!(text.as_str(), "value"),
                other => panic!("expected string, got {:?}", other),
            }

            drop(Box::from_raw(first));
            drop(Box::from_raw(second));
            drop(Box::from_raw(zero));
            drop(Box::from_raw(one));
            drop(Box::from_raw(first_value));
            drop(Box::from_raw(second_value));
            drop(Box::from_raw(tuple));
        }
    }

    #[test]
    fn dispatch_io_sets_exception_slot_for_runtime_errors() {
        let path = std::env::temp_dir().join("fidan-aot-ffi-io-missing.txt");
        let _ = std::fs::remove_file(&path);
        let result = dispatch_io(
            "readFile",
            vec![FidanValue::String(FidanString::new(
                &path.to_string_lossy(),
            ))],
        );

        assert!(matches!(unsafe { borrow(result) }, FidanValue::Nothing));
        let exception = drain_exception().expect("expected stored exception");
        match exception {
            FidanValue::String(text) => {
                assert!(text.as_str().contains("R3001"));
                assert!(text.as_str().contains("failed to open file"));
            }
            other => panic!("expected string exception, got {other:?}"),
        }

        unsafe {
            drop(Box::from_raw(result));
        }
    }

    #[test]
    fn dispatch_json_sets_exception_slot_for_runtime_errors() {
        let path = std::env::temp_dir().join("fidan-aot-ffi-json-invalid.json");
        std::fs::write(&path, "{not json").expect("write invalid json fixture");
        let result = dispatch_json(
            "load",
            vec![FidanValue::String(FidanString::new(
                &path.to_string_lossy(),
            ))],
        );

        assert!(matches!(unsafe { borrow(result) }, FidanValue::Nothing));
        let exception = drain_exception().expect("expected stored exception");
        match exception {
            FidanValue::String(text) => {
                assert!(text.as_str().contains("R3005"));
                assert!(text.as_str().contains("failed to parse JSON"));
            }
            other => panic!("expected string exception, got {other:?}"),
        }

        let _ = std::fs::remove_file(path);
        unsafe {
            drop(Box::from_raw(result));
        }
    }
}
