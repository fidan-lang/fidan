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
// Both AOT backends release fresh list/dict construction boxes after the
// collection helper clones them, and release prepared dynamic-call arguments.
// Both backends release explicitly cloned direct-call arguments, fresh boxed
// operator inputs, and owned operator results after scalar unboxing.
// Other call sites still create temporary boxes without a matching `fdn_drop`,
// including interpolation and some direct-call arguments.
// These allocations remain live for the process lifetime; a broader temporary
// ownership pass is still needed. Borrowed locals must never be freed as temps.

#![allow(clippy::missing_safety_doc)]
// In Rust 2024 edition, unsafe operations inside `unsafe fn` bodies require
// an explicit `unsafe { }` block.  We suppress this lint here because ffi.rs
// is pure FFI glue: every public function is already `unsafe extern "C"`, and
// adding inner unsafe blocks everywhere would add noise without safety benefit.
#![allow(unsafe_op_in_unsafe_fn)]

use crate::{
    FidanDict, FidanHashSet, FidanList, FidanString, OwnedRef, SharedRef,
    parallel::{FidanPending, ParallelArgs},
    stdlib,
    value::{FidanValue, FunctionId, display},
};
use fidan_config::{
    BuiltinSemantic, ReceiverBuiltinKind, ReceiverMethodOp, builtin_semantic, infer_receiver_member,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{
    cell::RefCell,
    io::{BufWriter, IsTerminal, LineWriter, Write},
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
    // Native AOT globals live in the executing thread. The generated data slots
    // provide stable identities only; storing FidanValue pointers in them would
    // let a real parallel worker clone an Rc-backed value from another thread.
    static AOT_GLOBALS: RefCell<HashMap<usize, FidanValue>> = RefCell::new(HashMap::new());
}

/// The captured globals for one real native worker. ParallelArgs snapshots
/// globals and call arguments as one graph, so aliases spanning that boundary
/// remain aliases in the worker without retaining an Rc from its parent.
struct AotWorkerGlobals {
    slots: Vec<usize>,
}

fn capture_aot_worker_payload(
    task_values: impl IntoIterator<Item = FidanValue>,
) -> (AotWorkerGlobals, ParallelArgs) {
    let (slots, mut values): (Vec<_>, Vec<_>) = AOT_GLOBALS.with(|globals| {
        globals
            .borrow()
            .iter()
            .map(|(slot, value)| (*slot, value.clone()))
            .unzip()
    });
    values.extend(task_values);
    (AotWorkerGlobals { slots }, ParallelArgs::new(&values))
}

impl AotWorkerGlobals {
    /// Install this worker's detached global snapshot and return its task args.
    /// Worker threads are fresh, so replacing rather than merging prevents an
    /// accidental ambient global from observing a parent-thread Rc allocation.
    fn install(self, payload: ParallelArgs) -> Vec<FidanValue> {
        let mut values = payload.into_vec();
        let task_args = values.split_off(self.slots.len());
        let globals = self.slots.into_iter().zip(values).collect();
        AOT_GLOBALS.with(|current| *current.borrow_mut() = globals);
        task_args
    }
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

fn missing_method(receiver: &FidanValue, method_name: &str) -> *mut FidanValue {
    unsafe {
        runtime_call_error(format!(
            "no method `{method_name}` found for `{}`",
            receiver.type_name()
        ))
    }
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
    v.truthy()
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
    into_raw(FidanValue::Shared(SharedRef::from_value(
        borrow(ptr).clone(),
    )))
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

/// Load a native AOT global by its generated slot address.
///
/// The address is an opaque stable key. It is deliberately never dereferenced:
/// ordinary Fidan values may contain Rc-backed collections and are therefore
/// owned by the current thread's global map rather than process-global storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_global_load(slot: *mut FidanValue) -> *mut FidanValue {
    debug_assert!(!slot.is_null(), "fdn_global_load requires a slot address");
    let key = slot as usize;
    let value = AOT_GLOBALS.with(|globals| globals.borrow().get(&key).cloned());
    into_raw(value.unwrap_or(FidanValue::Nothing))
}

/// Store a native AOT global in the current thread's runtime-owned map.
///
/// value is borrowed. The map owns its clone, preserving regular same-thread
/// Fidan alias semantics while preventing static storage from retaining an Rc.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_global_store(slot: *mut FidanValue, value: *mut FidanValue) {
    debug_assert!(!slot.is_null(), "fdn_global_store requires a slot address");
    let key = slot as usize;
    AOT_GLOBALS.with(|globals| {
        globals.borrow_mut().insert(key, borrow(value).clone());
    });
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

// Arithmetic failures use the existing exception slot, never unwind across C.
fn checked_integer_result(result: Result<i64, stdlib::StdlibRuntimeError>) -> i64 {
    match result {
        Ok(value) => value,
        Err(error) => {
            store_runtime_error(error.code, error.message);
            0 // The caller checks the exception slot before using this value.
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_add(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::add(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_sub(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::sub(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_mul(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::mul(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_div(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::div(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_rem(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::rem(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_pow(a: i64, b: i64) -> i64 {
    checked_integer_result(crate::integer::pow(a, b))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_abs(a: i64) -> i64 {
    checked_integer_result(crate::integer::abs(a))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_int_neg(a: i64) -> i64 {
    checked_integer_result(crate::integer::neg(a))
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_float_div(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        checked_integer_result(Err(stdlib::StdlibRuntimeError::new(
            fidan_diagnostics::diag_code!("R2001"),
            "division by zero",
        )));
        return 0.0;
    }
    a / b
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_float_rem(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        checked_integer_result(Err(stdlib::StdlibRuntimeError::new(
            fidan_diagnostics::diag_code!("R2001"),
            "modulo by zero",
        )));
        return 0.0;
    }
    a % b
}

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
                _ => {
                    return runtime_call_error(format!(
                        "type error: arithmetic on {} and {}",
                        av.type_name(),
                        bv.type_name()
                    ))
                }
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
                _ => {
                    return runtime_call_error(format!(
                        "type error: bitwise operands must be integers, got {} and {}",
                        av.type_name(),
                        bv.type_name()
                    ))
                }
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
            into_raw(FidanValue::Integer(fdn_int_add(*x, *y)))
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
        (FidanValue::String(_), _) | (_, FidanValue::String(_)) => fdn_dyn_concat(a, b),
        _ => runtime_call_error(format!(
            "type error: Add on {} and {}",
            av.type_name(),
            bv.type_name()
        )),
    }
}

numeric_binop!(
    fdn_dyn_sub,
    |x: i64, y: i64| fdn_int_sub(x, y),
    |x: f64, y: f64| x - y
);
numeric_binop!(
    fdn_dyn_mul,
    |x: i64, y: i64| fdn_int_mul(x, y),
    |x: f64, y: f64| x * y
);
numeric_binop!(
    fdn_dyn_div,
    |x: i64, y: i64| fdn_int_div(x, y),
    |x: f64, y: f64| fdn_float_div(x, y)
);
numeric_binop!(
    fdn_dyn_rem,
    |x: i64, y: i64| fdn_int_rem(x, y),
    |x: f64, y: f64| fdn_float_rem(x, y)
);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_pow(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    let av = borrow(a);
    let bv = borrow(b);
    let result = match (av, bv) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => match crate::integer::power(*x, *y) {
            Ok(crate::integer::Power::Integer(value)) => FidanValue::Integer(value),
            Ok(crate::integer::Power::Float(value)) => FidanValue::Float(value),
            Err(error) => {
                store_runtime_error(error.code, error.message);
                return into_raw(FidanValue::Nothing);
            }
        },
        (FidanValue::Float(x), FidanValue::Float(y)) => FidanValue::Float(x.powf(*y)),
        (FidanValue::Integer(x), FidanValue::Float(y)) => FidanValue::Float((*x as f64).powf(*y)),
        (FidanValue::Float(x), FidanValue::Integer(y)) => FidanValue::Float(x.powf(*y as f64)),
        _ => {
            return runtime_call_error(format!(
                "type error: Pow on {} and {}",
                av.type_name(),
                bv.type_name()
            ));
        }
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
                (FidanValue::Boolean(x), FidanValue::Boolean(y))
                    if matches!(stringify!($name), "fdn_dyn_eq" | "fdn_dyn_ne") =>
                {
                    $int_cmp(&(*x as i64), &(*y as i64))
                }
                (FidanValue::Nothing, FidanValue::Nothing)
                    if matches!(stringify!($name), "fdn_dyn_eq" | "fdn_dyn_ne") =>
                {
                    (stringify!($name) == "fdn_dyn_eq")
                }
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
                        drop(Box::from_raw(runtime_call_error(
                            "type error: ordering enum values",
                        )));
                        false
                    }
                }
                // One operand is Nothing, the other is not: for eq→false, ne→true, ordering→false.
                (FidanValue::Nothing, _) | (_, FidanValue::Nothing)
                    if matches!(stringify!($name), "fdn_dyn_eq" | "fdn_dyn_ne") =>
                {
                    stringify!($name) == "fdn_dyn_ne"
                }
                (FidanValue::EnumType(a), FidanValue::EnumType(b))
                    if stringify!($name) == "fdn_dyn_eq" =>
                {
                    a == b
                }
                (FidanValue::EnumType(a), FidanValue::EnumType(b))
                    if stringify!($name) == "fdn_dyn_ne" =>
                {
                    a != b
                }
                (FidanValue::EnumType(_), FidanValue::EnumVariant { .. })
                | (FidanValue::EnumVariant { .. }, FidanValue::EnumType(_))
                    if stringify!($name) == "fdn_dyn_eq" =>
                {
                    false
                }
                (FidanValue::EnumType(_), FidanValue::EnumVariant { .. })
                | (FidanValue::EnumVariant { .. }, FidanValue::EnumType(_))
                    if stringify!($name) == "fdn_dyn_ne" =>
                {
                    true
                }
                (FidanValue::ClassType(a), FidanValue::ClassType(b))
                    if stringify!($name) == "fdn_dyn_eq" =>
                {
                    a == b
                }
                (FidanValue::ClassType(a), FidanValue::ClassType(b))
                    if stringify!($name) == "fdn_dyn_ne" =>
                {
                    a != b
                }
                (FidanValue::Dict(a), FidanValue::Dict(b))
                    if a.borrow().aot_class_name().is_some()
                        && b.borrow().aot_class_name().is_some()
                        && matches!(stringify!($name), "fdn_dyn_eq" | "fdn_dyn_ne") =>
                {
                    (a.identity() == b.identity()) == (stringify!($name) == "fdn_dyn_eq")
                }
                (FidanValue::Object(a), FidanValue::Object(b))
                    if stringify!($name) == "fdn_dyn_eq" =>
                {
                    a.identity() == b.identity()
                }
                (FidanValue::Object(a), FidanValue::Object(b))
                    if stringify!($name) == "fdn_dyn_ne" =>
                {
                    a.identity() != b.identity()
                }
                _ => {
                    drop(Box::from_raw(runtime_call_error(format!(
                        "type error: comparison on {} and {}",
                        av.type_name(),
                        bv.type_name()
                    ))));
                    false
                }
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

/// MIR controls evaluation order; logical operators require Boolean operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_and(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    match (borrow(a), borrow(b)) {
        (FidanValue::Boolean(a), FidanValue::Boolean(b)) => into_raw(FidanValue::Boolean(*a && *b)),
        (a, b) => runtime_call_error(format!(
            "type error: And on {} and {}",
            a.type_name(),
            b.type_name()
        )),
    }
}

/// Boolean logical disjunction.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_or(a: *mut FidanValue, b: *mut FidanValue) -> *mut FidanValue {
    match (borrow(a), borrow(b)) {
        (FidanValue::Boolean(a), FidanValue::Boolean(b)) => into_raw(FidanValue::Boolean(*a || *b)),
        (a, b) => runtime_call_error(format!(
            "type error: Or on {} and {}",
            a.type_name(),
            b.type_name()
        )),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_not(ptr: *mut FidanValue) -> *mut FidanValue {
    match borrow(ptr) {
        FidanValue::Boolean(value) => into_raw(FidanValue::Boolean(!value)),
        value => runtime_call_error(format!("type error: Not on {}", value.type_name())),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dyn_neg(ptr: *mut FidanValue) -> *mut FidanValue {
    let result = match borrow(ptr) {
        FidanValue::Integer(n) => FidanValue::Integer(fdn_int_neg(*n)),
        FidanValue::Float(f) => FidanValue::Float(-f),
        value => return runtime_call_error(format!("type error: Neg on {}", value.type_name())),
    };
    into_raw(result)
}

// ── Bit operations ─────────────────────────────────────────────────────────────

int_only_binop!(fdn_dyn_bit_xor, |a: i64, b: i64| a ^ b);
int_only_binop!(fdn_dyn_bit_and, |a: i64, b: i64| a & b);
int_only_binop!(fdn_dyn_bit_or, |a: i64, b: i64| a | b);
int_only_binop!(fdn_dyn_shl, |a: i64, b: i64| a
    .wrapping_shl((b & 63) as u32));
int_only_binop!(fdn_dyn_shr, |a: i64, b: i64| a
    .wrapping_shr((b & 63) as u32));

// ── Range construction ─────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn fdn_make_range(start: i64, end: i64, inclusive: i8) -> *mut FidanValue {
    into_raw(FidanValue::Range {
        start,
        end,
        inclusive: inclusive != 0,
    })
}

/// Boxed range bounds require actual integers; scalar ABI coercions do not apply.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_make_range_checked(
    start: *mut FidanValue,
    end: *mut FidanValue,
    inclusive: i8,
) -> *mut FidanValue {
    match (borrow(start), borrow(end)) {
        (FidanValue::Integer(start), FidanValue::Integer(end)) => {
            fdn_make_range(*start, *end, inclusive)
        }
        (start, end) => runtime_call_error(format!(
            "type error: range bounds must be integers, got {} and {}",
            start.type_name(),
            end.type_name()
        )),
    }
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
    flush_stdout_buffer();
    // Null means no prompt; an explicitly supplied Nothing is still displayed.
    if !prompt.is_null() {
        with_stdout_buffer(|stdout| {
            let _ = crate::value::write_display_io(stdout, borrow(prompt));
            let _ = stdout.flush();
        });
    }
    builtin_value_result(crate::builtins::read_input_line(
        &mut std::io::stdin().lock(),
    ))
}

/// Return the length of a string / list / dict / range.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_len(ptr: *mut FidanValue) -> i64 {
    checked_integer_result(crate::builtins::len(borrow(ptr)))
}

/// Panic with a message.  Borrows `ptr`.  Does not return.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_panic(ptr: *mut FidanValue) -> ! {
    fdn_store_panic(ptr);
    fdn_report_unhandled_exception();
}

/// Assert `cond != 0`; panic with `msg` on failure.  Borrows `msg`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert(cond: i64, msg: *mut FidanValue) {
    if cond == 0 {
        fdn_throw_unhandled(msg);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_eq(lhs: *mut FidanValue, rhs: *mut FidanValue) {
    fdn_assert_eq_with_message(lhs, rhs, std::ptr::null_mut());
}

/// Borrows all arguments. A null message preserves the generated default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_eq_with_message(
    lhs: *mut FidanValue,
    rhs: *mut FidanValue,
    message: *mut FidanValue,
) {
    let left = borrow(lhs).clone();
    let right = borrow(rhs).clone();
    if !values_equal(&left, &right) {
        let msg = if message.is_null() {
            format!("assertEq failed: {} != {}", display(&left), display(&right))
        } else {
            display(borrow(message))
        };
        let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
        fdn_throw_unhandled(msg_val);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_ne(lhs: *mut FidanValue, rhs: *mut FidanValue) {
    fdn_assert_ne_with_message(lhs, rhs, std::ptr::null_mut());
}

/// Borrows all arguments. A null message preserves the generated default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_assert_ne_with_message(
    lhs: *mut FidanValue,
    rhs: *mut FidanValue,
    message: *mut FidanValue,
) {
    let left = borrow(lhs).clone();
    let right = borrow(rhs).clone();
    if values_equal(&left, &right) {
        let msg = if message.is_null() {
            format!("assertNe failed: both are {}", display(&left))
        } else {
            display(borrow(message))
        };
        let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
        fdn_throw_unhandled(msg_val);
    }
}

/// Return the runtime type name as a new owned `String`.  Borrows `ptr`.
// AOT objects already carry their class tag in the established Dict representation.
// Preserve the interpreter's `type(object) == "object"`, including typed catches.
fn native_type_name(value: &FidanValue) -> &'static str {
    if let FidanValue::Dict(fields) = value
        && fields.borrow().aot_class_name().is_some()
    {
        return "object";
    }
    value.type_name()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_type_name(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::String(FidanString::new(native_type_name(
        borrow(ptr),
    ))))
}

/// Stringify via `display` and return a new owned `String`.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_string(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::String(FidanString::new(&display(borrow(ptr)))))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_integer(ptr: *mut FidanValue) -> *mut FidanValue {
    builtin_value_result(crate::builtins::integer(borrow(ptr)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_float(ptr: *mut FidanValue) -> *mut FidanValue {
    builtin_value_result(crate::builtins::float(borrow(ptr)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_to_boolean(ptr: *mut FidanValue) -> *mut FidanValue {
    into_raw(FidanValue::Boolean(borrow(ptr).truthy()))
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
        drop(Box::from_raw(runtime_call_error(format!(
            "certain parameter `{name}` cannot be nothing"
        ))));
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
    let optional = |ptr| match borrow(ptr) {
        FidanValue::Nothing => None,
        value => Some(value),
    };
    match crate::slice_value(
        borrow(obj),
        optional(start),
        optional(end),
        inclusive != 0,
        optional(step),
    ) {
        Ok(value) => into_raw(value),
        Err(message) => {
            fdn_throw_unhandled(into_raw(FidanValue::String(FidanString::new(&message))));
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
/// Returns an owned clone; bounds/type failures populate the exception slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_get(
    list: *mut FidanValue,
    idx: *mut FidanValue,
) -> *mut FidanValue {
    match crate::index::get(borrow(list), borrow(idx)) {
        Ok(value) => into_raw(value),
        Err(error) => {
            store_runtime_error(error.code, error.message);
            into_raw(FidanValue::Nothing)
        }
    }
}

/// Set `list[idx]` to a clone of `val`.  Borrows both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_set(
    list: *mut FidanValue,
    idx: *mut FidanValue,
    val: *mut FidanValue,
) {
    if let Err(error) = crate::index::set(borrow(list), borrow(idx), borrow(val).clone()) {
        store_runtime_error(error.code, error.message);
    }
}

/// Return the number of elements.  Borrows `list`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_len(list: *mut FidanValue) -> i64 {
    fdn_len(list)
}

/// Concatenate two lists into a new owned list.  Borrows both.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_list_concat(
    a: *mut FidanValue,
    b: *mut FidanValue,
) -> *mut FidanValue {
    let capacity = match borrow(a) {
        FidanValue::List(la) => la.borrow().len(),
        _ => 0,
    } + match borrow(b) {
        FidanValue::List(lb) => lb.borrow().len(),
        _ => 0,
    };
    let mut result = FidanList::with_capacity(capacity);
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
    fdn_list_get(dict, key)
}

/// Insert a clone of `val` under `key`.  Borrows `dict`, `key`, and `val`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_dict_set(
    dict: *mut FidanValue,
    key: *mut FidanValue,
    val: *mut FidanValue,
) {
    fdn_list_set(dict, key, val);
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
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (key, _) in borrow.iter() {
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
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (_, value) in borrow.iter() {
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
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (key, value) in borrow.iter() {
                let mut pair = FidanList::with_capacity(2);
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
        d.mark_aot_object(FidanString::new(&class_name));
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
    if let Some(value) = crate::receivers::receiver_field(borrow(obj), &field_name) {
        return into_raw(value);
    }
    let result = match borrow(obj) {
        FidanValue::Dict(d) => {
            let fields = d.borrow();
            if let Some(value) = fields
                .get(&FidanValue::String(FidanString::new(&field_name)))
                .ok()
                .flatten()
            {
                value.clone()
            } else if fields.aot_class_name().is_some() {
                let key = FidanValue::String(FidanString::new(&format!("__method__{field_name}")));
                if let Some(FidanValue::Function(fn_id)) = fields.get(&key).ok().flatten() {
                    FidanValue::Closure {
                        fn_id: *fn_id,
                        captured: vec![borrow(obj).clone()],
                    }
                } else {
                    FidanValue::Nothing
                }
            } else {
                FidanValue::Nothing
            }
        }
        value @ (FidanValue::Function(_)
        | FidanValue::Closure { .. }
        | FidanValue::StdlibFn(..)) => {
            let result = registered_callable_metadata(value)
                .and_then(|(metadata, captures)| {
                    crate::callable::field(&metadata, captures, &field_name)
                })
                .map(|value| value.unwrap_or(FidanValue::Nothing));
            return builtin_value_result(result);
        }
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
        FidanValue::Namespace(module) => {
            let qualified = format!("{module}::{field_name}");
            if let Some(table) = FN_NAME_TABLE.get()
                && let Ok(guard) = table.lock()
                && let Some(member) = guard.get(qualified.as_str())
            {
                return into_raw(match member {
                    RegisteredNamespaceMember::Function(id) => {
                        FidanValue::Function(FunctionId(*id as u32))
                    }
                    RegisteredNamespaceMember::Namespace(target) => {
                        FidanValue::Namespace(Arc::clone(target))
                    }
                });
            }
            if module.starts_with("__module_") {
                return into_raw(FidanValue::Nothing);
            }
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

    if let Some(result) = crate::builtins::receiver_conversion(recv, &method_name) {
        return builtin_value_result(result);
    }
    if let Some(result) = crate::receivers::numeric_method(recv, &method_name, &extra) {
        return builtin_value_result(result);
    }
    match recv {
        FidanValue::EnumType(_) => into_raw(FidanValue::EnumVariant {
            tag: Arc::from(method_name.as_str()),
            payload: extra,
        }),
        FidanValue::Namespace(ns_name) => {
            // Try stdlib first. dispatch_stdlib_inline returns None for unrecognised
            // module names, so adding a new stdlib module only requires updating that
            // one function — no separate list here to keep in sync.
            if let Some(v) = dispatch_stdlib_inline(ns_name.as_ref(), &method_name, extra.clone()) {
                return v;
            }
            // User namespace: look up method_name in the name table and call through FN_TABLE.
            let qualified = if ns_name.starts_with("__module_") {
                format!("{ns_name}::{method_name}")
            } else {
                method_name.clone()
            };
            if let Some(name_table) = FN_NAME_TABLE.get()
                && let Ok(guard) = name_table.lock()
                && let Some(&RegisteredNamespaceMember::Function(idx)) =
                    guard.get(qualified.as_str())
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
        value @ (FidanValue::Function(_)
        | FidanValue::Closure { .. }
        | FidanValue::StdlibFn(..))
            if method_name == "invoke" =>
        {
            match crate::callable::invocation_arguments(extra) {
                Ok(args) => into_raw(call_dynamic_owned(value.clone(), args)),
                Err(error) => builtin_value_result(Err(error)),
            }
        }
        FidanValue::String(s) => dispatch_string_method(s.clone(), &method_name, extra),
        FidanValue::List(l) => dispatch_list_method(l, &method_name, extra),
        FidanValue::Dict(d) => {
            let branded = d.borrow().aot_class_name().is_some();
            if branded {
                let field = d
                    .borrow()
                    .get(&FidanValue::String(FidanString::new(&method_name)))
                    .ok()
                    .flatten()
                    .cloned();
                if let Some(callable) = field {
                    return into_raw(call_dynamic_owned(callable, extra));
                }
            }
            // Check for a user-defined method stored as `__method__<name>` in the dict.
            let method_key =
                FidanValue::String(FidanString::new(&format!("__method__{}", method_name)));
            // Release the field borrow before calling a method that may mutate self.
            let method_fn = if branded {
                d.borrow().get(&method_key).ok().flatten().cloned()
            } else {
                None
            };
            if let Some(method_fn) = method_fn {
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
            if branded {
                missing_method(recv, &method_name)
            } else {
                dispatch_dict_method(d, &method_name, extra)
            }
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
                Some("get") => match sr.snapshot() {
                    Ok(value) => into_raw(value),
                    Err(error) => runtime_call_error(error),
                },
                Some("set") => {
                    let val = extra.into_iter().next().unwrap_or(FidanValue::Nothing);
                    match sr.store_value(val) {
                        Ok(()) => into_raw(FidanValue::Nothing),
                        Err(error) => runtime_call_error(error),
                    }
                }
                Some("update") => {
                    if extra.len() != 1 {
                        return runtime_call_error("Shared.update expects exactly one callback");
                    }
                    shared_update(sr, extra.into_iter().next().unwrap(), None)
                }
                Some("weak") => into_raw(FidanValue::WeakShared(sr.downgrade())),
                _ => missing_method(recv, &method_name),
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
                _ => missing_method(recv, &method_name),
            }
        }
        _ => missing_method(recv, &method_name),
    }
}

// ── String method dispatch ─────────────────────────────────────────────────────

fn async_wait_any_result(index: i64, value: FidanValue) -> FidanValue {
    stdlib::async_std::wait_any_result(index, value)
}

fn async_timeout_result(completed: bool, value: FidanValue) -> FidanValue {
    stdlib::async_std::timeout_result(completed, value)
}

fn resolve_async_value_owned(
    value: FidanValue,
) -> Result<FidanValue, crate::exception::CapturedException> {
    match value {
        FidanValue::Pending(pending) => pending.try_join_exception(),
        other => Ok(other),
    }
}

fn try_take_async_value_ready(
    value: &FidanValue,
) -> Option<Result<FidanValue, crate::exception::CapturedException>> {
    match value {
        FidanValue::Pending(pending) => pending.try_take_ready_exception(),
        other => Some(Ok(other.clone())),
    }
}

unsafe fn shared_update(
    shared: &crate::SharedRef<FidanValue>,
    callback: FidanValue,
    contract: Option<&str>,
) -> *mut FidanValue {
    let result = shared.transform(|current| {
        let result = call_dynamic_owned(callback, vec![current]);
        if fdn_has_exception() != 0 {
            // Keep the original callback exception in its established channel.
            return Err(stdlib::StdlibRuntimeError::new(
                fidan_diagnostics::diag_code!("R0001"),
                "Shared.update callback failed",
            ));
        }
        if let Some(contract) = contract {
            crate::contracts::prepare(result, contract)
        } else {
            Ok(result)
        }
    });
    match result {
        Ok(value) => into_raw(value),
        Err(_) if fdn_has_exception() != 0 => into_raw(FidanValue::Nothing),
        Err(error) => runtime_call_error(error.message),
    }
}

/// Atomic typed transformation. Arguments are borrowed; the result is owned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_shared_update_typed(
    shared: *mut FidanValue,
    callback: *mut FidanValue,
    contract: *const u8,
    length: i64,
) -> *mut FidanValue {
    let FidanValue::Shared(shared) = borrow(shared) else {
        return runtime_call_error("type error: expected Shared receiver");
    };
    let descriptor = std::str::from_utf8(std::slice::from_raw_parts(contract, length as usize))
        .expect("compiler contract is UTF-8");
    shared_update(shared, borrow(callback).clone(), Some(descriptor))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_check_argument(
    value: *mut FidanValue,
    contract: *const u8,
    length: i64,
) -> *mut FidanValue {
    let descriptor = std::str::from_utf8(std::slice::from_raw_parts(contract, length as usize))
        .expect("compiler contract is UTF-8");
    match crate::contracts::prepare(borrow(value).clone(), descriptor) {
        Ok(value) => into_raw(value),
        Err(error) => runtime_call_error(error.message),
    }
}

fn dispatch_string_method(s: FidanString, method: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    builtin_value_result(crate::receivers::string_method(s, method, args))
}

// ── List method dispatch ───────────────────────────────────────────────────────

fn dispatch_list_method(
    list: &OwnedRef<FidanList>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    let result = crate::receivers::list_method(list, method, args.clone()).and_then(|value| {
        if value.is_some() {
            return Ok(value);
        }
        crate::receivers::list_callback(list, method, args, |callback, values| {
            // Native entry initialization owns the registered trampoline table;
            // this helper borrows arguments and consumes each owned return once.
            let result = unsafe { call_dynamic_owned(callback, values) };
            if fdn_has_exception() != 0 {
                return Err(stdlib::StdlibRuntimeError::new(
                    fidan_diagnostics::diag_code!("R0001"),
                    "list callback failed",
                ));
            }
            Ok(result)
        })
    });
    // Do not overwrite the callback's original typed exception.
    if fdn_has_exception() != 0 {
        return into_raw(FidanValue::Nothing);
    }
    builtin_value_result(result.and_then(|value| {
        value.ok_or_else(|| {
            stdlib::StdlibRuntimeError::new(
                fidan_diagnostics::diag_code!("R0001"),
                format!("no method `{method}` found for `list`"),
            )
        })
    }))
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
        return unsafe { runtime_call_error(format!("no method `{method}` found for `dict`")) };
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
            let mut list = FidanList::with_capacity(b.len());
            for (key, _) in b.iter() {
                list.append(key.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Values => {
            let b = dict.borrow();
            let mut list = FidanList::with_capacity(b.len());
            for (_, val) in b.iter() {
                list.append(val.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Entries => {
            let b = dict.borrow();
            let mut list = FidanList::with_capacity(b.len());
            for (k, v) in b.iter() {
                let mut pair = FidanList::with_capacity(2);
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
        return unsafe { runtime_call_error(format!("no method `{method}` found for `hashset`")) };
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
    match crate::range_method(start, end, inclusive, method, _args) {
        Ok(Some(value)) => into_raw(value),
        Err(message) => builtin_value_result(Err(stdlib::StdlibRuntimeError::new(
            fidan_diagnostics::diag_code!("R2002"),
            message,
        ))),
        Ok(None) => unsafe { runtime_call_error(format!("range method not found: .{method}()")) },
    }
}
// ── Value comparison helpers ───────────────────────────────────────────────────

/// Exact recursive value equality used by collection search.
pub fn values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    values_equal_with(a, b, |x, y| x == y)
}

/// Test assertions supply their float comparator without changing language equality.
pub fn values_equal_with(
    a: &FidanValue,
    b: &FidanValue,
    float_equal: fn(f64, f64) -> bool,
) -> bool {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x == y,
        (FidanValue::Float(x), FidanValue::Float(y)) => float_equal(*x, *y),
        (FidanValue::Integer(x), FidanValue::Float(y)) => float_equal(*x as f64, *y),
        (FidanValue::Float(x), FidanValue::Integer(y)) => float_equal(*x, *y as f64),
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
                    .all(|(left, right)| values_equal_with(left, right, float_equal))
        }
        (FidanValue::Tuple(lhs), FidanValue::Tuple(rhs)) => {
            lhs.len() == rhs.len()
                && lhs
                    .iter()
                    .zip(rhs.iter())
                    .all(|(left, right)| values_equal_with(left, right, float_equal))
        }
        (FidanValue::Dict(lhs), FidanValue::Dict(rhs)) => {
            if lhs.borrow().aot_class_name().is_some() || rhs.borrow().aot_class_name().is_some() {
                return lhs.identity() == rhs.identity();
            }
            let lhs = lhs.borrow();
            let rhs = rhs.borrow();
            lhs.len() == rhs.len()
                && lhs.iter().all(|(key, left)| {
                    rhs.get(key)
                        .ok()
                        .flatten()
                        .is_some_and(|right| values_equal_with(left, right, float_equal))
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
                    .all(|(left, right)| values_equal_with(left, right, float_equal))
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
                && pa
                    .iter()
                    .zip(pb.iter())
                    .all(|(a, b)| values_equal_with(a, b, float_equal))
        }
        _ => false,
    }
}

pub(crate) fn compare_values(a: &FidanValue, b: &FidanValue) -> std::cmp::Ordering {
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
                let pointers: Vec<_> = args
                    .iter()
                    .map(|value| std::ptr::from_ref(value).cast_mut())
                    .collect();
                unsafe {
                    fdn_print_many(pointers.as_ptr(), pointers.len() as i64);
                }
                Some(into_raw(FidanValue::Nothing))
            }
            BuiltinSemantic::Eprint => {
                let mut stderr = std::io::stderr().lock();
                for (index, value) in args.iter().enumerate() {
                    if index > 0 {
                        let _ = stderr.write_all(b" ");
                    }
                    let _ = crate::value::write_display_io(&mut stderr, value);
                }
                let _ = stderr.write_all(b"\n");
                Some(into_raw(FidanValue::Nothing))
            }
            BuiltinSemantic::Input => {
                let prompt = args.first().map_or(std::ptr::null_mut(), |value| {
                    std::ptr::from_ref(value).cast_mut()
                });
                Some(unsafe { fdn_input(prompt) })
            }
            BuiltinSemantic::String => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::String(FidanString::new(&display(
                    &value,
                )))))
            }
            BuiltinSemantic::Integer => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(builtin_value_result(crate::builtins::integer(&value)))
            }
            BuiltinSemantic::Float => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(builtin_value_result(crate::builtins::float(&value)))
            }
            BuiltinSemantic::Boolean => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::Boolean(value.truthy())))
            }
            BuiltinSemantic::Len => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(builtin_value_result(
                    crate::builtins::len(&value).map(FidanValue::Integer),
                ))
            }
            BuiltinSemantic::Type => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::String(FidanString::new(
                    native_type_name(&value),
                ))))
            }
            BuiltinSemantic::HashSetConstructor => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(builtin_value_result(crate::builtins::hashset(value)))
            }
            BuiltinSemantic::SharedConstructor => {
                let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(into_raw(FidanValue::Shared(SharedRef::from_value(inner))))
            }
            BuiltinSemantic::WeakSharedConstructor => {
                let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                Some(builtin_value_result(crate::builtins::weak_shared(value)))
            }
            BuiltinSemantic::Assert => {
                let condition = args[0].truthy();
                let mut message = args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| FidanValue::String(FidanString::new("assertion failed")));
                unsafe {
                    fdn_assert(i64::from(condition), &mut message);
                }
                Some(into_raw(FidanValue::Nothing))
            }
            BuiltinSemantic::AssertEq | BuiltinSemantic::AssertNe => {
                let lhs = std::ptr::from_ref(&args[0]).cast_mut();
                let rhs = std::ptr::from_ref(&args[1]).cast_mut();
                let message = args.get(2).map_or(std::ptr::null_mut(), |value| {
                    std::ptr::from_ref(value).cast_mut()
                });
                unsafe {
                    if semantic == BuiltinSemantic::AssertEq {
                        fdn_assert_eq_with_message(lhs, rhs, message);
                    } else {
                        fdn_assert_ne_with_message(lhs, rhs, message);
                    }
                }
                Some(into_raw(FidanValue::Nothing))
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
    // Preserve None for non-stdlib namespaces; all known callable dispatches
    // share the same contract, including direct calls and Shared callbacks.
    if (module == fidan_config::BUILTIN_VALUE_MODULE
        || !fidan_config::stdlib::module_members(module).is_empty())
        && let Err(error) = stdlib::validate_callable_arity(module, func, args.len())
    {
        return Some(unsafe { runtime_call_error(error.message) });
    }
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
    match stdlib::math::dispatch_result(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(error)) => {
            checked_integer_result(Err(error));
            into_raw(FidanValue::Nothing)
        }
        None => into_raw(FidanValue::Nothing),
    }
}

// ── string module (free-function API) ─────────────────────────────────────────

fn dispatch_string_fn(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    stdlib::string::dispatch(func, args)
        .map(into_raw)
        .unwrap_or_else(|| into_raw(FidanValue::Nothing))
}

fn builtin_value_result(result: Result<FidanValue, stdlib::StdlibRuntimeError>) -> *mut FidanValue {
    match result {
        Ok(value) => into_raw(value),
        Err(error) => {
            checked_integer_result(Err(error));
            into_raw(FidanValue::Nothing)
        }
    }
}

fn runtime_internal_error(message: String) -> *mut FidanValue {
    store_exception(crate::exception::RuntimeException::internal(
        FidanValue::String(FidanString::new(&message)),
    ));
    into_raw(FidanValue::Nothing)
}

unsafe fn runtime_call_error(message: impl Into<String>) -> *mut FidanValue {
    store_runtime_error(fidan_diagnostics::diag_code!("R0001"), message.into());
    into_raw(FidanValue::Nothing)
}

/// Check positional dynamic-call arity before a trampoline reads or discards args.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_validate_call(count: i64, minimum: i64, maximum: i64) -> i8 {
    if count < minimum || count > maximum {
        drop(Box::from_raw(runtime_call_error(format!(
            "callback argument count: expected {minimum}..{maximum}, got {count}"
        ))));
        return 0;
    }
    1
}

/// Prepare one owned callback argument from borrowed input/default values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_prepare_argument(
    argument: *mut FidanValue,
    default: *mut FidanValue,
    certain: i8,
) -> *mut FidanValue {
    let value = if argument.is_null() || matches!(borrow(argument), FidanValue::Nothing) {
        borrow(default)
    } else {
        borrow(argument)
    };
    if certain != 0 && matches!(value, FidanValue::Nothing) {
        return runtime_call_error("certain callback parameter cannot be nothing");
    }
    into_raw(value.clone())
}

// ── io module ─────────────────────────────────────────────────────────────────
fn dispatch_io(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::io::dispatch_result(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(err)) => {
            store_runtime_error(err.code, err.message);
            into_raw(FidanValue::Nothing)
        }
        None => into_raw(FidanValue::Nothing),
    }
}

// ── json module ───────────────────────────────────────────────────────────────
fn dispatch_json(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::json::dispatch_result(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(err)) => {
            store_runtime_error(err.code, err.message);
            into_raw(FidanValue::Nothing)
        }
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
    let op = match stdlib::parallel::dispatch_op(func, args) {
        Some(Ok(op)) => op,
        Some(Err(error)) => return builtin_value_result(Err(error)),
        None => {
            return runtime_internal_error(format!(
                "unknown native std.parallel callable `{func}`"
            ));
        }
    };
    match stdlib::parallel::execute(op, |callback, args| {
        let value = unsafe { call_dynamic_owned(callback, args) };
        if fdn_has_exception() != 0 {
            Err(())
        } else {
            Ok(value)
        }
    }) {
        Ok(value) => into_raw(value),
        Err(()) => into_raw(FidanValue::Nothing), // Original exception stays pending.
    }
}

// ── test module ───────────────────────────────────────────────────────────────
fn dispatch_test(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match stdlib::test_runner::dispatch(func, args) {
        Some(Ok(value)) => into_raw(value),
        Some(Err(msg)) => {
            let msg_val = into_raw(FidanValue::String(FidanString::new(&format!(
                "assertion failed: {msg}"
            ))));
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
                let captures = ParallelArgs::new(&values);
                let pending = FidanPending::defer_exception(captures, |bundle| {
                    let mut out = FidanList::new();
                    for value in bundle.into_vec() {
                        out.append(resolve_async_value_owned(value)?);
                    }
                    Ok(FidanValue::List(OwnedRef::new(out)))
                });
                into_raw(FidanValue::Pending(pending))
            }
            stdlib::async_std::AsyncOp::WaitAny { values } => {
                let captures = ParallelArgs::new(&values);
                let pending = FidanPending::defer_exception(captures, |bundle| {
                    let values = bundle.into_vec();
                    if values.is_empty() {
                        return Ok(async_wait_any_result(-1, FidanValue::Nothing));
                    }
                    loop {
                        for (index, value) in values.iter().enumerate() {
                            if let Some(result) = try_take_async_value_ready(value) {
                                return result
                                    .map(|resolved| async_wait_any_result(index as i64, resolved));
                            }
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                });
                into_raw(FidanValue::Pending(pending))
            }
            stdlib::async_std::AsyncOp::Timeout { handle, ms } => {
                let pending =
                    FidanPending::defer_exception(ParallelArgs::new(&[handle]), move |bundle| {
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
                    });
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
        let environment: Vec<FidanValue> =
            env_slice.iter().map(|ptr| borrow(*ptr).clone()).collect();
        let first_exception: std::sync::Arc<
            std::sync::Mutex<Option<crate::exception::CapturedException>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));

        std::thread::scope(|scope| {
            for item in items {
                let mut values = vec![item];
                values.extend(environment.iter().cloned());
                let (worker_globals, payload) = capture_aot_worker_payload(values);
                let err_slot = std::sync::Arc::clone(&first_exception);
                scope.spawn(move || {
                    let result =
                        call_trampoline_owned(fn_idx as usize, worker_globals.install(payload));
                    if fdn_has_exception() != 0 {
                        let exn_cap = take_exception()
                            .expect("worker pending exception")
                            .capture();
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
            store_exception(exn_cap.restore());
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

    let values = args_slice
        .iter()
        .map(|ptr| borrow(*ptr).clone())
        .collect::<Vec<_>>();
    ParallelArgs::new(&values)
}

unsafe fn build_aot_worker_payload_from_ptrs(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> (AotWorkerGlobals, ParallelArgs) {
    let args_slice: &[*mut FidanValue] = if args_cnt > 0 && !args_ptr.is_null() {
        std::slice::from_raw_parts(args_ptr, args_cnt as usize)
    } else {
        &[]
    };
    capture_aot_worker_payload(args_slice.iter().map(|ptr| borrow(*ptr).clone()))
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
    let pending = FidanPending::defer_exception(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, bundle.into_vec());
        if fdn_has_exception() != 0 {
            Err(take_exception().expect("task pending exception").capture())
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
    let (worker_globals, args) = build_aot_worker_payload_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::spawn_exception(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, worker_globals.install(bundle));
        if fdn_has_exception() != 0 {
            Err(take_exception()
                .expect("task pending exception")
                .capture()
                .with_context(&format!("task `{task_name}` failed")))
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_concurrent(
    fn_idx: i64,
    name_bytes: *const u8,
    name_len: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let task_name = str_from_raw(name_bytes, name_len);
    let args = build_parallel_args_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::defer_exception(args, move |bundle: ParallelArgs| {
        let result = call_trampoline_owned(fn_idx as usize, bundle.into_vec());
        if fdn_has_exception() != 0 {
            Err(take_exception()
                .expect("task pending exception")
                .capture()
                .with_context(&format!("task `{task_name}` failed")))
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
    let mut values = vec![borrow(function_or_receiver).clone()];
    values.extend((0..args_cnt as usize).map(|i| borrow(*args_ptr.add(i)).clone()));
    let args = ParallelArgs::new(&values);
    let method_name =
        (!method_bytes.is_null() && method_len > 0).then(|| str_from_raw(method_bytes, method_len));
    let pending = FidanPending::defer_exception(args, move |bundle: ParallelArgs| {
        let mut values = bundle.into_vec();
        let first = values.remove(0);
        let result = if let Some(ref method_name) = method_name {
            call_method_owned(first, method_name, values)
        } else {
            call_dynamic_owned(first, values)
        };
        if fdn_has_exception() != 0 {
            Err(take_exception().expect("task pending exception").capture())
        } else {
            Ok(result)
        }
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_pending_join(handle: *mut FidanValue) -> *mut FidanValue {
    match borrow(handle) {
        FidanValue::Pending(pending) => match pending.try_join_exception() {
            Ok(value) => into_raw(value),
            Err(exception) => {
                store_exception(exception.restore());
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
    let part_count = count as usize;
    let mut result = if part_count <= 4 {
        String::with_capacity(part_count.saturating_mul(16))
    } else {
        let mut capacity = 0usize;
        for i in 0..part_count {
            let p = *parts_ptr.add(i);
            capacity = capacity.saturating_add(display_len_hint(borrow(p)));
        }
        String::with_capacity(capacity)
    };
    for i in 0..part_count {
        let p = *parts_ptr.add(i);
        crate::value::display_into(&mut result, borrow(p));
    }
    into_raw(FidanValue::String(FidanString::new(&result)))
}

// ── Exception handling ─────────────────────────────────────────────────────────

thread_local! {
    static EXCEPTION_VALUE: std::cell::RefCell<Option<crate::exception::RuntimeException>> =
        const { std::cell::RefCell::new(None) };
}

pub fn take_exception() -> Option<crate::exception::RuntimeException> {
    EXCEPTION_VALUE.with(|slot| slot.borrow_mut().take())
}

fn store_exception(exception: crate::exception::RuntimeException) {
    // Replacing a pending exception drops its previous owned payload.
    EXCEPTION_VALUE.with(|slot| *slot.borrow_mut() = Some(exception));
}

fn store_runtime_error(code: fidan_diagnostics::DiagCode, message: String) {
    store_exception(crate::exception::RuntimeException::runtime(code, message));
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_push_catch(_catch_id: i64) {}
#[unsafe(no_mangle)]
pub extern "C" fn fdn_pop_catch() {}
#[unsafe(no_mangle)]
pub extern "C" fn fdn_has_exception() -> i8 {
    EXCEPTION_VALUE.with(|slot| i8::from(slot.borrow().is_some()))
}

/// Store an internal failure. Language `panic` uses fdn_store_panic instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_store_exception(val: *mut FidanValue) {
    store_exception(crate::exception::RuntimeException::internal(
        borrow(val).clone(),
    ));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_store_panic(val: *mut FidanValue) {
    store_exception(crate::exception::RuntimeException {
        value: borrow(val).clone(),
        origin: crate::exception::ExceptionOrigin::UserPanic,
    });
}

/// The executable's uncaught boundary retains origin instead of inspecting text.
#[unsafe(no_mangle)]
pub extern "C" fn fdn_report_unhandled_exception() -> ! {
    let exception = take_exception().unwrap_or_else(|| {
        crate::exception::RuntimeException::internal(FidanValue::String(FidanString::new(
            "missing pending exception at uncaught boundary",
        )))
    });
    let (code, message) = exception.diagnostic();
    fidan_diagnostics::render_message_to_stderr(fidan_diagnostics::Severity::Error, code, &message);
    std::process::exit(1);
}

/// Legacy user-throw ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_throw(val: *mut FidanValue) -> ! {
    fdn_store_panic(val);
    fdn_report_unhandled_exception();
}

/// Fatal assertion ABI; assertions remain non-catchable and are not user panics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_throw_unhandled(val: *mut FidanValue) -> ! {
    let exception = crate::exception::RuntimeException::internal(borrow(val).clone());
    let (code, message) = exception.diagnostic();
    fidan_diagnostics::render_message_to_stderr(fidan_diagnostics::Severity::Error, code, &message);
    std::process::exit(1);
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_catch_exception() -> *mut FidanValue {
    into_raw(
        take_exception()
            .map(|exception| exception.value)
            .unwrap_or(FidanValue::Nothing),
    )
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

#[derive(Clone, Default)]
struct RegisteredFunction {
    trampoline: Option<FnTrampoline>,
    metadata: Option<serde_json::Value>,
}

static FN_TABLE: std::sync::OnceLock<std::sync::Mutex<Vec<RegisteredFunction>>> =
    std::sync::OnceLock::new();

enum RegisteredNamespaceMember {
    Function(usize),
    Namespace(Arc<str>),
}

/// Qualified namespace member names, populated by AOT-generated startup code.
static FN_NAME_TABLE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, RegisteredNamespaceMember>>,
> = std::sync::OnceLock::new();

/// Register a function name → table index mapping (called from AOT startup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_fn_name_register(name_bytes: *const u8, name_len: i64, idx: i64) {
    let name = unsafe { str_from_raw(name_bytes, name_len) }.to_string();
    let table =
        FN_NAME_TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut guard) = table.lock() {
        guard.insert(name, RegisteredNamespaceMember::Function(idx as usize));
    }
}

/// Populate metadata in the existing function table, before user code executes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_fn_metadata_register(bytes: *const u8, length: i64, idx: i64) {
    let descriptor = str_from_raw(bytes, length);
    let parsed = serde_json::from_str::<serde_json::Value>(&descriptor);
    if let Ok(metadata) = parsed
        && metadata["name"].is_string()
        && metadata["parameters"].is_array()
        && let Some(table) = FN_TABLE.get()
        && let Ok(mut guard) = table.lock()
        && let Some(function) = usize::try_from(idx)
            .ok()
            .and_then(|index| guard.get_mut(index))
    {
        function.metadata = Some(metadata);
    } else {
        drop(Box::from_raw(runtime_call_error(
            "invalid compiler callable registration",
        )));
    }
}

fn registered_callable_metadata(
    value: &FidanValue,
) -> Result<(serde_json::Value, usize), stdlib::StdlibRuntimeError> {
    let (id, captures) = match value {
        FidanValue::Function(FunctionId(id)) => (*id, 0),
        FidanValue::Closure {
            fn_id: FunctionId(id),
            captured,
        } => (*id, captured.len()),
        FidanValue::StdlibFn(module, name) => {
            return crate::callable::stdlib_metadata(module, name).map(|metadata| (metadata, 0));
        }
        _ => {
            return Err(stdlib::StdlibRuntimeError::new(
                fidan_diagnostics::diag_code!("R0001"),
                "expected a callable value",
            ));
        }
    };
    FN_TABLE
        .get()
        .and_then(|table| table.lock().ok())
        .and_then(|guard| {
            guard
                .get(id as usize)
                .and_then(|function| function.metadata.clone())
        })
        .map(|metadata| (metadata, captures))
        .ok_or_else(|| {
            stdlib::StdlibRuntimeError::new(
                fidan_diagnostics::diag_code!("R0001"),
                "callable metadata is not registered",
            )
        })
}

/// Register a resolved re-export in the same namespace table as user functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_namespace_export_register(
    alias_bytes: *const u8,
    alias_len: i64,
    target_bytes: *const u8,
    target_len: i64,
) {
    let alias = str_from_raw(alias_bytes, alias_len);
    let target = Arc::from(str_from_raw(target_bytes, target_len));
    let table =
        FN_NAME_TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut guard) = table.lock() {
        guard.insert(alias, RegisteredNamespaceMember::Namespace(target));
    }
}

/// Allocate the global function table with `count` slots (all initialised to None).
/// Called once from the generated `fdn_init` before any user code runs.
#[unsafe(no_mangle)]
pub extern "C" fn fdn_fn_table_init(count: i64) {
    let _ = FN_TABLE.set(std::sync::Mutex::new(vec![
        RegisteredFunction::default();
        count as usize
    ]));
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
            guard[i].trampoline = Some(unsafe { std::mem::transmute::<usize, FnTrampoline>(ptr) });
        }
    }
}

/// Call a trampoline by its table index, with the given argument pointers (borrowed).
unsafe fn call_trampoline_by_idx(idx: usize, arg_ptrs: &[*mut FidanValue]) -> *mut FidanValue {
    let trampoline = FN_TABLE
        .get()
        .and_then(|t| t.lock().ok())
        .and_then(|g| g.get(idx).and_then(|function| function.trampoline));
    match trampoline {
        Some(t) => t(arg_ptrs.as_ptr(), arg_ptrs.len() as i64),
        None => runtime_internal_error(format!(
            "native function table has no trampoline for index {idx}"
        )),
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
            runtime_internal_error(format!(
                "native dynamic call references unknown stdlib module `{module}`"
            ))
        });
    }

    let (fn_id, captured): (u32, &[FidanValue]) = match fv {
        FidanValue::Function(FunctionId(id)) => (*id, &[]),
        FidanValue::Closure {
            fn_id: FunctionId(id),
            captured,
        } => (*id, captured.as_slice()),
        _ => {
            return runtime_call_error(format!("cannot call value of type `{}`", fv.type_name()));
        }
    };

    let trampoline = FN_TABLE.get().and_then(|t| t.lock().ok()).and_then(|g| {
        g.get(fn_id as usize)
            .and_then(|function| function.trampoline)
    });

    let Some(trampoline) = trampoline else {
        return runtime_call_error(format!("no callback trampoline for function {fn_id}"));
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
    #[test]
    fn enum_payload_constructor_uses_native_method_dispatch() {
        for name in ["Ok", "toString"] {
            let result = unsafe {
                call_method_owned(
                    FidanValue::EnumType(Arc::from("Result")),
                    name,
                    vec![FidanValue::Integer(3)],
                )
            };
            let failed = fdn_has_exception() != 0;
            if failed {
                unsafe {
                    fdn_drop(fdn_catch_exception());
                }
            }
            assert!(
                !failed,
                "enum construction must not raise a missing-method error"
            );
            assert!(
                matches!(result, FidanValue::EnumVariant { tag, payload } if tag.as_ref() == name && matches!(payload.as_slice(), [FidanValue::Integer(3)])),
                "variant {name} must take precedence over conversion methods"
            );
        }
    }
    #[test]
    fn native_workers_capture_global_and_argument_aliases_as_one_graph() {
        let mut slot = FidanValue::Nothing;
        let slot_address = (&mut slot as *mut FidanValue) as usize;
        let source = OwnedRef::new(FidanList::from_vec(vec![FidanValue::Integer(1)]));
        let source_identity = source.identity();
        let value = into_raw(FidanValue::List(source.clone()));
        unsafe {
            fdn_global_store(slot_address as *mut FidanValue, value);
            fdn_drop(value);
        }
        let (globals, payload) = capture_aot_worker_payload([FidanValue::List(source.clone())]);
        std::thread::spawn(move || {
            let args = globals.install(payload);
            let global = unsafe { Box::from_raw(fdn_global_load(slot_address as *mut FidanValue)) };
            let FidanValue::List(global_list) = &*global else {
                panic!("list global")
            };
            let FidanValue::List(argument_list) = &args[0] else {
                panic!("list argument")
            };
            assert_eq!(
                global_list.identity(),
                argument_list.identity(),
                "worker aliases must survive"
            );
            assert_ne!(
                global_list.identity(),
                source_identity,
                "worker must not retain parent Rc"
            );
            argument_list.borrow_mut().append(FidanValue::Integer(2));
            assert_eq!(global_list.borrow().len(), 2);
        })
        .join()
        .unwrap();
        assert_eq!(source.borrow().len(), 1);
        let global = unsafe { Box::from_raw(fdn_global_load(slot_address as *mut FidanValue)) };
        let FidanValue::List(parent) = &*global else {
            panic!("parent list")
        };
        assert_eq!(parent.borrow().len(), 1);
        AOT_GLOBALS.with(|globals| globals.borrow_mut().remove(&slot_address));
    }

    #[test]
    fn ordinary_dict_class_key_cannot_forge_object_identity() {
        let mut dict = FidanDict::new();
        dict.insert(
            FidanValue::String(FidanString::new("__class__")),
            FidanValue::String(FidanString::new("UserData")),
        )
        .unwrap();
        let value = FidanValue::Dict(OwnedRef::new(dict));
        assert_eq!(native_type_name(&value), "dict");
        assert_eq!(crate::display(&value), "{__class__: UserData}");
        assert!(crate::contracts::prepare(value, r#"{"Object":0}"#).is_err());
        unsafe {
            let object = fdn_obj_new(b"Actual".as_ptr(), 6);
            assert_eq!(native_type_name(borrow(object)), "object");
            assert_eq!(crate::display(borrow(object)), "<Actual>");
            assert!(crate::contracts::prepare(borrow(object).clone(), r#"{"Object":0}"#).is_ok());
            fdn_drop(object);
        }
    }

    use super::*;

    #[test]
    fn dynamic_arithmetic_reports_checked_errors() {
        type Binary = unsafe extern "C" fn(*mut FidanValue, *mut FidanValue) -> *mut FidanValue;
        let cases: [(Binary, i64, i64, &str); 9] = [
            (fdn_dyn_add, i64::MAX, 1, "R2003"),
            (fdn_dyn_sub, i64::MIN, 1, "R2003"),
            (fdn_dyn_mul, i64::MAX, 2, "R2003"),
            (fdn_dyn_div, i64::MIN, -1, "R2003"),
            (fdn_dyn_rem, i64::MIN, -1, "R2003"),
            (fdn_dyn_div, 10, 0, "R2001"),
            (fdn_dyn_rem, 10, 0, "R2001"),
            (fdn_dyn_pow, 2, 63, "R2003"),
            (fdn_dyn_pow, 0, -1, "R2001"),
        ];
        unsafe {
            for (function, a, b, code) in cases {
                let a = into_raw(FidanValue::Integer(a));
                let b = into_raw(FidanValue::Integer(b));
                let result = function(a, b);
                assert!(display(&drain_exception().expect("runtime diagnostic")).contains(code));
                for value in [a, b, result] {
                    fdn_drop(value);
                }
            }
            let a = into_raw(FidanValue::Integer(i64::MIN));
            let result = fdn_dyn_neg(a);
            assert!(display(&drain_exception().expect("negation overflow")).contains("R2003"));
            fdn_drop(a);
            fdn_drop(result);
            for function in [fdn_dyn_div as Binary, fdn_dyn_rem as Binary] {
                let a = into_raw(FidanValue::Float(1.0));
                let b = into_raw(FidanValue::Float(-0.0));
                let result = function(a, b);
                assert!(display(&drain_exception().expect("float zero divisor")).contains("R2001"));
                for value in [a, b, result] {
                    fdn_drop(value);
                }
            }
        }
    }

    #[test]
    fn native_indexing_preserves_nothing_elements_and_reports_invalid_indices() {
        unsafe {
            let mut values = FidanList::new();
            values.append(FidanValue::Nothing);
            let list = into_raw(FidanValue::List(OwnedRef::new(values)));
            let zero = into_raw(FidanValue::Integer(0));
            let result = fdn_list_get(list, zero);
            assert!(matches!(borrow(result), FidanValue::Nothing));
            assert!(drain_exception().is_none());
            drop(Box::from_raw(result));
            let invalid = into_raw(FidanValue::Integer(-2));
            let result = fdn_list_get(list, invalid);
            assert!(matches!(borrow(result), FidanValue::Nothing));
            assert!(display(&drain_exception().expect("index error")).contains("R2002"));
            fdn_list_set(list, invalid, zero);
            assert!(display(&drain_exception().expect("index assignment error")).contains("R2002"));
            assert_eq!(display(borrow(list)), "[nothing]");
            for value in [list, zero, invalid, result] {
                drop(Box::from_raw(value));
            }
        }
    }

    #[test]
    fn native_range_contains_and_inclusive_materialization() {
        for (value, expected) in [(4, true), (5, false), (0, false)] {
            let result =
                dispatch_range_method(1, 5, false, "contains", vec![FidanValue::Integer(value)]);
            assert!(
                matches!(unsafe { borrow(result) }, FidanValue::Boolean(found) if *found == expected)
            );
            unsafe {
                drop(Box::from_raw(result));
            }
        }
        let result = dispatch_range_method(i64::MAX - 1, i64::MAX, true, "to_list", vec![]);
        assert_eq!(
            display(unsafe { borrow(result) }),
            format!("[{}, {}]", i64::MAX - 1, i64::MAX)
        );
        unsafe {
            drop(Box::from_raw(result));
        }
    }

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
    #[test]
    fn strict_operator_and_builtin_errors_use_exception_slot() {
        unsafe {
            let text = into_raw(FidanValue::String(FidanString::new("abc")));
            let integer = into_raw(FidanValue::Integer(42));
            let float = into_raw(FidanValue::Float(1.5));
            for operation in 0..4 {
                let result = match operation {
                    0 => fdn_dyn_bit_and(text, integer),
                    1 => fdn_to_integer(text),
                    2 => fdn_to_float(text),
                    _ => fdn_make_range_checked(float, integer, 0),
                };
                assert!(matches!(borrow(result), FidanValue::Nothing));
                let error = drain_exception().expect("stored R0001");
                assert!(display(&error).contains("R0001"));
                drop(Box::from_raw(result));
            }
            // Check each operation separately: the slot holds one pending error.
            for helper in [fdn_dyn_bit_or, fdn_dyn_bit_xor, fdn_dyn_shl, fdn_dyn_shr] {
                let result = helper(text, integer);
                assert!(
                    display(&drain_exception().expect("stored bitwise error")).contains("R0001")
                );
                drop(Box::from_raw(result));
            }
            assert_eq!(fdn_len(integer), 0);
            assert!(display(&drain_exception().expect("stored length error")).contains("R0001"));
            let range = fdn_make_range(i64::MIN, i64::MAX, 1);
            assert_eq!(fdn_len(range), 0);
            assert!(display(&drain_exception().expect("range length error")).contains("R0001"));
            let result = dispatch_range_method(i64::MIN, i64::MAX, true, "len", vec![]);
            assert!(display(&drain_exception().expect("range receiver error")).contains("R2002"));
            for ptr in [text, integer, float, range, result] {
                drop(Box::from_raw(ptr));
            }
        }
    }
    #[test]
    fn constructor_dispatch_stores_catchable_errors() {
        for name in ["hashset", "WeakShared"] {
            let result =
                dispatch_stdlib_inline("__builtin__", name, vec![FidanValue::Integer(42)]).unwrap();
            assert!(matches!(unsafe { borrow(result) }, FidanValue::Nothing));
            assert!(
                display(&drain_exception().expect("R0001 constructor error")).contains("R0001")
            );
            unsafe {
                drop(Box::from_raw(result));
            }
        }
    }
}

#[cfg(test)]
mod exception_origin_tests {
    use super::*;

    #[test]
    fn caught_user_panic_returns_raw_payload_and_clears_slot() {
        unsafe {
            let value = into_raw(FidanValue::Integer(42));
            fdn_store_panic(value);
            fdn_drop(value);
            assert_eq!(fdn_has_exception(), 1);
            let caught = *Box::from_raw(fdn_catch_exception());
            assert!(matches!(caught, FidanValue::Integer(42)));
            assert_eq!(fdn_has_exception(), 0);
        }
    }

    #[test]
    fn native_await_preserves_panic_payload_and_runtime_error_codes() {
        for origin in [
            crate::exception::ExceptionOrigin::UserPanic,
            crate::exception::ExceptionOrigin::Internal,
        ] {
            let pending = FidanPending::defer_exception(ParallelArgs::new(&[]), move |_| {
                Err(crate::exception::RuntimeException {
                    value: FidanValue::Integer(7),
                    origin,
                }
                .capture())
            });
            unsafe {
                let handle = into_raw(FidanValue::Pending(pending));
                fdn_drop(fdn_pending_join(handle));
                fdn_drop(handle);
                let exception = take_exception().expect("await must propagate failure");
                assert!(matches!(exception.value, FidanValue::Integer(7)));
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_enter_call() -> i8 {
    match crate::exception::enter_call() {
        Ok(()) => 1,
        Err(error) => {
            store_runtime_error(error.code, error.message);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_leave_call() {
    crate::exception::leave_call();
}

/// Join a structured group completely. Input array entries are borrowed handles;
/// each output entry is replaced by an owned result for generated local storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_join_all(handles: *mut *mut FidanValue, count: i64) {
    let mut failures = Vec::new();
    for index in 0..count as usize {
        let slot = handles.add(index);
        let result = match borrow(*slot) {
            FidanValue::Pending(pending) => pending.try_join_exception(),
            value => Ok(value.clone()),
        };
        *slot = into_raw(match result {
            Ok(value) => value,
            Err(exception) => {
                failures.push(exception.to_string());
                FidanValue::Nothing
            }
        });
    }
    if !failures.is_empty() {
        let count = failures.len();
        store_runtime_error(
            fidan_diagnostics::diag_code!("R9001"),
            format!(
                "{count} task{} failed in task block:\n{}",
                if count == 1 { "" } else { "s" },
                failures.join("\n")
            ),
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fdn_exception_is_catchable() -> i8 {
    EXCEPTION_VALUE.with(|slot| {
        i8::from(
            slot.borrow()
                .as_ref()
                .is_some_and(crate::exception::RuntimeException::is_catchable),
        )
    })
}

#[cfg(test)]
mod internal_failure_tests {
    use super::*;

    #[test]
    fn missing_native_entry_is_an_internal_error_not_a_user_panic() {
        unsafe {
            fdn_drop(call_trampoline_by_idx(usize::MAX, &[]));
        }
        let error = take_exception().expect("missing entry must not silently return nothing");
        assert_eq!(error.diagnostic().0, fidan_diagnostics::diag_code!("R0001"));
        assert!(!error.is_catchable());
    }

    #[test]
    fn host_task_panic_is_not_misclassified_as_a_language_panic() {
        let pending = FidanPending::spawn_fallible(ParallelArgs::new(&[]), |_| {
            panic!("synthetic host failure")
        });
        let error = pending.try_join_diagnostic().unwrap_err();
        assert_eq!(error.diagnostic().0, fidan_diagnostics::diag_code!("R0001"));
        assert!(!error.is_catchable());
    }
}
