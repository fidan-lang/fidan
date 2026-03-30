//! `fidan-stdlib` — Standard library implementations (Rust, callable from Fidan via FFI).
//!
//! # Import system
//!
//! Fidan's `use` statement resolves stdlib paths at interpreter startup:
//!
//! ```fidan
//! use std.io              # registers io.* as module namespace
//! use std.io.{readFile}   # injects readFile as a free builtin
//! use std.math            # registers math.* as module namespace  
//! ```
//!
//! The `StdlibRegistry` maps fully-qualified paths (e.g. `"std.math"`) to
//! `StdlibModule` descriptors. The MIR interpreter queries the registry when
//! it resolves `Callee::StdlibFn { module, name }` calls.

pub mod async_std;
pub mod collections;
pub mod io;
pub mod math;
pub mod metadata;
pub mod parallel;
pub mod regex;
pub mod sandbox;
pub mod string;
pub mod test_runner;
pub mod time;

/// A dispatched stdlib call result.
pub use sandbox::{SandboxPolicy, SandboxViolation};

/// A dispatched stdlib call result.
pub enum StdlibResult {
    /// Synchronous result value.
    Value(fidan_runtime::FidanValue),
    /// The call requires async/pending orchestration in the host runtime.
    NeedsAsyncDispatch(async_std::AsyncOp),
    /// The call requires callback dispatch (e.g. parallelMap needs MIR fn dispatch).
    /// Contains an opaque bytes payload — the parallel module's `ParallelOp`.
    NeedsCallbackDispatch(parallel::ParallelOp),
}

/// Dispatch a stdlib function call.
///
/// `module` is the canonical module name (e.g. `"io"`, `"math"`, `"string"`).
/// `name` is the function name within that module.
/// `args` is the argument list.
///
/// Returns `None` if no stdlib module matches.
/// Returns `Some(StdlibResult::Value(v))` for synchronous calls.
/// Returns `Some(StdlibResult::NeedsCallbackDispatch(op))` for parallel callbacks.
pub fn dispatch_stdlib(
    module: &str,
    name: &str,
    args: Vec<fidan_runtime::FidanValue>,
) -> Option<StdlibResult> {
    match module {
        "async" => async_std::dispatch(name, args).map(|result| match result {
            async_std::AsyncDispatch::Value(v) => StdlibResult::Value(v),
            async_std::AsyncDispatch::Op(op) => StdlibResult::NeedsAsyncDispatch(op),
        }),
        "test" => {
            test_runner::dispatch(name, args).map(|res| {
                match res {
                    Ok(v) => StdlibResult::Value(v),
                    // Assertion failures are converted to Nothing here; the MIR
                    // interpreter should check for assertion failure panics via
                    // the dedicated dispatch path.
                    Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                        fidan_runtime::FidanString::new(&format!("__test_fail__: {msg}")),
                    )),
                }
            })
        }
        "parallel" => parallel::dispatch_op(name, args).map(|res| match res {
            Ok(Some(op)) => StdlibResult::NeedsCallbackDispatch(op),
            Ok(None) => StdlibResult::Value(fidan_runtime::FidanValue::Nothing),
            Err(msg) => StdlibResult::Value(fidan_runtime::FidanValue::String(
                fidan_runtime::FidanString::new(&format!("__error__: {msg}")),
            )),
        }),
        _ => fidan_runtime::stdlib::dispatch_value_module(module, name, args)
            .map(StdlibResult::Value),
    }
}

/// Returns true when `module` is a known stdlib module name.
pub fn is_stdlib_module(module: &str) -> bool {
    module == "parallel" || fidan_runtime::stdlib::is_stdlib_module(module)
}

/// Returns all exported function names for a given stdlib module.
/// Used by `use std.module.{name}` to validate name lists at import resolution time.
pub fn module_exports(module: &str) -> &'static [&'static str] {
    match module {
        "test" => test_runner::exported_names(),
        "parallel" => parallel::exported_names(),
        _ => fidan_runtime::stdlib::module_exports(module),
    }
}

/// Dispatch a test assertion — returns `Err(failure_message)` on failure.
pub fn dispatch_test_assertion(
    name: &str,
    args: Vec<fidan_runtime::FidanValue>,
) -> Option<Result<fidan_runtime::FidanValue, String>> {
    test_runner::dispatch(name, args)
}

pub use metadata::{
    MathIntrinsic, StdlibIntrinsic, StdlibMethodInfo, StdlibValueKind, infer_receiver_method,
    infer_stdlib_method,
};
