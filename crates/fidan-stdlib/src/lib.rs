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

#[derive(Clone, Copy)]
pub struct StdlibModuleInfo {
    pub name: &'static str,
    pub exports: fn() -> &'static [&'static str],
    pub doc: &'static str,
}

pub const STDLIB_MODULES: &[StdlibModuleInfo] = &[
    StdlibModuleInfo {
        name: "async",
        exports: fidan_runtime::stdlib::async_std::exported_names,
        doc: "Same-thread async helpers like sleep, gather, waitAny, and timeout.",
    },
    StdlibModuleInfo {
        name: "collections",
        exports: fidan_runtime::stdlib::collections::exported_names,
        doc: "Collection helpers like zip, enumerate, chunk, window, partition, and groupBy.",
    },
    StdlibModuleInfo {
        name: "env",
        exports: fidan_runtime::stdlib::env::exported_names,
        doc: "Environment variables and process arguments.",
    },
    StdlibModuleInfo {
        name: "io",
        exports: fidan_runtime::stdlib::io::exported_names,
        doc: "Printing, input, file I/O, paths, directories, and terminal helpers.",
    },
    StdlibModuleInfo {
        name: "math",
        exports: fidan_runtime::stdlib::math::exported_names,
        doc: "Math functions, constants, random helpers, and numeric transforms.",
    },
    StdlibModuleInfo {
        name: "parallel",
        exports: parallel::exported_names,
        doc: "Thread-backed parallel collection helpers.",
    },
    StdlibModuleInfo {
        name: "regex",
        exports: fidan_runtime::stdlib::regex::exported_names,
        doc: "Regex compile, match, capture, replace, and split helpers.",
    },
    StdlibModuleInfo {
        name: "string",
        exports: fidan_runtime::stdlib::string::exported_names,
        doc: "String transforms, parsing, slicing, casing, and character helpers.",
    },
    StdlibModuleInfo {
        name: "test",
        exports: test_runner::exported_names,
        doc: "Assertion helpers used by `fidan test` and inline test blocks.",
    },
    StdlibModuleInfo {
        name: "time",
        exports: fidan_runtime::stdlib::time::exported_names,
        doc: "Clocks, elapsed timing, sleep/wait, and date/time helpers.",
    },
];

pub fn module_info(module: &str) -> Option<&'static StdlibModuleInfo> {
    STDLIB_MODULES.iter().find(|info| info.name == module)
}

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
    module_info(module).is_some()
}

/// Returns all exported function names for a given stdlib module.
/// Used by `use std.module.{name}` to validate name lists at import resolution time.
pub fn module_exports(module: &str) -> &'static [&'static str] {
    module_info(module)
        .map(|info| (info.exports)())
        .unwrap_or(&[])
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
