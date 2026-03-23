#![allow(clippy::missing_safety_doc)]
#![allow(unsafe_op_in_unsafe_fn)]

use fidan_driver::{FrontendOutput, compile_file_to_mir, compile_source_to_mir};
use fidan_interp::MirMachine;
use fidan_runtime::{FidanString, display};
use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;

pub use fidan_interp::FidanValue;

pub struct FidanVm {
    base_dir: PathBuf,
    last_error: Option<CString>,
}

impl FidanVm {
    fn new() -> Self {
        Self {
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            last_error: None,
        }
    }

    fn clear_error(&mut self) {
        self.last_error = None;
    }

    fn set_error(&mut self, message: impl AsRef<str>) {
        let sanitized = message.as_ref().replace('\0', " ");
        self.last_error = CString::new(sanitized).ok();
    }
}

fn into_raw_value(value: FidanValue) -> *mut FidanValue {
    Box::into_raw(Box::new(value))
}

unsafe fn borrow_value<'a>(ptr: *mut FidanValue) -> &'a FidanValue {
    debug_assert!(!ptr.is_null(), "null FidanValue pointer");
    &*ptr
}

fn find_result_slot(
    program: &fidan_mir::MirProgram,
    interner: &fidan_lexer::SymbolInterner,
) -> Option<usize> {
    // Initial embedding slice: if the script exposes a top-level `result`
    // binding, surface it as the eval result. Otherwise successful execution
    // returns `nothing`.
    let result_sym = interner.intern("result");
    program
        .globals
        .iter()
        .enumerate()
        .rev()
        .find(|(_, global)| global.name == result_sym)
        .map(|(idx, _)| idx)
}

fn eval_program(program: FrontendOutput) -> Result<*mut FidanValue, String> {
    let result_idx = find_result_slot(&program.mir, &program.interner);
    let mut machine = MirMachine::new(
        Arc::new(program.mir),
        Arc::clone(&program.interner),
        Arc::clone(&program.source_map),
    );
    machine
        .run()
        .map_err(|err| format!("{}: {}", err.code, err.message))?;
    let globals = machine.snapshot_globals();
    let result = result_idx
        .and_then(|idx| globals.get(idx).cloned())
        .unwrap_or(FidanValue::Nothing);
    Ok(into_raw_value(result))
}

#[unsafe(no_mangle)]
pub extern "C" fn fidan_vm_new() -> *mut FidanVm {
    Box::into_raw(Box::new(FidanVm::new()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_vm_free(vm: *mut FidanVm) {
    if !vm.is_null() {
        drop(Box::from_raw(vm));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_vm_set_base_dir(vm: *mut FidanVm, path: *const c_char) -> i8 {
    if vm.is_null() || path.is_null() {
        return 0;
    }
    let vm = &mut *vm;
    let raw = match CStr::from_ptr(path).to_str() {
        Ok(raw) => raw,
        Err(_) => {
            vm.set_error("invalid UTF-8 base directory");
            return 0;
        }
    };
    vm.base_dir = PathBuf::from(raw);
    vm.clear_error();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_vm_last_error(vm: *const FidanVm) -> *const c_char {
    if vm.is_null() {
        return ptr::null();
    }
    match (&*vm).last_error.as_ref() {
        Some(msg) => msg.as_ptr(),
        None => ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_eval(
    vm: *mut FidanVm,
    source: *const u8,
    len: usize,
) -> *mut FidanValue {
    if vm.is_null() || source.is_null() {
        return ptr::null_mut();
    }
    let vm = &mut *vm;
    let bytes = std::slice::from_raw_parts(source, len);
    let src = match std::str::from_utf8(bytes) {
        Ok(src) => src,
        Err(_) => {
            vm.set_error("source is not valid UTF-8");
            return ptr::null_mut();
        }
    };

    match compile_source_to_mir("<memory>", src, &vm.base_dir) {
        Ok(program) => match eval_program(program) {
            Ok(value) => {
                vm.clear_error();
                value
            }
            Err(err) => {
                vm.set_error(err);
                ptr::null_mut()
            }
        },
        Err(err) => {
            vm.set_error(err.to_string());
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_eval_file(vm: *mut FidanVm, path: *const c_char) -> *mut FidanValue {
    if vm.is_null() || path.is_null() {
        return ptr::null_mut();
    }
    let vm = &mut *vm;
    let raw_path = match CStr::from_ptr(path).to_str() {
        Ok(path) => path,
        Err(_) => {
            vm.set_error("file path is not valid UTF-8");
            return ptr::null_mut();
        }
    };

    match compile_file_to_mir(Path::new(raw_path)) {
        Ok(program) => match eval_program(program) {
            Ok(value) => {
                vm.clear_error();
                value
            }
            Err(err) => {
                vm.set_error(err);
                ptr::null_mut()
            }
        },
        Err(err) => {
            vm.set_error(err.to_string());
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_free(value: *mut FidanValue) {
    if !value.is_null() {
        drop(Box::from_raw(value));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_clone(value: *mut FidanValue) -> *mut FidanValue {
    if value.is_null() {
        return ptr::null_mut();
    }
    into_raw_value(borrow_value(value).clone())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_type_name(value: *mut FidanValue) -> *mut FidanValue {
    if value.is_null() {
        return ptr::null_mut();
    }
    into_raw_value(FidanValue::String(FidanString::new(
        borrow_value(value).type_name(),
    )))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_to_string(value: *mut FidanValue) -> *mut FidanValue {
    if value.is_null() {
        return ptr::null_mut();
    }
    into_raw_value(FidanValue::String(FidanString::new(&display(
        borrow_value(value),
    ))))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_as_int(value: *mut FidanValue) -> i64 {
    if value.is_null() {
        return 0;
    }
    match borrow_value(value) {
        FidanValue::Integer(n) => *n,
        FidanValue::Float(f) => *f as i64,
        FidanValue::Boolean(b) => *b as i64,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_as_float(value: *mut FidanValue) -> f64 {
    if value.is_null() {
        return 0.0;
    }
    match borrow_value(value) {
        FidanValue::Float(f) => *f,
        FidanValue::Integer(n) => *n as f64,
        _ => 0.0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_as_bool(value: *mut FidanValue) -> i8 {
    if value.is_null() {
        return 0;
    }
    match borrow_value(value) {
        FidanValue::Boolean(b) => *b as i8,
        FidanValue::Integer(n) => (*n != 0) as i8,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_is_nothing(value: *mut FidanValue) -> i8 {
    if value.is_null() {
        return 1;
    }
    matches!(borrow_value(value), FidanValue::Nothing) as i8
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_string_len(value: *mut FidanValue) -> usize {
    if value.is_null() {
        return 0;
    }
    match borrow_value(value) {
        FidanValue::String(s) => s.len(),
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fidan_value_string_bytes(value: *mut FidanValue) -> *const u8 {
    if value.is_null() {
        return ptr::null();
    }
    match borrow_value(value) {
        FidanValue::String(s) => s.as_str().as_ptr(),
        _ => ptr::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("libfidan-test-{unique}"));
        fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn eval_returns_top_level_result_binding() {
        unsafe {
            let vm = fidan_vm_new();
            let src = b"var result = 42\n";
            let value = fidan_eval(vm, src.as_ptr(), src.len());
            assert!(!value.is_null());
            assert_eq!(fidan_value_as_int(value), 42);
            fidan_value_free(value);
            fidan_vm_free(vm);
        }
    }

    #[test]
    fn eval_without_result_returns_nothing() {
        unsafe {
            let vm = fidan_vm_new();
            let src = b"print(\"hello from test\")\n";
            let value = fidan_eval(vm, src.as_ptr(), src.len());
            assert!(!value.is_null());
            assert_eq!(fidan_value_is_nothing(value), 1);
            fidan_value_free(value);
            fidan_vm_free(vm);
        }
    }

    #[test]
    fn eval_failure_sets_last_error() {
        unsafe {
            let vm = fidan_vm_new();
            let src = b"var result =\n";
            let value = fidan_eval(vm, src.as_ptr(), src.len());
            assert!(value.is_null());
            let err = fidan_vm_last_error(vm);
            assert!(!err.is_null());
            let text = CStr::from_ptr(err).to_str().expect("utf-8 error text");
            assert!(!text.is_empty());
            fidan_vm_free(vm);
        }
    }

    #[test]
    fn eval_uses_vm_base_dir_for_relative_imports() {
        let dir = temp_dir();
        fs::write(
            dir.join("helper.fdn"),
            "action exported returns string {\n    return \"from import\"\n}\n",
        )
        .expect("write helper");

        unsafe {
            let vm = fidan_vm_new();
            let base = CString::new(dir.to_string_lossy().to_string()).expect("base dir cstring");
            assert_eq!(fidan_vm_set_base_dir(vm, base.as_ptr()), 1);

            let src = b"use helper\nvar result = helper.exported()\n";
            let value = fidan_eval(vm, src.as_ptr(), src.len());
            assert!(!value.is_null());

            let text = fidan_value_to_string(value);
            let bytes = fidan_value_string_bytes(text);
            let len = fidan_value_string_len(text);
            let result_text =
                std::str::from_utf8(std::slice::from_raw_parts(bytes, len)).expect("utf-8 result");
            assert_eq!(result_text, "from import");

            fidan_value_free(text);
            fidan_value_free(value);
            fidan_vm_free(vm);
        }

        let _ = fs::remove_dir_all(dir);
    }
}
