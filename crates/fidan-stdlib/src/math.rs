//! `std.math` — Mathematical functions for Fidan.
//!
//! Exposed as free functions in the `math` namespace:
//!   `use std.math`  → `math.sin(x)`, `math.pi()`, etc.
//!   `use std.math.{sin, cos}` → `sin(x)`, `cos(x)` directly.

use crate::{MathIntrinsic, StdlibIntrinsic, StdlibMethodInfo, StdlibValueKind};
use fidan_runtime::FidanValue;

/// Dispatch a `math.<name>(args)` call.
/// Returns `None` if the function name is unknown.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    fidan_runtime::stdlib::math::dispatch(name, args)
}

/// All exported function names from `std.math` (used by import resolution).
pub fn exported_names() -> &'static [&'static str] {
    fidan_runtime::stdlib::math::exported_names()
}

pub fn method_info(name: &str, arg_kinds: &[StdlibValueKind]) -> Option<StdlibMethodInfo> {
    use StdlibValueKind as Kind;

    let first = arg_kinds.first().copied().unwrap_or(Kind::Dynamic);
    let second = arg_kinds.get(1).copied().unwrap_or(Kind::Dynamic);
    let info = |return_kind, intrinsic| StdlibMethodInfo {
        return_kind,
        intrinsic,
    };

    match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2" | "sinh" | "cosh" | "tanh"
        | "sqrt" | "cbrt" | "pow" | "exp" | "exp2" | "log" | "log2" | "log10" | "logN"
        | "fract" | "hypot" | "pi" | "e" | "tau" | "inf" | "nan" | "random" | "toDeg"
        | "to_deg" | "degrees" | "toRad" | "to_rad" | "radians" => Some(info(
            Kind::Float,
            if name == "sqrt" {
                Some(StdlibIntrinsic::Math(MathIntrinsic::Sqrt))
            } else {
                None
            },
        )),
        "floor" => Some(info(
            Kind::Integer,
            Some(StdlibIntrinsic::Math(MathIntrinsic::Floor)),
        )),
        "ceil" => Some(info(
            Kind::Integer,
            Some(StdlibIntrinsic::Math(MathIntrinsic::Ceil)),
        )),
        "round" | "randomInt" | "random_int" => Some(info(Kind::Integer, None)),
        "trunc" => Some(info(
            Kind::Float,
            Some(StdlibIntrinsic::Math(MathIntrinsic::Trunc)),
        )),
        "abs" => Some(match first {
            Kind::Integer => info(
                Kind::Integer,
                Some(StdlibIntrinsic::Math(MathIntrinsic::Abs)),
            ),
            Kind::Float => info(Kind::Float, Some(StdlibIntrinsic::Math(MathIntrinsic::Abs))),
            _ => info(Kind::Dynamic, None),
        }),
        "sign" | "signum" => Some(match first {
            Kind::Integer => info(Kind::Integer, None),
            Kind::Float => info(Kind::Float, None),
            _ => info(Kind::Dynamic, None),
        }),
        "min" | "max" => Some(match (first, second) {
            (Kind::Integer, Kind::Integer) => info(Kind::Integer, None),
            (Kind::Dynamic, _) | (_, Kind::Dynamic) => info(Kind::Dynamic, None),
            _ => info(Kind::Float, None),
        }),
        "clamp" => Some(info(Kind::Float, None)),
        "isNan" | "is_nan" | "isInfinite" | "is_infinite" | "isFinite" | "is_finite" => {
            Some(info(Kind::Boolean, None))
        }
        _ => None,
    }
}
