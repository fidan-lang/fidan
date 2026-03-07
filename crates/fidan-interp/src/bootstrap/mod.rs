//! Bootstrap method dispatch — single entry point that routes to per-type files.
//!
//! These are placeholder implementations that make receiver methods work before
//! Phase 7 stdlib (`std.string`, `std.collections`, `std.math`) is built.
//! Once a stdlib module defines an extension action for a method, the normal
//! extension-action dispatch fires first and these become unreachable.

pub mod dict_methods;
pub mod list_methods;
pub mod numeric_methods;
pub mod range_methods;
pub mod string_methods;

use fidan_runtime::FidanValue;

/// Dispatch `receiver.method(args)` to the correct bootstrap implementation.
///
/// Returns `Some(value)` when a bootstrap method handles the call,
/// `None` when no handler exists (caller should error with "method not found").
pub fn call_bootstrap_method(
    receiver: FidanValue,
    method: &str,
    args: Vec<FidanValue>,
) -> Option<FidanValue> {
    match receiver {
        FidanValue::String(s) => string_methods::dispatch(s, method, args),
        FidanValue::List(l) => list_methods::dispatch(l, method, args),
        FidanValue::Dict(d) => dict_methods::dispatch(d, method, args),
        v @ (FidanValue::Integer(_) | FidanValue::Float(_)) => numeric_methods::dispatch(v, method),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => range_methods::dispatch(start, end, inclusive, method, args),
        _ => None,
    }
}
