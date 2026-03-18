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
        "io" => io::dispatch(name, args).map(StdlibResult::Value),
        "math" => math::dispatch(name, args).map(StdlibResult::Value),
        "string" => string::dispatch(name, args).map(StdlibResult::Value),
        "collections" => collections::dispatch(name, args).map(StdlibResult::Value),
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
        "time" => time::dispatch(name, args).map(StdlibResult::Value),
        "regex" => regex::dispatch(name, args).map(StdlibResult::Value),
        _ => None,
    }
}

/// Returns true when `module` is a known stdlib module name.
pub fn is_stdlib_module(module: &str) -> bool {
    matches!(
        module,
        "io" | "math" | "string" | "collections" | "test" | "parallel" | "time"
    )
}

/// Returns all exported function names for a given stdlib module.
/// Used by `use std.module.{name}` to validate name lists at import resolution time.
pub fn module_exports(module: &str) -> &'static [&'static str] {
    match module {
        "io" => io::exported_names(),
        "math" => math::exported_names(),
        "string" => string::exported_names(),
        "collections" => collections::exported_names(),
        "test" => test_runner::exported_names(),
        "parallel" => parallel::exported_names(),
        "time" => time::exported_names(),
        _ => &[],
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
