//! Bootstrap numeric receiver methods — placeholder until `std.math` (Phase 7).
//!
//! These allow `x.abs()`, `f.sqrt()`, `f.floor()`, etc. on numeric values.
//! Free-standing forms (`abs(x)`, `sqrt(x)`) remain in `builtins::call_builtin`.

use fidan_runtime::FidanValue;

/// Dispatch a method call on an integer or float receiver.
/// `receiver` is guaranteed to be `Integer` or `Float` by the caller.
pub fn dispatch(receiver: FidanValue, method: &str) -> Option<FidanValue> {
    match (receiver, method) {
        (FidanValue::Integer(n), "abs") => Some(FidanValue::Integer(n.abs())),
        (FidanValue::Float(f), "abs") => Some(FidanValue::Float(f.abs())),
        (FidanValue::Integer(n), "sqrt") => Some(FidanValue::Float((n as f64).sqrt())),
        (FidanValue::Float(f), "sqrt") => Some(FidanValue::Float(f.sqrt())),
        (FidanValue::Float(f), "floor") => Some(FidanValue::Integer(f.floor() as i64)),
        (FidanValue::Float(f), "ceil") => Some(FidanValue::Integer(f.ceil() as i64)),
        (FidanValue::Float(f), "round") => Some(FidanValue::Integer(f.round() as i64)),
        (FidanValue::Integer(n), "to_float") => Some(FidanValue::Float(n as f64)),
        (FidanValue::Float(f), "to_int") => Some(FidanValue::Integer(f as i64)),
        _ => None,
    }
}
