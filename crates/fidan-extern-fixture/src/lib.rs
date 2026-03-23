use fidan_runtime::FidanValue;

#[unsafe(no_mangle)]
pub extern "C" fn defaultExternAdd(a: i64, b: i64) -> i64 {
    a + b
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_native_add(a: i64, b: i64) -> i64 {
    a + b
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_float_scale(x: f64, scale: f64) -> f64 {
    x * scale
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_negate_bool(v: i8) -> i8 {
    if v == 0 { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_make_handle() -> usize {
    41
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_inc_handle(h: usize) -> usize {
    h + 1
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_read_handle(h: usize) -> i64 {
    h as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_fixture_free_handle(_: usize) {}

#[unsafe(no_mangle)]
/// # Safety
///
/// `args_ptr` must point to `args_cnt` valid `*mut FidanValue` entries allocated
/// by the caller, and each pointed-to value must remain valid for the duration
/// of this call.
pub unsafe extern "C" fn fidan_fixture_add_boxed(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = unsafe { std::slice::from_raw_parts(args_ptr, args_cnt as usize) };
    let a = match unsafe { &*args[0] } {
        FidanValue::Integer(n) => *n,
        _ => 0,
    };
    let b = match unsafe { &*args[1] } {
        FidanValue::Integer(n) => *n,
        _ => 0,
    };
    Box::into_raw(Box::new(FidanValue::Integer(a + b)))
}

#[unsafe(no_mangle)]
/// # Safety
///
/// `args_ptr` must point to `args_cnt` valid `*mut FidanValue` entries allocated
/// by the caller, and each pointed-to value must remain valid for the duration
/// of this call.
pub unsafe extern "C" fn fidan_fixture_echo_boxed(
    args_ptr: *const *mut FidanValue,
    args_cnt: i64,
) -> *mut FidanValue {
    let args = unsafe { std::slice::from_raw_parts(args_ptr, args_cnt as usize) };
    let first = unsafe { &*args[0] }.clone();
    Box::into_raw(Box::new(first))
}
