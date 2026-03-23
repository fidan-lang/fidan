use libfidan::{
    FidanValue, FidanVm, fidan_eval, fidan_eval_file, fidan_value_as_bool, fidan_value_as_float,
    fidan_value_as_int, fidan_value_clone, fidan_value_free, fidan_value_is_nothing,
    fidan_value_string_bytes, fidan_value_string_len, fidan_value_to_string, fidan_value_type_name,
    fidan_vm_free, fidan_vm_last_error, fidan_vm_new, fidan_vm_set_base_dir,
};
use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("{0}")]
    Message(String),
    #[error("string contains interior NUL byte")]
    InteriorNul,
}

pub struct Vm {
    raw: NonNull<FidanVm>,
}

impl Vm {
    pub fn new() -> Self {
        let raw = NonNull::new(fidan_vm_new()).expect("fidan_vm_new returned null");
        Self { raw }
    }

    pub fn set_base_dir(&mut self, path: impl AsRef<Path>) -> Result<(), EmbedError> {
        let owned = path.as_ref().to_string_lossy().into_owned();
        let c_path = CString::new(owned).map_err(|_| EmbedError::InteriorNul)?;
        let ok = unsafe { fidan_vm_set_base_dir(self.raw.as_ptr(), c_path.as_ptr()) };
        if ok == 1 {
            Ok(())
        } else {
            Err(self.take_last_error())
        }
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, EmbedError> {
        let raw = unsafe { fidan_eval(self.raw.as_ptr(), source.as_ptr(), source.len()) };
        self.wrap_result(raw)
    }

    pub fn eval_file(&mut self, path: impl AsRef<Path>) -> Result<Value, EmbedError> {
        let owned = path.as_ref().to_string_lossy().into_owned();
        let c_path = CString::new(owned).map_err(|_| EmbedError::InteriorNul)?;
        let raw = unsafe { fidan_eval_file(self.raw.as_ptr(), c_path.as_ptr()) };
        self.wrap_result(raw)
    }

    fn wrap_result(&mut self, raw: *mut FidanValue) -> Result<Value, EmbedError> {
        match NonNull::new(raw) {
            Some(raw) => Ok(Value { raw }),
            None => Err(self.take_last_error()),
        }
    }

    fn take_last_error(&self) -> EmbedError {
        let raw = unsafe { fidan_vm_last_error(self.raw.as_ptr()) };
        if raw.is_null() {
            EmbedError::Message("unknown libfidan error".to_string())
        } else {
            let msg = unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned();
            EmbedError::Message(msg)
        }
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        unsafe {
            fidan_vm_free(self.raw.as_ptr());
        }
    }
}

#[derive(Debug)]
pub struct Value {
    raw: NonNull<FidanValue>,
}

impl Value {
    pub fn type_name(&self) -> String {
        let raw = unsafe { fidan_value_type_name(self.raw.as_ptr()) };
        let value = Value {
            raw: NonNull::new(raw).expect("type_name returned null"),
        };
        value.as_display_string()
    }

    pub fn as_display_string(&self) -> String {
        let raw = unsafe { fidan_value_to_string(self.raw.as_ptr()) };
        let value = Value {
            raw: NonNull::new(raw).expect("to_string returned null"),
        };
        value.as_string().unwrap_or_default()
    }

    pub fn as_i64(&self) -> i64 {
        unsafe { fidan_value_as_int(self.raw.as_ptr()) }
    }

    pub fn as_f64(&self) -> f64 {
        unsafe { fidan_value_as_float(self.raw.as_ptr()) }
    }

    pub fn as_bool(&self) -> bool {
        unsafe { fidan_value_as_bool(self.raw.as_ptr()) != 0 }
    }

    pub fn is_nothing(&self) -> bool {
        unsafe { fidan_value_is_nothing(self.raw.as_ptr()) != 0 }
    }

    pub fn as_string(&self) -> Option<String> {
        let len = unsafe { fidan_value_string_len(self.raw.as_ptr()) };
        let bytes = unsafe { fidan_value_string_bytes(self.raw.as_ptr()) };
        if bytes.is_null() {
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
        Some(String::from_utf8_lossy(slice).into_owned())
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        let raw = unsafe { fidan_value_clone(self.raw.as_ptr()) };
        Self {
            raw: NonNull::new(raw).expect("clone returned null"),
        }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        unsafe {
            fidan_value_free(self.raw.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fidan-embed-test-{unique}"));
        fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn safe_wrapper_eval_returns_result() {
        let mut vm = Vm::new();
        let value = vm.eval("var result = 123\n").expect("eval ok");
        assert_eq!(value.as_i64(), 123);
        assert_eq!(value.type_name(), "integer");
    }

    #[test]
    fn safe_wrapper_eval_file_resolves_relative_imports() {
        let dir = temp_dir();
        fs::write(
            dir.join("helper.fdn"),
            "action greet returns string {\n    return \"hi\"\n}\n",
        )
        .expect("write helper");
        fs::write(
            dir.join("main.fdn"),
            "use helper\nvar result = helper.greet()\n",
        )
        .expect("write main");

        let mut vm = Vm::new();
        let value = vm.eval_file(dir.join("main.fdn")).expect("eval file ok");
        assert_eq!(value.as_display_string(), "hi");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn safe_wrapper_surfaces_errors() {
        let mut vm = Vm::new();
        let err = vm
            .eval("var result =\n")
            .expect_err("expected parse failure");
        let text = err.to_string();
        assert!(!text.is_empty());
    }
}
