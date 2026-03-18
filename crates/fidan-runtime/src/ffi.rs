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
    FidanDict, FidanList, FidanString, OwnedRef, SharedRef,
    parallel::{FidanPending, ParallelArgs, ParallelCapture},
    value::{FidanValue, FunctionId, display},
};
use std::sync::{Arc, LazyLock};

use dashmap::DashMap;
use regex::Regex;

/// Process-wide cache: pattern string → compiled `Regex`.
static REGEX_CACHE: LazyLock<DashMap<String, Arc<Regex>>> = LazyLock::new(DashMap::new);

fn compile_regex(pattern: &str) -> Option<Arc<Regex>> {
    if let Some(cached) = REGEX_CACHE.get(pattern) {
        return Some(Arc::clone(&*cached));
    }
    match Regex::new(pattern) {
        Ok(re) => {
            let arc = Arc::new(re);
            REGEX_CACHE.insert(pattern.to_string(), Arc::clone(&arc));
            Some(arc)
        }
        Err(_) => None,
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Borrow a raw pointer as `&FidanValue` without taking ownership.
#[inline(always)]
unsafe fn borrow<'a>(ptr: *mut FidanValue) -> &'a FidanValue {
    debug_assert!(!ptr.is_null(), "fdn_*: null ptr");
    &*ptr
}

/// Allocate a new owned `FidanValue` and return its raw pointer.
#[inline(always)]
fn into_raw(v: FidanValue) -> *mut FidanValue {
    Box::into_raw(Box::new(v))
}

/// Coerce to `FidanString` if possible, otherwise stringify.
#[inline]
fn as_fidan_string(v: &FidanValue) -> FidanString {
    match v {
        FidanValue::String(s) => s.clone(),
        other => FidanString::new(&display(other)),
    }
}

fn is_truthy(v: &FidanValue) -> bool {
    match v {
        FidanValue::Nothing => false,
        FidanValue::Boolean(b) => *b,
        FidanValue::Integer(n) => *n != 0,
        FidanValue::Float(f) => *f != 0.0,
        FidanValue::String(s) => !s.is_empty(),
        FidanValue::List(l) => !l.borrow().is_empty(),
        FidanValue::Dict(d) => !d.borrow().is_empty(),
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
    into_raw(borrow(ptr).clone())
}

/// Drop: the ONLY function that consumes its argument.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_drop(ptr: *mut FidanValue) {
    debug_assert!(!ptr.is_null());
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
    println!("{}", display(borrow(ptr)));
}

/// Print multiple values space-separated, then newline.
/// `ptrs` is an array of `n` `*mut FidanValue` pointers.  Borrows each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_print_many(ptrs: *const *mut FidanValue, n: i64) {
    let parts: Vec<String> = (0..n as usize)
        .map(|i| display(borrow(*ptrs.add(i))).to_string())
        .collect();
    println!("{}", parts.join(" "));
}

/// Print without newline.  Borrows `ptr`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_print(ptr: *mut FidanValue) {
    print!("{}", display(borrow(ptr)));
}

/// Read a line from stdin, optionally printing a UTF-8 prompt.  Borrows `prompt`.
/// Returns a new owned `String` value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_input(prompt: *mut FidanValue) -> *mut FidanValue {
    let pv = borrow(prompt);
    if !matches!(pv, FidanValue::Nothing) {
        print!("{}", display(pv));
        use std::io::Write;
        let _ = std::io::stdout().flush();
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
pub unsafe extern "C" fn fdn_assert(cond: i8, msg: *mut FidanValue) {
    if cond == 0 {
        eprintln!("assertion failed: {}", display(borrow(msg)));
        std::process::exit(1);
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
        FidanValue::Dict(d) => {
            let key = FidanString::new(&as_str_val(idx_val));
            d.borrow().get(&key).cloned().unwrap_or(FidanValue::Nothing)
        }
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
            let key = FidanString::new(&as_str_val(idx_val));
            d.borrow_mut().insert(key, borrow(val).clone());
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
        let k = as_fidan_string(borrow(key));
        d.borrow().get(&k).cloned().unwrap_or(FidanValue::Nothing)
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
        let k = as_fidan_string(borrow(key));
        d.borrow_mut().insert(k, borrow(val).clone());
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

// ── Object ─────────────────────────────────────────────────────────────────────
// Phase 11.1: objects are backed by FidanDict (string-keyed).

/// Allocate a new empty object.  `class_bytes`/`class_len` are the class name
/// (used for display / future class-table lookup).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_obj_new(class_bytes: *const u8, class_len: i64) -> *mut FidanValue {
    let mut d = FidanDict::new();
    if !class_bytes.is_null() && class_len > 0 {
        let class_name = str_from_raw(class_bytes, class_len);
        d.insert(
            FidanString::new("__class__"),
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
        FidanValue::Dict(d) => {
            let key = FidanString::new(&field_name);
            d.borrow().get(&key).cloned().unwrap_or(FidanValue::Nothing)
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
        let key = FidanString::new(&field_name);
        d.borrow_mut().insert(key, borrow(val).clone());
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
            let method_key = FidanString::new(&format!("__method__{}", method_name));
            if let Some(method_fn) = d.borrow().get(&method_key).cloned() {
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
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => dispatch_range_method(*start, *end, *inclusive, &method_name, extra),
        FidanValue::Shared(sr) => match method_name.as_str() {
            "get" => into_raw(sr.0.lock().unwrap().clone()),
            "set" => {
                let val = extra.into_iter().next().unwrap_or(FidanValue::Nothing);
                *sr.0.lock().unwrap() = val;
                into_raw(FidanValue::Nothing)
            }
            _ => {
                eprintln!(
                    "AOT: method dispatch not implemented: {}.{}()",
                    recv.type_name(),
                    method_name
                );
                into_raw(FidanValue::Nothing)
            }
        },
        _ => {
            eprintln!(
                "AOT: method dispatch not implemented: {}.{}()",
                recv.type_name(),
                method_name
            );
            into_raw(FidanValue::Nothing)
        }
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

fn dispatch_string_method(s: FidanString, method: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    let str_val = s.as_str().to_owned();
    match method {
        // ── Case ──────────────────────────────────────────────────────────────
        "lower" | "toLower" | "to_lower" => into_raw(FidanValue::String(FidanString::new(
            &str_val.to_lowercase(),
        ))),
        "upper" | "toUpper" | "to_upper" => into_raw(FidanValue::String(FidanString::new(
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
        "trimStart" | "ltrim" | "trim_start" => {
            into_raw(FidanValue::String(FidanString::new(str_val.trim_start())))
        }
        "trimEnd" | "rtrim" | "trim_end" => {
            into_raw(FidanValue::String(FidanString::new(str_val.trim_end())))
        }

        // ── Length ────────────────────────────────────────────────────────────
        "len" | "length" => into_raw(FidanValue::Integer(str_val.chars().count() as i64)),
        "byteLen" | "byte_len" => into_raw(FidanValue::Integer(str_val.len() as i64)),
        "isEmpty" | "is_empty" => into_raw(FidanValue::Boolean(str_val.is_empty())),

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
        "startsWith" | "starts_with" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::Boolean(str_val.starts_with(pat.as_str())))
        }
        "endsWith" | "ends_with" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::Boolean(str_val.ends_with(pat.as_str())))
        }
        "indexOf" | "index_of" => {
            let pat = args.first().map(as_str_val).unwrap_or_default();
            match str_val.find(pat.as_str()) {
                Some(i) => into_raw(FidanValue::Integer(i as i64)),
                None => into_raw(FidanValue::Integer(-1)),
            }
        }
        "lastIndexOf" | "last_index_of" => {
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
        "replaceAll" | "replace_all" => {
            let from = args.first().map(as_str_val).unwrap_or_default();
            let to = args.get(1).map(as_str_val).unwrap_or_default();
            into_raw(FidanValue::String(FidanString::new(
                &str_val.replace(from.as_str(), to.as_str()),
            )))
        }
        "replaceFirst" | "replace_first" => {
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
        "charAt" | "char_at" => {
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
        "substring" | "slice" => {
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
        "toInt" | "to_int" | "parseInt" | "parse_int" => match str_val.trim().parse::<i64>() {
            Ok(n) => into_raw(FidanValue::Integer(n)),
            Err(_) => into_raw(FidanValue::Nothing),
        },
        "toFloat" | "to_float" | "parseFloat" | "parse_float" => {
            match str_val.trim().parse::<f64>() {
                Ok(f) => into_raw(FidanValue::Float(f)),
                Err(_) => into_raw(FidanValue::Nothing),
            }
        }
        "toBool" | "to_bool" => {
            let b = matches!(str_val.trim().to_lowercase().as_str(), "true" | "1" | "yes");
            into_raw(FidanValue::Boolean(b))
        }
        "toString" | "to_string" => into_raw(FidanValue::String(FidanString::new(&str_val))),

        // ── Padding ───────────────────────────────────────────────────────────
        "padStart" | "pad_start" => {
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
        "padEnd" | "pad_end" => {
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
        "charCode" | "char_code" => {
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
        _ => {
            eprintln!("AOT: string method not found: .{}()", method);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── List method dispatch ───────────────────────────────────────────────────────

fn dispatch_list_method(
    list: &OwnedRef<FidanList>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    match method {
        "len" | "length" | "size" => into_raw(FidanValue::Integer(list.borrow().len() as i64)),
        "isEmpty" | "is_empty" => into_raw(FidanValue::Boolean(list.borrow().is_empty())),
        "append" | "push" => {
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
        "first" | "head" => into_raw(list.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing)),
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
        "indexOf" | "index_of" => {
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
        "extend" | "concat" => {
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
        "toString" | "to_string" => {
            let b = list.borrow();
            let items: Vec<String> = b.iter().map(display).collect();
            into_raw(FidanValue::String(FidanString::new(&format!(
                "[{}]",
                items.join(", ")
            ))))
        }
        "forEach" | "for_each" | "each" => {
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

        "map" | "transform" | "collect" => {
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

        "filter" | "where_" | "select" => {
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

        "firstWhere" | "first_where" => {
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

        "reduce" | "fold" => {
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

        _ => {
            eprintln!("AOT: list method not found: .{}()", method);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── Dict method dispatch ───────────────────────────────────────────────────────

fn dispatch_dict_method(
    dict: &OwnedRef<FidanDict>,
    method: &str,
    args: Vec<FidanValue>,
) -> *mut FidanValue {
    match method {
        "len" | "length" | "size" => into_raw(FidanValue::Integer(dict.borrow().len() as i64)),
        "isEmpty" | "is_empty" => into_raw(FidanValue::Boolean(dict.borrow().is_empty())),
        "get" => {
            if let Some(key_val) = args.first() {
                let key = FidanString::new(&as_str_val(key_val));
                into_raw(
                    dict.borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "set" | "put" => {
            if let (Some(k), Some(v)) = (args.first(), args.get(1)) {
                let key = FidanString::new(&as_str_val(k));
                dict.borrow_mut().insert(key, v.clone());
            }
            into_raw(FidanValue::Nothing)
        }
        "contains" | "has" | "containsKey" | "contains_key" => {
            if let Some(key_val) = args.first() {
                let key = FidanString::new(&as_str_val(key_val));
                into_raw(FidanValue::Boolean(dict.borrow().get(&key).is_some()))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "remove" | "delete" => {
            if let Some(key_val) = args.first() {
                let key = FidanString::new(&as_str_val(key_val));
                dict.borrow_mut().remove(&key);
            }
            into_raw(FidanValue::Nothing)
        }
        "keys" => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (key, _) in b.iter() {
                list.append(FidanValue::String(key.clone()));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "values" => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (_, val) in b.iter() {
                list.append(val.clone());
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "entries" | "items" => {
            let b = dict.borrow();
            let mut list = FidanList::new();
            for (k, v) in b.iter() {
                let mut pair = FidanList::new();
                pair.append(FidanValue::String(k.clone()));
                pair.append(v.clone());
                list.append(FidanValue::List(OwnedRef::new(pair)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "toString" | "to_string" => into_raw(FidanValue::String(FidanString::new(&display(
            &FidanValue::Dict(dict.clone()),
        )))),
        _ => {
            eprintln!("AOT: dict method not found: .{}()", method);
            into_raw(FidanValue::Nothing)
        }
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
fn dispatch_stdlib_inline(
    module: &str,
    func: &str,
    args: Vec<FidanValue>,
) -> Option<*mut FidanValue> {
    match module {
        "math" => Some(dispatch_math(func, args)),
        "string" => Some(dispatch_string_fn(func, args)),
        "io" => Some(dispatch_io(func, args)),
        "collections" => Some(dispatch_collections(func, args)),
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
    fn num(v: &FidanValue) -> f64 {
        match v {
            FidanValue::Float(f) => *f,
            FidanValue::Integer(n) => *n as f64,
            _ => 0.0,
        }
    }
    fn int_or_float(f: f64) -> FidanValue {
        if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
            FidanValue::Integer(f as i64)
        } else {
            FidanValue::Float(f)
        }
    }
    match func {
        "PI" | "pi" => into_raw(FidanValue::Float(std::f64::consts::PI)),
        "E" | "e" => into_raw(FidanValue::Float(std::f64::consts::E)),
        "TAU" | "tau" => into_raw(FidanValue::Float(std::f64::consts::TAU)),
        "sqrt" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).sqrt(),
        )),
        "cbrt" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).cbrt(),
        )),
        "abs" => match args.first().unwrap_or(&FidanValue::Nothing) {
            FidanValue::Integer(n) => into_raw(FidanValue::Integer(n.abs())),
            FidanValue::Float(f) => into_raw(FidanValue::Float(f.abs())),
            _ => into_raw(FidanValue::Nothing),
        },
        "floor" => into_raw(int_or_float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).floor(),
        )),
        "ceil" => into_raw(int_or_float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).ceil(),
        )),
        "round" => into_raw(int_or_float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).round(),
        )),
        "trunc" => into_raw(int_or_float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).trunc(),
        )),
        "exp" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).exp(),
        )),
        "exp2" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).exp2(),
        )),
        "ln" | "log_e" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).ln(),
        )),
        "log2" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).log2(),
        )),
        "log10" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).log10(),
        )),
        "log" => {
            let base = args.get(1).map(num).unwrap_or(std::f64::consts::E);
            into_raw(FidanValue::Float(
                num(args.first().unwrap_or(&FidanValue::Nothing)).log(base),
            ))
        }
        "sin" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).sin(),
        )),
        "cos" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).cos(),
        )),
        "tan" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).tan(),
        )),
        "asin" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).asin(),
        )),
        "acos" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).acos(),
        )),
        "atan" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).atan(),
        )),
        "atan2" => {
            let y = num(args.first().unwrap_or(&FidanValue::Nothing));
            let x = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Float(y.atan2(x)))
        }
        "pow" => {
            let base = num(args.first().unwrap_or(&FidanValue::Nothing));
            let exp = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Float(base.powf(exp)))
        }
        "min" => {
            let a = num(args.first().unwrap_or(&FidanValue::Nothing));
            let b = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(if a <= b {
                args.first().cloned().unwrap_or(FidanValue::Nothing)
            } else {
                args.get(1).cloned().unwrap_or(FidanValue::Nothing)
            })
        }
        "max" => {
            let a = num(args.first().unwrap_or(&FidanValue::Nothing));
            let b = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(if a >= b {
                args.first().cloned().unwrap_or(FidanValue::Nothing)
            } else {
                args.get(1).cloned().unwrap_or(FidanValue::Nothing)
            })
        }
        "clamp" => {
            let v = num(args.first().unwrap_or(&FidanValue::Nothing));
            let lo = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            let hi = num(args.get(2).unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Float(v.clamp(lo, hi)))
        }
        "sign" | "signum" => match args.first().unwrap_or(&FidanValue::Nothing) {
            FidanValue::Integer(n) => into_raw(FidanValue::Integer(n.signum())),
            FidanValue::Float(f) => into_raw(FidanValue::Float(f.signum())),
            _ => into_raw(FidanValue::Nothing),
        },
        "isNaN" | "is_nan" => into_raw(FidanValue::Boolean(
            num(args.first().unwrap_or(&FidanValue::Nothing)).is_nan(),
        )),
        "isInfinite" | "is_infinite" => into_raw(FidanValue::Boolean(
            num(args.first().unwrap_or(&FidanValue::Nothing)).is_infinite(),
        )),
        "isFinite" | "is_finite" => into_raw(FidanValue::Boolean(
            num(args.first().unwrap_or(&FidanValue::Nothing)).is_finite(),
        )),
        // ── Hyperbolic ────────────────────────────────────────────────────
        "sinh" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).sinh(),
        )),
        "cosh" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).cosh(),
        )),
        "tanh" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).tanh(),
        )),
        "hypot" => {
            let a = num(args.first().unwrap_or(&FidanValue::Nothing));
            let b = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Float(a.hypot(b)))
        }
        "fract" => into_raw(FidanValue::Float(
            num(args.first().unwrap_or(&FidanValue::Nothing)).fract(),
        )),
        // ── More log ──────────────────────────────────────────────────────
        "logN" | "log_n" => {
            let base = num(args.get(1).unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Float(
                num(args.first().unwrap_or(&FidanValue::Nothing)).log(base),
            ))
        }
        // ── Constants ─────────────────────────────────────────────────────
        "inf" | "infinity" => into_raw(FidanValue::Float(f64::INFINITY)),
        "nan" | "NaN" => into_raw(FidanValue::Float(f64::NAN)),
        // ── Random ────────────────────────────────────────────────────────
        "random" => {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(12345);
            let lcg = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            into_raw(FidanValue::Float((lcg as f64) / (u32::MAX as f64)))
        }
        "randomInt" | "random_int" => {
            let lo = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let hi = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n,
                _ => 100,
            };
            if hi <= lo {
                return into_raw(FidanValue::Integer(lo));
            }
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(42);
            let lcg = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            into_raw(FidanValue::Integer(lo + (lcg as i64).abs() % (hi - lo)))
        }
        _ => {
            eprintln!("AOT stdlib math: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── string module (free-function API) ─────────────────────────────────────────

fn dispatch_string_fn(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    // Handle functions whose first arg is NOT a string.
    match func {
        "fromChars" | "from_chars" => {
            let list_val = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::List(l) = list_val {
                let s: String = l
                    .borrow()
                    .iter()
                    .filter_map(|v| {
                        if let FidanValue::String(cs) = v {
                            cs.as_str().chars().next()
                        } else {
                            None
                        }
                    })
                    .collect();
                return into_raw(FidanValue::String(FidanString::new(&s)));
            }
            return into_raw(FidanValue::String(FidanString::new("")));
        }
        "fromCharCode" | "from_char_code" => {
            let code = match args.first() {
                Some(FidanValue::Integer(n)) => *n as u32,
                _ => 0,
            };
            let ch = char::from_u32(code).unwrap_or('\0');
            return into_raw(FidanValue::String(FidanString::new(&ch.to_string())));
        }
        _ => {}
    }
    // Delegate to method dispatch by treating the first arg as the string receiver.
    if let Some(recv) = args.first().cloned()
        && let FidanValue::String(s) = recv
    {
        let rest = args.into_iter().skip(1).collect();
        return dispatch_string_method(s, func, rest);
    }
    into_raw(FidanValue::Nothing)
}

// ── io module ─────────────────────────────────────────────────────────────────

fn dispatch_io(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match func {
        "readFile" | "read_file" => {
            if let Some(FidanValue::String(path)) = args.first() {
                match std::fs::read_to_string(path.as_str()) {
                    Ok(s) => into_raw(FidanValue::String(FidanString::new(&s))),
                    Err(e) => {
                        eprintln!("io.readFile error: {}", e);
                        into_raw(FidanValue::Nothing)
                    }
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "writeFile" | "write_file" => {
            if let (Some(FidanValue::String(path)), Some(content)) = (args.first(), args.get(1)) {
                let text = as_str_val(content);
                match std::fs::write(path.as_str(), text) {
                    Ok(_) => into_raw(FidanValue::Boolean(true)),
                    Err(e) => {
                        eprintln!("io.writeFile error: {}", e);
                        into_raw(FidanValue::Boolean(false))
                    }
                }
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "appendFile" | "append_file" => {
            if let (Some(FidanValue::String(path)), Some(content)) = (args.first(), args.get(1)) {
                use std::io::Write;
                let text = as_str_val(content);
                match std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path.as_str())
                {
                    Ok(mut f) => {
                        let ok = f.write_all(text.as_bytes()).is_ok();
                        into_raw(FidanValue::Boolean(ok))
                    }
                    Err(e) => {
                        eprintln!("io.appendFile error: {}", e);
                        into_raw(FidanValue::Boolean(false))
                    }
                }
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "readLines" | "read_lines" => {
            if let Some(FidanValue::String(path)) = args.first() {
                match std::fs::read_to_string(path.as_str()) {
                    Ok(s) => {
                        let mut list = FidanList::new();
                        for line in s.lines() {
                            list.append(FidanValue::String(FidanString::new(line)));
                        }
                        into_raw(FidanValue::List(OwnedRef::new(list)))
                    }
                    Err(e) => {
                        eprintln!("io.readLines error: {}", e);
                        into_raw(FidanValue::Nothing)
                    }
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "fileExists" | "file_exists" | "exists" => {
            if let Some(FidanValue::String(path)) = args.first() {
                into_raw(FidanValue::Boolean(
                    std::path::Path::new(path.as_str()).exists(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "deleteFile" | "delete_file" => {
            if let Some(FidanValue::String(path)) = args.first() {
                into_raw(FidanValue::Boolean(
                    std::fs::remove_file(path.as_str()).is_ok(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "print" => {
            let text = args.iter().map(as_str_val).collect::<Vec<_>>().join(" ");
            println!("{}", text);
            into_raw(FidanValue::Nothing)
        }
        "println" => {
            let text = args.iter().map(as_str_val).collect::<Vec<_>>().join(" ");
            println!("{}", text);
            into_raw(FidanValue::Nothing)
        }
        "eprint" | "eprintln" => {
            let text = args.iter().map(as_str_val).collect::<Vec<_>>().join(" ");
            eprintln!("{}", text);
            into_raw(FidanValue::Nothing)
        }
        "readLine" | "read_line" | "readline" => {
            let mut input = String::new();
            let _ = std::io::stdin().read_line(&mut input);
            let trimmed = input.trim_end_matches('\n').trim_end_matches('\r');
            into_raw(FidanValue::String(FidanString::new(trimmed)))
        }
        // ── File predicates ───────────────────────────────────────────────
        "isFile" | "is_file" => {
            if let Some(FidanValue::String(path)) = args.first() {
                into_raw(FidanValue::Boolean(
                    std::path::Path::new(path.as_str()).is_file(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "isDir" | "is_dir" | "isDirectory" | "is_directory" => {
            if let Some(FidanValue::String(path)) = args.first() {
                into_raw(FidanValue::Boolean(
                    std::path::Path::new(path.as_str()).is_dir(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        // ── Directory ops ─────────────────────────────────────────────────
        "makeDir" | "make_dir" | "mkdir" | "createDir" | "create_dir" => {
            if let Some(FidanValue::String(path)) = args.first() {
                into_raw(FidanValue::Boolean(
                    std::fs::create_dir_all(path.as_str()).is_ok(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "listDir" | "list_dir" | "readDir" | "read_dir" => {
            if let Some(FidanValue::String(path)) = args.first() {
                let mut list = FidanList::new();
                if let Ok(entries) = std::fs::read_dir(path.as_str()) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        list.append(FidanValue::String(FidanString::new(&name)));
                    }
                }
                into_raw(FidanValue::List(OwnedRef::new(list)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── File copy/rename ──────────────────────────────────────────────
        "copyFile" | "copy_file" => {
            if let (Some(FidanValue::String(from)), Some(FidanValue::String(to))) =
                (args.first(), args.get(1))
            {
                into_raw(FidanValue::Boolean(
                    std::fs::copy(from.as_str(), to.as_str()).is_ok(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "renameFile" | "rename_file" | "moveFile" | "move_file" => {
            if let (Some(FidanValue::String(from)), Some(FidanValue::String(to))) =
                (args.first(), args.get(1))
            {
                into_raw(FidanValue::Boolean(
                    std::fs::rename(from.as_str(), to.as_str()).is_ok(),
                ))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        // ── Path utilities ────────────────────────────────────────────────
        "join" | "joinPath" | "join_path" => {
            let mut path = std::path::PathBuf::new();
            for arg in &args {
                path.push(as_str_val(arg));
            }
            into_raw(FidanValue::String(FidanString::new(
                &path.to_string_lossy(),
            )))
        }
        "dirname" | "dir_name" | "parent" => {
            if let Some(FidanValue::String(p)) = args.first() {
                let dir = std::path::Path::new(p.as_str())
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .unwrap_or_default();
                into_raw(FidanValue::String(FidanString::new(&dir)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "basename" | "base_name" | "fileName" | "file_name" => {
            if let Some(FidanValue::String(p)) = args.first() {
                let name = std::path::Path::new(p.as_str())
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                into_raw(FidanValue::String(FidanString::new(&name)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "extension" | "ext" => {
            if let Some(FidanValue::String(p)) = args.first() {
                let ext = std::path::Path::new(p.as_str())
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();
                into_raw(FidanValue::String(FidanString::new(&ext)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "cwd" | "currentDir" | "current_dir" | "pwd" => {
            let dir = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            into_raw(FidanValue::String(FidanString::new(&dir)))
        }
        "absolutePath" | "absolute_path" | "realPath" | "real_path" => {
            if let Some(FidanValue::String(p)) = args.first() {
                let abs = std::fs::canonicalize(p.as_str())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| p.as_str().to_string());
                into_raw(FidanValue::String(FidanString::new(&abs)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── Env (accessible from io namespace) ───────────────────────────
        "getEnv" | "get_env" | "env" => {
            if let Some(FidanValue::String(key)) = args.first() {
                match std::env::var(key.as_str()) {
                    Ok(v) => into_raw(FidanValue::String(FidanString::new(&v))),
                    Err(_) => into_raw(FidanValue::Nothing),
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setEnv" | "set_env" => {
            if let (Some(FidanValue::String(k)), Some(v)) = (args.first(), args.get(1)) {
                unsafe { std::env::set_var(k.as_str(), as_str_val(v)) };
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "args" | "argv" => {
            let mut list = FidanList::new();
            for a in std::env::args() {
                list.append(FidanValue::String(FidanString::new(&a)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        // ── Misc ──────────────────────────────────────────────────────────
        "flush" => {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            into_raw(FidanValue::Nothing)
        }
        _ => {
            eprintln!("AOT stdlib io: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── collections module ────────────────────────────────────────────────────────

fn dispatch_collections(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match func {
        "range" => {
            let start = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let end = args
                .get(1)
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let inclusive = args
                .get(2)
                .map(|v| matches!(v, FidanValue::Boolean(true)))
                .unwrap_or(false);
            let mut list = FidanList::new();
            let real_end = if inclusive { end + 1 } else { end };
            for i in start..real_end {
                list.append(FidanValue::Integer(i));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "sort" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let mut items: Vec<FidanValue> = b.iter().cloned().collect();
                items.sort_by(compare_values);
                let mut new_list = FidanList::new();
                for v in items {
                    new_list.append(v);
                }
                into_raw(FidanValue::List(OwnedRef::new(new_list)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "reverse" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let mut new_list = FidanList::new();
                for v in b.iter().rev() {
                    new_list.append(v.clone());
                }
                into_raw(FidanValue::List(OwnedRef::new(new_list)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "flatten" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
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
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "zip" => {
            if let (Some(FidanValue::List(la)), Some(FidanValue::List(lb))) =
                (args.first(), args.get(1))
            {
                let ba = la.borrow();
                let bb = lb.borrow();
                let len = ba.len().min(bb.len());
                let mut pairs = FidanList::new();
                for i in 0..len {
                    let mut pair = FidanList::new();
                    pair.append(ba.get(i).cloned().unwrap_or(FidanValue::Nothing));
                    pair.append(bb.get(i).cloned().unwrap_or(FidanValue::Nothing));
                    pairs.append(FidanValue::List(OwnedRef::new(pair)));
                }
                into_raw(FidanValue::List(OwnedRef::new(pairs)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "sum" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let mut total_i: i64 = 0;
                let mut has_float = false;
                let mut total_f: f64 = 0.0;
                for v in b.iter() {
                    match v {
                        FidanValue::Integer(n) => {
                            total_i = total_i.wrapping_add(*n);
                            total_f += *n as f64;
                        }
                        FidanValue::Float(f) => {
                            has_float = true;
                            total_f += f;
                        }
                        _ => {}
                    }
                }
                if has_float {
                    into_raw(FidanValue::Float(total_f))
                } else {
                    into_raw(FidanValue::Integer(total_i))
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "min" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let min = b.iter().min_by(|a, b| compare_values(a, b)).cloned();
                into_raw(min.unwrap_or(FidanValue::Nothing))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "max" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let max = b.iter().max_by(|a, b| compare_values(a, b)).cloned();
                into_raw(max.unwrap_or(FidanValue::Nothing))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "unique" | "deduplicate" | "dedup" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let b = l.borrow();
                let mut seen: Vec<FidanValue> = Vec::new();
                let mut new_list = FidanList::new();
                for v in b.iter() {
                    if !seen.iter().any(|s| values_equal(s, v)) {
                        seen.push(v.clone());
                        new_list.append(v.clone());
                    }
                }
                into_raw(FidanValue::List(OwnedRef::new(new_list)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── Higher-order (no callback) ────────────────────────────────────
        "count" | "length" | "len" => match args.first() {
            Some(FidanValue::List(l)) => into_raw(FidanValue::Integer(l.borrow().len() as i64)),
            Some(FidanValue::Dict(d)) => into_raw(FidanValue::Integer(d.borrow().len() as i64)),
            _ => into_raw(FidanValue::Integer(0)),
        },
        "isEmpty" | "is_empty" => match args.first() {
            Some(FidanValue::List(l)) => into_raw(FidanValue::Boolean(l.borrow().is_empty())),
            Some(FidanValue::Dict(d)) => into_raw(FidanValue::Boolean(d.borrow().is_empty())),
            _ => into_raw(FidanValue::Boolean(true)),
        },
        "concat" => {
            let mut result = FidanList::new();
            for arg in &args {
                if let FidanValue::List(l) = arg {
                    for v in l.borrow().iter() {
                        result.append(v.clone());
                    }
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(result)))
        }
        "slice" | "sliceList" | "slice_list" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let items: Vec<FidanValue> = l.borrow().iter().cloned().collect();
                let len = items.len();
                let start = match args.get(1) {
                    Some(FidanValue::Integer(n)) => (*n).max(0) as usize,
                    _ => 0,
                };
                let end = match args.get(2) {
                    Some(FidanValue::Integer(n)) => (*n as usize).min(len),
                    _ => len,
                };
                let mut result = FidanList::new();
                for v in items[start.min(len)..end.min(len)].iter() {
                    result.append(v.clone());
                }
                into_raw(FidanValue::List(OwnedRef::new(result)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "first" | "head" => match args.first() {
            Some(FidanValue::List(l)) => {
                into_raw(l.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            }
            _ => into_raw(FidanValue::Nothing),
        },
        "last" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let borrow = l.borrow();
                let len = borrow.len();
                into_raw(
                    borrow
                        .get(len.saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "join" => {
            let list_val = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let sep = args.get(1).map(as_str_val).unwrap_or_default();
            if let FidanValue::List(l) = list_val {
                let parts: Vec<String> = l.borrow().iter().map(as_str_val).collect();
                into_raw(FidanValue::String(FidanString::new(&parts.join(&sep))))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "product" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let prod: f64 = l
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        FidanValue::Integer(n) => *n as f64,
                        FidanValue::Float(f) => *f,
                        _ => 1.0,
                    })
                    .product();
                if prod.fract() == 0.0 && prod.abs() < i64::MAX as f64 {
                    into_raw(FidanValue::Integer(prod as i64))
                } else {
                    into_raw(FidanValue::Float(prod))
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── Set operations (Set = Dict with Boolean true values) ──────────
        "Set" => {
            let mut dict = FidanDict::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    let key = FidanString::new(&as_str_val(v));
                    dict.insert(key, FidanValue::Boolean(true));
                }
            }
            into_raw(FidanValue::Dict(OwnedRef::new(dict)))
        }
        "setAdd" | "set_add" => {
            if let (Some(FidanValue::Dict(d)), Some(val)) = (args.first(), args.get(1)) {
                let key = FidanString::new(&as_str_val(val));
                d.borrow_mut().insert(key, FidanValue::Boolean(true));
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setRemove" | "set_remove" => {
            if let (Some(FidanValue::Dict(d)), Some(val)) = (args.first(), args.get(1)) {
                let key = FidanString::new(&as_str_val(val));
                d.borrow_mut().remove(&key);
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setContains" | "set_contains" | "setHas" | "set_has" => {
            if let (Some(FidanValue::Dict(d)), Some(val)) = (args.first(), args.get(1)) {
                let key = FidanString::new(&as_str_val(val));
                into_raw(FidanValue::Boolean(d.borrow().get(&key).is_some()))
            } else {
                into_raw(FidanValue::Boolean(false))
            }
        }
        "setToList" | "set_to_list" | "setValues" | "set_values" => {
            if let Some(FidanValue::Dict(d)) = args.first() {
                let mut list = FidanList::new();
                for (k, _) in d.borrow().iter() {
                    list.append(FidanValue::String(k.clone()));
                }
                into_raw(FidanValue::List(OwnedRef::new(list)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setLen" | "set_len" | "setSize" | "set_size" => {
            if let Some(FidanValue::Dict(d)) = args.first() {
                into_raw(FidanValue::Integer(d.borrow().len() as i64))
            } else {
                into_raw(FidanValue::Integer(0))
            }
        }
        "setUnion" | "set_union" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let mut result = FidanDict::new();
                for (k, v) in a.borrow().iter() {
                    result.insert(k.clone(), v.clone());
                }
                for (k, v) in b.borrow().iter() {
                    result.insert(k.clone(), v.clone());
                }
                into_raw(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setIntersect" | "set_intersect" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let mut result = FidanDict::new();
                let b_ref = b.borrow();
                for (k, v) in a.borrow().iter() {
                    if b_ref.get(k).is_some() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                into_raw(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "setDiff" | "set_diff" | "setDifference" | "set_difference" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let mut result = FidanDict::new();
                let b_ref = b.borrow();
                for (k, v) in a.borrow().iter() {
                    if b_ref.get(k).is_none() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                into_raw(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── Queue (FIFO) ──────────────────────────────────────────────────
        "Queue" => {
            let mut list = FidanList::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    list.append(v.clone());
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "enqueue" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let l = l.clone();
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                l.borrow_mut().append(val);
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "dequeue" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let mut borrow = l.borrow_mut();
                let items: Vec<FidanValue> = borrow.iter().cloned().collect();
                if items.is_empty() {
                    return into_raw(FidanValue::Nothing);
                }
                let first_item = items[0].clone();
                let mut new_list = FidanList::new();
                for item in items.into_iter().skip(1) {
                    new_list.append(item);
                }
                *borrow = new_list;
                into_raw(first_item)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "peek" | "front" => {
            if let Some(FidanValue::List(l)) = args.first() {
                into_raw(l.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        // ── Stack (LIFO) ──────────────────────────────────────────────────
        "Stack" => {
            let mut list = FidanList::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    list.append(v.clone());
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "push" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let l = l.clone();
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                l.borrow_mut().append(val);
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "pop" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let mut borrow = l.borrow_mut();
                let items: Vec<FidanValue> = borrow.iter().cloned().collect();
                let len = items.len();
                if len == 0 {
                    return into_raw(FidanValue::Nothing);
                }
                let top = items[len - 1].clone();
                let mut new_list = FidanList::new();
                for item in items.into_iter().take(len - 1) {
                    new_list.append(item);
                }
                *borrow = new_list;
                into_raw(top)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "stackPeek" | "top" | "stack_peek" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let borrow = l.borrow();
                let len = borrow.len();
                into_raw(
                    borrow
                        .get(len.saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        _ => {
            eprintln!("AOT stdlib collections: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── env module ────────────────────────────────────────────────────────────────

fn dispatch_env(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match func {
        "get" | "getVar" | "get_var" => {
            if let Some(FidanValue::String(key)) = args.first() {
                match std::env::var(key.as_str()) {
                    Ok(v) => into_raw(FidanValue::String(FidanString::new(&v))),
                    Err(_) => into_raw(FidanValue::Nothing),
                }
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "set" | "setVar" | "set_var" => {
            if let (Some(FidanValue::String(k)), Some(v)) = (args.first(), args.get(1)) {
                unsafe { std::env::set_var(k.as_str(), as_str_val(v)) };
                into_raw(FidanValue::Nothing)
            } else {
                into_raw(FidanValue::Nothing)
            }
        }
        "args" => {
            let mut list = FidanList::new();
            for arg in std::env::args().skip(1) {
                list.append(FidanValue::String(FidanString::new(&arg)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        _ => {
            eprintln!("AOT stdlib env: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── regex module ──────────────────────────────────────────────────────────────

fn dispatch_regex(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    fn as_s(v: &FidanValue) -> String {
        match v {
            FidanValue::String(s) => s.as_str().to_string(),
            FidanValue::Integer(n) => n.to_string(),
            FidanValue::Float(f) => f.to_string(),
            FidanValue::Boolean(b) => b.to_string(),
            _ => String::new(),
        }
    }
    match func {
        "test" | "isMatch" | "is_match" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let result = compile_regex(&pattern)
                .map(|re| re.is_match(&subject))
                .unwrap_or(false);
            into_raw(FidanValue::Boolean(result))
        }
        "match" | "find" | "find_first" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile_regex(&pattern)
                .and_then(|re| re.find(&subject).map(|m| m.as_str().to_string()))
            {
                Some(s) => into_raw(FidanValue::String(FidanString::new(&s))),
                None => into_raw(FidanValue::Nothing),
            }
        }
        "findAll" | "find_all" | "matches" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Some(re) = compile_regex(&pattern) {
                for m in re.find_iter(&subject) {
                    list.append(FidanValue::String(FidanString::new(m.as_str())));
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "replace" | "sub" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = as_s(args.get(2).unwrap_or(&FidanValue::Nothing));
            let result = if let Some(re) = compile_regex(&pattern) {
                re.replace(&subject, replacement.as_str()).to_string()
            } else {
                subject
            };
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        "replaceAll" | "replace_all" | "gsub" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = as_s(args.get(2).unwrap_or(&FidanValue::Nothing));
            let result = if let Some(re) = compile_regex(&pattern) {
                re.replace_all(&subject, replacement.as_str()).to_string()
            } else {
                subject
            };
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        "split" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Some(re) = compile_regex(&pattern) {
                for part in re.split(&subject) {
                    list.append(FidanValue::String(FidanString::new(part)));
                }
            } else {
                list.append(FidanValue::String(FidanString::new(&subject)));
            }
            into_raw(FidanValue::List(OwnedRef::new(list)))
        }
        "capture" | "exec" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile_regex(&pattern) {
                Some(re) => match re.captures(&subject) {
                    Some(caps) => {
                        let mut list = FidanList::new();
                        for g in caps.iter() {
                            match g {
                                Some(m) => {
                                    list.append(FidanValue::String(FidanString::new(m.as_str())))
                                }
                                None => list.append(FidanValue::Nothing),
                            }
                        }
                        into_raw(FidanValue::List(OwnedRef::new(list)))
                    }
                    None => into_raw(FidanValue::Nothing),
                },
                None => into_raw(FidanValue::Nothing),
            }
        }
        "isValid" | "is_valid" => {
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            into_raw(FidanValue::Boolean(Regex::new(&pattern).is_ok()))
        }
        "captureAll" | "capture_all" | "execAll" | "exec_all" => {
            // Returns a list of lists; each inner list is one match's capture groups.
            let pattern = as_s(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_s(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut outer = FidanList::new();
            if let Some(re) = compile_regex(&pattern) {
                for caps in re.captures_iter(&subject) {
                    let mut inner = FidanList::new();
                    for g in caps.iter() {
                        match g {
                            Some(m) => {
                                inner.append(FidanValue::String(FidanString::new(m.as_str())))
                            }
                            None => inner.append(FidanValue::Nothing),
                        }
                    }
                    outer.append(FidanValue::List(OwnedRef::new(inner)));
                }
            }
            into_raw(FidanValue::List(OwnedRef::new(outer)))
        }
        _ => {
            eprintln!("AOT stdlib regex: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── parallel module ───────────────────────────────────────────────────────────
//
// These run sequentially in AOT (true parallelism would require extra runtime
// infrastructure). Behaviour is identical to the interpreter's parallel module.

fn dispatch_parallel(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    match func {
        "parallelMap" | "parallel_map" => {
            // parallelMap(list, fn) -> list
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
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
            // parallelFilter(list, predicate) -> list
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
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
            // parallelForEach(list, fn) -> nothing
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            for item in list {
                let item_ptr = into_raw(item);
                let r = unsafe { fdn_call_dynamic(fn_ptr, &item_ptr as *const *mut FidanValue, 1) };
                if !r.is_null() {
                    unsafe { drop(Box::from_raw(r)) };
                }
                unsafe { drop(Box::from_raw(item_ptr)) };
            }
            unsafe { drop(Box::from_raw(fn_ptr)) };
            into_raw(FidanValue::Nothing)
        }
        "parallelReduce" | "parallel_reduce" => {
            // parallelReduce(list, fn, initial) -> value
            let list = match args.first() {
                Some(FidanValue::List(l)) => l.borrow().iter().cloned().collect::<Vec<_>>(),
                _ => return into_raw(FidanValue::Nothing),
            };
            let fn_val = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let initial = args.into_iter().nth(2).unwrap_or(FidanValue::Nothing);
            let fn_ptr = into_raw(fn_val);
            let mut acc = initial;
            for item in list {
                let acc_ptr = into_raw(acc);
                let item_ptr = into_raw(item);
                let call_args = [acc_ptr, item_ptr];
                let r = unsafe { fdn_call_dynamic(fn_ptr, call_args.as_ptr(), 2) };
                acc = if !r.is_null() {
                    let v = unsafe { (*r).clone() };
                    unsafe { drop(Box::from_raw(r)) };
                    v
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
    fn val_display(v: &FidanValue) -> String {
        match v {
            FidanValue::String(s) => format!("\"{}\"", s.as_str()),
            FidanValue::Integer(n) => n.to_string(),
            FidanValue::Float(f) => f.to_string(),
            FidanValue::Boolean(b) => b.to_string(),
            FidanValue::Nothing => "nothing".to_string(),
            _ => "<value>".to_string(),
        }
    }
    fn fail_test(msg: String) -> *mut FidanValue {
        eprintln!("Test failed: {}", msg);
        let msg_val = into_raw(FidanValue::String(FidanString::new(&msg)));
        unsafe { fdn_throw_unhandled(msg_val) }
    }
    match func {
        "assert" => {
            let cond = matches!(args.first(), Some(FidanValue::Boolean(true)));
            let msg = args
                .get(1)
                .map(|v| match v {
                    FidanValue::String(s) => s.as_str().to_string(),
                    _ => "assertion failed".to_string(),
                })
                .unwrap_or_else(|| "assertion failed".to_string());
            if !cond {
                return fail_test(msg);
            }
            into_raw(FidanValue::Nothing)
        }
        "assertEq" | "assert_eq" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if !values_equal(&a, &b) {
                return fail_test(format!(
                    "assertEq failed: {} != {}",
                    val_display(&a),
                    val_display(&b)
                ));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertNe" | "assert_ne" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if values_equal(&a, &b) {
                return fail_test(format!("assertNe failed: both are {}", val_display(&a)));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertGt" | "assert_gt" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if compare_values(&a, &b) != std::cmp::Ordering::Greater {
                return fail_test(format!(
                    "assertGt failed: {} is not > {}",
                    val_display(&a),
                    val_display(&b)
                ));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertLt" | "assert_lt" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if compare_values(&a, &b) != std::cmp::Ordering::Less {
                return fail_test(format!(
                    "assertLt failed: {} is not < {}",
                    val_display(&a),
                    val_display(&b)
                ));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertGe" | "assert_ge" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if compare_values(&a, &b) == std::cmp::Ordering::Less {
                return fail_test(format!(
                    "assertGe failed: {} is not >= {}",
                    val_display(&a),
                    val_display(&b)
                ));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertLe" | "assert_le" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if compare_values(&a, &b) == std::cmp::Ordering::Greater {
                return fail_test(format!(
                    "assertLe failed: {} is not <= {}",
                    val_display(&a),
                    val_display(&b)
                ));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertSome" | "assert_some" | "assertNotNothing" | "assert_not_nothing" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if matches!(v, FidanValue::Nothing) {
                return fail_test("assertSome failed: value is nothing".to_string());
            }
            into_raw(FidanValue::Nothing)
        }
        "assertNothing" | "assert_nothing" | "assertIsNothing" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if !matches!(v, FidanValue::Nothing) {
                return fail_test(format!("assertNothing failed: got {}", val_display(&v)));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertTrue" | "assert_true" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if !matches!(v, FidanValue::Boolean(true)) {
                return fail_test(format!("assertTrue failed: got {}", val_display(&v)));
            }
            into_raw(FidanValue::Nothing)
        }
        "assertFalse" | "assert_false" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if !matches!(v, FidanValue::Boolean(false)) {
                return fail_test(format!("assertFalse failed: got {}", val_display(&v)));
            }
            into_raw(FidanValue::Nothing)
        }
        "fail" => {
            let msg = args
                .first()
                .map(|v| match v {
                    FidanValue::String(s) => s.as_str().to_string(),
                    _ => "test.fail() called".to_string(),
                })
                .unwrap_or_else(|| "test.fail() called".to_string());
            fail_test(msg)
        }
        "pass" | "ok" => into_raw(FidanValue::Nothing),
        _ => {
            eprintln!("AOT stdlib test: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

// ── time module ───────────────────────────────────────────────────────────────

fn dispatch_time(func: &str, args: Vec<FidanValue>) -> *mut FidanValue {
    fn to_ms_val(v: &FidanValue) -> u64 {
        match v {
            FidanValue::Integer(n) => (*n).max(0) as u64,
            FidanValue::Float(f) => f.max(0.0) as u64,
            _ => 0,
        }
    }
    match func {
        "now" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            into_raw(FidanValue::Integer(ms))
        }
        "timestamp" => {
            let s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            into_raw(FidanValue::Integer(s))
        }
        "sleep" => {
            let ms = to_ms_val(args.first().unwrap_or(&FidanValue::Nothing));
            std::thread::sleep(std::time::Duration::from_millis(ms));
            into_raw(FidanValue::Nothing)
        }
        "elapsed" => {
            let start = match args.first().unwrap_or(&FidanValue::Nothing) {
                FidanValue::Integer(n) => *n,
                _ => 0,
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            into_raw(FidanValue::Integer(now - start))
        }
        "wait" => {
            // Alias for sleep
            let ms = to_ms_val(args.first().unwrap_or(&FidanValue::Nothing));
            std::thread::sleep(std::time::Duration::from_millis(ms));
            into_raw(FidanValue::Nothing)
        }
        "date" | "today" => {
            let ms = match args.first() {
                Some(v) => match v {
                    FidanValue::Integer(n) => *n,
                    _ => 0,
                },
                None => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            };
            let (y, mo, d, _, _, _) = ms_to_civil(ms);
            let s = format!("{:04}-{:02}-{:02}", y, mo, d);
            into_raw(FidanValue::String(FidanString::new(&s)))
        }
        "time" | "timeStr" | "time_str" => {
            let ms = match args.first() {
                Some(v) => match v {
                    FidanValue::Integer(n) => *n,
                    _ => 0,
                },
                None => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            };
            let (_, _, _, h, min, s) = ms_to_civil(ms);
            let ms_part = (ms.abs() % 1000) as u32;
            let result = format!("{:02}:{:02}:{:02}.{:03}", h, min, s, ms_part);
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        "datetime" => {
            let ms = match args.first() {
                Some(v) => match v {
                    FidanValue::Integer(n) => *n,
                    _ => 0,
                },
                None => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            };
            let (y, mo, d, h, min, s) = ms_to_civil(ms);
            let result = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, mo, d, h, min, s);
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        "format" | "formatDate" | "format_date" => {
            // format(ms, pattern) — supports %Y %m %d %H %M %S %L placeholders
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
            };
            let pattern = match args.get(1) {
                Some(FidanValue::String(s)) => s.as_str().to_string(),
                _ => "%Y-%m-%dT%H:%M:%S".to_string(),
            };
            let (y, mo, d, h, min, s) = ms_to_civil(ms);
            let ms_part = (ms.abs() % 1000) as u32;
            let result = pattern
                .replace("%Y", &format!("{:04}", y))
                .replace("%m", &format!("{:02}", mo))
                .replace("%d", &format!("{:02}", d))
                .replace("%H", &format!("{:02}", h))
                .replace("%M", &format!("{:02}", min))
                .replace("%S", &format!("{:02}", s))
                .replace("%L", &format!("{:03}", ms_part));
            into_raw(FidanValue::String(FidanString::new(&result)))
        }
        "year" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (y, _, _, _, _, _) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(y as i64))
        }
        "month" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (_, mo, _, _, _, _) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(mo as i64))
        }
        "day" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (_, _, d, _, _, _) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(d as i64))
        }
        "hour" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (_, _, _, h, _, _) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(h as i64))
        }
        "minute" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (_, _, _, _, min, _) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(min as i64))
        }
        "second" => {
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (_, _, _, _, _, s) = ms_to_civil(ms);
            into_raw(FidanValue::Integer(s as i64))
        }
        "weekday" => {
            // Returns 0=Sunday .. 6=Saturday, using Tomohiko Sakamoto's algorithm
            let ms = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let (y, mo, d, _, _, _) = ms_to_civil(ms);
            let t = [0i32, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
            let yr = if mo < 3 { y - 1 } else { y };
            let wd = (yr + yr / 4 - yr / 100 + yr / 400 + t[(mo as usize) - 1] + d as i32) % 7;
            into_raw(FidanValue::Integer(wd as i64))
        }
        _ => {
            eprintln!("AOT stdlib time: unknown function '{}'", func);
            into_raw(FidanValue::Nothing)
        }
    }
}

/// Convert milliseconds since Unix epoch to (year, month, day, hour, minute, second).
/// Uses the civil-date algorithm (Euclidean affine functions).
fn ms_to_civil(ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    let total_secs = ms.div_euclid(1000);
    let time_of_day = total_secs.rem_euclid(86400);
    let days = total_secs.div_euclid(86400);
    // Chrono-free civil date from epoch days (Henry S. Warren Jr.)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    (y as i32, m as u32, d as u32, h as u32, min as u32, s as u32)
}

// ── parallel iter (sequential AOT fallback) ───────────────────────────────────

/// Sequential implementation of `parallel for`: call `body_fn` (by FN_TABLE index)
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
    if let FidanValue::List(list_ref) = coll {
        let items: Vec<FidanValue> = list_ref.borrow().iter().cloned().collect();
        for item in items {
            let item_ptr = into_raw(item);
            let mut call_args: Vec<*mut FidanValue> = Vec::with_capacity(1 + env_slice.len());
            call_args.push(item_ptr);
            call_args.extend_from_slice(env_slice);
            let result = call_trampoline_by_idx(fn_idx as usize, &call_args);
            if !result.is_null() {
                drop(Box::from_raw(result));
            }
            drop(Box::from_raw(item_ptr));
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_spawn_expr(
    fn_idx: i64,
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = build_parallel_args_from_ptrs(args_ptr, args_cnt);
    let pending = FidanPending::spawn_with_args(args, move |bundle: ParallelArgs| {
        call_trampoline_owned(fn_idx as usize, bundle.into_vec())
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
    let pending = FidanPending::spawn_with_args(args, move |bundle: ParallelArgs| {
        let _ = &task_name;
        call_trampoline_owned(fn_idx as usize, bundle.into_vec())
    });
    into_raw(FidanValue::Pending(pending))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdn_pending_join(handle: *mut FidanValue) -> *mut FidanValue {
    match borrow(handle) {
        FidanValue::Pending(pending) => into_raw(pending.join()),
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
    let mut result = String::new();
    for i in 0..count as usize {
        let p = *parts_ptr.add(i);
        result.push_str(&display(borrow(p)));
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
