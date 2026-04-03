use std::ffi::{CString, c_char, c_void};
use std::sync::{Arc, LazyLock};

use fidan_mir::{MirExternAbi, MirExternDecl, MirFunction, MirParam, MirTy};
use fidan_runtime::FidanValue;
use libffi::middle::{Cif, CodePtr, Type, arg};
use parking_lot::RwLock;
use rustc_hash::FxHashMap;

static LIBRARIES: LazyLock<RwLock<FxHashMap<String, Arc<ExternLibrary>>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));
static SELF_SYMBOLS: LazyLock<RwLock<FxHashMap<String, usize>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeKind {
    Integer,
    Float,
    Boolean,
    Handle,
    Nothing,
}

struct ExternLibrary {
    handle: *mut c_void,
    owned: bool,
    symbols: RwLock<FxHashMap<String, *mut c_void>>,
}

unsafe impl Send for ExternLibrary {}
unsafe impl Sync for ExternLibrary {}

impl Drop for ExternLibrary {
    fn drop(&mut self) {
        if self.owned {
            unsafe { platform::close_library(self.handle) };
        }
    }
}

impl ExternLibrary {
    fn open(lib: &str) -> Result<Self, String> {
        if lib.eq_ignore_ascii_case("self") {
            return Ok(Self {
                handle: unsafe { platform::current_process()? },
                owned: false,
                symbols: RwLock::new(FxHashMap::default()),
            });
        }
        let mut last_error = None;
        for candidate in library_candidates(lib) {
            let c = CString::new(candidate.clone())
                .map_err(|_| format!("library identifier `{lib}` contains an interior NUL byte"))?;
            match unsafe { platform::open_library(c.as_ptr()) } {
                Ok(handle) => {
                    return Ok(Self {
                        handle,
                        owned: true,
                        symbols: RwLock::new(FxHashMap::default()),
                    });
                }
                Err(err) => last_error = Some(format!("{candidate}: {err}")),
            }
        }
        Err(last_error.unwrap_or_else(|| format!("could not load library `{lib}`")))
    }

    fn resolve(&self, symbol: &str) -> Result<*mut c_void, String> {
        if let Some(ptr) = SELF_SYMBOLS.read().get(symbol).copied() {
            return Ok(ptr as *mut c_void);
        }
        if let Some(ptr) = self.symbols.read().get(symbol).copied() {
            return Ok(ptr);
        }
        let c = CString::new(symbol)
            .map_err(|_| format!("symbol `{symbol}` contains an interior NUL byte"))?;
        let ptr = unsafe { platform::load_symbol(self.handle, c.as_ptr()) }
            .map_err(|err| format!("could not resolve symbol `{symbol}`: {err}"))?;
        self.symbols.write().insert(symbol.to_string(), ptr);
        Ok(ptr)
    }
}

pub fn call_extern(function: &MirFunction, args: Vec<FidanValue>) -> Result<FidanValue, String> {
    let decl = function
        .extern_decl
        .as_ref()
        .ok_or_else(|| "missing extern metadata".to_string())?;
    match decl.abi {
        MirExternAbi::Fidan => call_fidan_abi(decl, args),
        MirExternAbi::Native => call_native_abi(decl, &function.params, &function.return_ty, args),
    }
}

pub fn register_self_symbol(name: &str, symbol: *mut c_void) {
    SELF_SYMBOLS
        .write()
        .insert(name.to_string(), symbol as usize);
}

fn load_library(lib: &str) -> Result<Arc<ExternLibrary>, String> {
    if let Some(existing) = LIBRARIES.read().get(lib).cloned() {
        return Ok(existing);
    }
    let library = Arc::new(ExternLibrary::open(lib)?);
    LIBRARIES
        .write()
        .insert(lib.to_string(), Arc::clone(&library));
    Ok(library)
}

fn call_fidan_abi(decl: &MirExternDecl, args: Vec<FidanValue>) -> Result<FidanValue, String> {
    let library = load_library(&decl.lib)?;
    let symbol = library.resolve(&decl.symbol)?;
    type RawExtern = unsafe extern "C" fn(*const *mut FidanValue, i64) -> *mut FidanValue;
    let func: RawExtern = unsafe { std::mem::transmute(symbol) };

    let mut raw_args: Vec<*mut FidanValue> = args
        .into_iter()
        .map(|value| Box::into_raw(Box::new(value)))
        .collect();
    let result = unsafe { func(raw_args.as_ptr(), raw_args.len() as i64) };
    for ptr in raw_args.drain(..) {
        unsafe { drop(Box::from_raw(ptr)) };
    }
    take_fidan_abi_result(&library, result)
}

fn take_fidan_abi_result(
    library: &ExternLibrary,
    result: *mut FidanValue,
) -> Result<FidanValue, String> {
    if result.is_null() {
        return Ok(FidanValue::Nothing);
    }

    if !library.owned {
        return Ok(unsafe { *Box::from_raw(result) });
    }

    // External Rust plugins may allocate the returned box with a different allocator
    // than the host process (e.g. the CLI uses mimalloc). Clone the foreign value into
    // host-owned memory first, then ask the foreign library to free its own box.
    let cloned = unsafe { (&*result).clone() };
    if let Ok(drop_symbol) = library.resolve("fdn_drop") {
        type RawDrop = unsafe extern "C" fn(*mut FidanValue);
        let drop_fn: RawDrop = unsafe { std::mem::transmute(drop_symbol) };
        unsafe { drop_fn(result) };
    }
    Ok(cloned)
}

fn call_native_abi(
    decl: &MirExternDecl,
    params: &[MirParam],
    return_ty: &MirTy,
    args: Vec<FidanValue>,
) -> Result<FidanValue, String> {
    let library = load_library(&decl.lib)?;
    let symbol = library.resolve(&decl.symbol)?;
    let param_kinds: Vec<NativeKind> = params
        .iter()
        .map(|param| native_kind(&param.ty))
        .collect::<Result<_, _>>()?;
    let ret_kind = native_kind(return_ty)?;
    let storage = param_kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| NativeArgValue::from_fidan(*kind, &args, index))
        .collect::<Result<Vec<_>, _>>()?;
    let ffi_args = storage
        .iter()
        .map(NativeArgValue::as_ffi_arg)
        .collect::<Vec<_>>();
    let cif = Cif::new(
        param_kinds.iter().map(|kind| native_kind_ffi_type(*kind)),
        native_kind_ffi_type(ret_kind),
    );
    let code_ptr = CodePtr(symbol);

    unsafe {
        match ret_kind {
            NativeKind::Integer => Ok(FidanValue::Integer(cif.call(code_ptr, &ffi_args))),
            NativeKind::Float => Ok(FidanValue::Float(cif.call(code_ptr, &ffi_args))),
            NativeKind::Boolean => Ok(FidanValue::Boolean(
                cif.call::<i8>(code_ptr, &ffi_args) != 0,
            )),
            NativeKind::Handle => Ok(FidanValue::Handle(cif.call(code_ptr, &ffi_args))),
            NativeKind::Nothing => {
                cif.call::<()>(code_ptr, &ffi_args);
                Ok(FidanValue::Nothing)
            }
        }
    }
}

fn native_kind(ty: &MirTy) -> Result<NativeKind, String> {
    match ty {
        MirTy::Integer => Ok(NativeKind::Integer),
        MirTy::Float => Ok(NativeKind::Float),
        MirTy::Boolean => Ok(NativeKind::Boolean),
        MirTy::Handle => Ok(NativeKind::Handle),
        MirTy::Nothing => Ok(NativeKind::Nothing),
        other => Err(format!(
            "unsupported native @extern type `{}` in interpreter",
            display_mir_ty(other)
        )),
    }
}

fn display_mir_ty(ty: &MirTy) -> String {
    match ty {
        MirTy::Integer => "integer".to_string(),
        MirTy::Float => "float".to_string(),
        MirTy::Boolean => "boolean".to_string(),
        MirTy::String => "string".to_string(),
        MirTy::Nothing => "nothing".to_string(),
        MirTy::Dynamic => "dynamic".to_string(),
        MirTy::Handle => "handle".to_string(),
        MirTy::List(inner) => format!("list<{}>", display_mir_ty(inner)),
        MirTy::Dict(k, v) => format!("dict<{}, {}>", display_mir_ty(k), display_mir_ty(v)),
        MirTy::Tuple(_) => "tuple".to_string(),
        MirTy::Object(_) => "object".to_string(),
        MirTy::Enum(_) => "enum".to_string(),
        MirTy::Shared(inner) => format!("Shared<{}>", display_mir_ty(inner)),
        MirTy::WeakShared(inner) => format!("WeakShared<{}>", display_mir_ty(inner)),
        MirTy::Pending(inner) => format!("Pending<{}>", display_mir_ty(inner)),
        MirTy::Function => "action".to_string(),
        MirTy::Error => "<error>".to_string(),
    }
}

enum NativeArgValue {
    Integer(i64),
    Float(f64),
    Boolean(i8),
    Handle(usize),
}

impl NativeArgValue {
    fn from_fidan(kind: NativeKind, args: &[FidanValue], index: usize) -> Result<Self, String> {
        match kind {
            NativeKind::Integer => Ok(Self::Integer(arg_as_i64(args, index)?)),
            NativeKind::Float => Ok(Self::Float(arg_as_f64(args, index)?)),
            NativeKind::Boolean => Ok(Self::Boolean(arg_as_i8(args, index)?)),
            NativeKind::Handle => Ok(Self::Handle(arg_as_usize(args, index)?)),
            NativeKind::Nothing => {
                Err("native @extern parameters cannot use type `nothing`".to_string())
            }
        }
    }

    fn as_ffi_arg(&self) -> libffi::middle::Arg<'_> {
        match self {
            Self::Integer(value) => arg(value),
            Self::Float(value) => arg(value),
            Self::Boolean(value) => arg(value),
            Self::Handle(value) => arg(value),
        }
    }
}

fn native_kind_ffi_type(kind: NativeKind) -> Type {
    match kind {
        NativeKind::Integer => Type::i64(),
        NativeKind::Float => Type::f64(),
        NativeKind::Boolean => Type::i8(),
        NativeKind::Handle => Type::usize(),
        NativeKind::Nothing => Type::void(),
    }
}

fn arg_as_i64(args: &[FidanValue], index: usize) -> Result<i64, String> {
    match args.get(index) {
        Some(FidanValue::Integer(n)) => Ok(*n),
        Some(FidanValue::Float(f)) => Ok(*f as i64),
        Some(FidanValue::Boolean(b)) => Ok(i64::from(*b)),
        Some(FidanValue::Handle(h)) => Ok(*h as i64),
        Some(other) => Err(format!(
            "argument {} cannot be passed as integer (got `{}`)",
            index,
            other.type_name()
        )),
        None => Err(format!("missing argument {index}")),
    }
}

fn arg_as_f64(args: &[FidanValue], index: usize) -> Result<f64, String> {
    match args.get(index) {
        Some(FidanValue::Float(f)) => Ok(*f),
        Some(FidanValue::Integer(n)) => Ok(*n as f64),
        Some(other) => Err(format!(
            "argument {} cannot be passed as float (got `{}`)",
            index,
            other.type_name()
        )),
        None => Err(format!("missing argument {index}")),
    }
}

fn arg_as_i8(args: &[FidanValue], index: usize) -> Result<i8, String> {
    match args.get(index) {
        Some(FidanValue::Boolean(b)) => Ok(i8::from(*b)),
        Some(FidanValue::Integer(n)) => Ok((*n != 0) as i8),
        Some(other) => Err(format!(
            "argument {} cannot be passed as boolean (got `{}`)",
            index,
            other.type_name()
        )),
        None => Err(format!("missing argument {index}")),
    }
}

fn arg_as_usize(args: &[FidanValue], index: usize) -> Result<usize, String> {
    match args.get(index) {
        Some(FidanValue::Handle(h)) => Ok(*h),
        Some(FidanValue::Integer(n)) if *n >= 0 => Ok(*n as usize),
        Some(other) => Err(format!(
            "argument {} cannot be passed as handle (got `{}`)",
            index,
            other.type_name()
        )),
        None => Err(format!("missing argument {index}")),
    }
}

fn library_candidates(lib: &str) -> Vec<String> {
    let mut out = vec![lib.to_string()];
    let has_sep = lib.contains('/') || lib.contains('\\');
    let has_ext = lib.contains('.');
    if !has_sep && !has_ext {
        #[cfg(windows)]
        out.push(format!("{lib}.dll"));
        #[cfg(target_os = "macos")]
        out.push(format!("lib{lib}.dylib"));
        #[cfg(all(unix, not(target_os = "macos")))]
        out.push(format!("lib{lib}.so"));
    }
    out
}

#[cfg(windows)]
mod platform {
    use super::{c_char, c_void};

    unsafe extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        fn FreeLibrary(handle: *mut c_void) -> i32;
    }

    pub unsafe fn open_library(name: *const c_char) -> Result<*mut c_void, String> {
        let handle = unsafe { LoadLibraryA(name) };
        if handle.is_null() {
            Err("LoadLibraryA failed".to_string())
        } else {
            Ok(handle)
        }
    }

    pub unsafe fn load_symbol(
        handle: *mut c_void,
        name: *const c_char,
    ) -> Result<*mut c_void, String> {
        let ptr = unsafe { GetProcAddress(handle, name) };
        if ptr.is_null() {
            Err("GetProcAddress failed".to_string())
        } else {
            Ok(ptr)
        }
    }

    pub unsafe fn current_process() -> Result<*mut c_void, String> {
        let handle = unsafe { GetModuleHandleA(std::ptr::null()) };
        if handle.is_null() {
            Err("GetModuleHandleA(NULL) failed".to_string())
        } else {
            Ok(handle)
        }
    }

    pub unsafe fn close_library(handle: *mut c_void) {
        if !handle.is_null() {
            let _ = unsafe { FreeLibrary(handle) };
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::{c_char, c_void};
    use std::ffi::CStr;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> i32;
        fn dlerror() -> *const c_char;
    }

    const RTLD_NOW: i32 = 2;

    fn last_error() -> String {
        unsafe {
            let ptr = dlerror();
            if ptr.is_null() {
                "dynamic loader error".to_string()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        }
    }

    pub unsafe fn open_library(name: *const c_char) -> Result<*mut c_void, String> {
        let handle = unsafe { dlopen(name, RTLD_NOW) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(handle)
        }
    }

    pub unsafe fn load_symbol(
        handle: *mut c_void,
        name: *const c_char,
    ) -> Result<*mut c_void, String> {
        let ptr = unsafe { dlsym(handle, name) };
        if ptr.is_null() {
            Err(last_error())
        } else {
            Ok(ptr)
        }
    }

    pub unsafe fn current_process() -> Result<*mut c_void, String> {
        let handle = unsafe { dlopen(std::ptr::null(), RTLD_NOW) };
        if handle.is_null() {
            Err(last_error())
        } else {
            Ok(handle)
        }
    }

    pub unsafe fn close_library(handle: *mut c_void) {
        if !handle.is_null() {
            let _ = unsafe { dlclose(handle) };
        }
    }
}
