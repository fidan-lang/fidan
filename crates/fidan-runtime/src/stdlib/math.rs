use crate::FidanValue;

fn arg0(args: &[FidanValue]) -> f64 {
    match args.first() {
        Some(FidanValue::Float(f)) => *f,
        Some(FidanValue::Integer(n)) => *n as f64,
        _ => 0.0,
    }
}

fn arg1(args: &[FidanValue]) -> f64 {
    match args.get(1) {
        Some(FidanValue::Float(f)) => *f,
        Some(FidanValue::Integer(n)) => *n as f64,
        _ => 0.0,
    }
}

fn float_val(v: f64) -> FidanValue {
    FidanValue::Float(v)
}

fn to_f64(v: &FidanValue) -> f64 {
    match v {
        FidanValue::Float(f) => *f,
        FidanValue::Integer(n) => *n as f64,
        _ => 0.0,
    }
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "sin" => Some(float_val(arg0(&args).sin())),
        "cos" => Some(float_val(arg0(&args).cos())),
        "tan" => Some(float_val(arg0(&args).tan())),
        "asin" => Some(float_val(arg0(&args).asin())),
        "acos" => Some(float_val(arg0(&args).acos())),
        "atan" => Some(float_val(arg0(&args).atan())),
        "atan2" => Some(float_val(arg0(&args).atan2(arg1(&args)))),
        "sinh" => Some(float_val(arg0(&args).sinh())),
        "cosh" => Some(float_val(arg0(&args).cosh())),
        "tanh" => Some(float_val(arg0(&args).tanh())),
        "sqrt" => Some(float_val(arg0(&args).sqrt())),
        "cbrt" => Some(float_val(arg0(&args).cbrt())),
        "pow" => Some(float_val(arg0(&args).powf(arg1(&args)))),
        "exp" => Some(float_val(arg0(&args).exp())),
        "exp2" => Some(float_val(arg0(&args).exp2())),
        "log" => Some(float_val(arg0(&args).ln())),
        "log2" => Some(float_val(arg0(&args).log2())),
        "log10" => Some(float_val(arg0(&args).log10())),
        "logN" | "log_n" => Some(float_val(arg0(&args).log(arg1(&args)))),
        "floor" => Some(FidanValue::Integer(arg0(&args).floor() as i64)),
        "ceil" => Some(FidanValue::Integer(arg0(&args).ceil() as i64)),
        "round" => Some(FidanValue::Integer(arg0(&args).round() as i64)),
        "trunc" => Some(float_val(arg0(&args).trunc())),
        "fract" => Some(float_val(arg0(&args).fract())),
        "abs" => match args.first() {
            Some(FidanValue::Integer(n)) => Some(FidanValue::Integer(n.abs())),
            Some(FidanValue::Float(f)) => Some(float_val(f.abs())),
            _ => Some(FidanValue::Nothing),
        },
        "sign" | "signum" => match args.first() {
            Some(FidanValue::Integer(n)) => Some(FidanValue::Integer(n.signum())),
            Some(FidanValue::Float(f)) => Some(float_val(f.signum())),
            _ => Some(FidanValue::Nothing),
        },
        "min" => match (args.first(), args.get(1)) {
            (Some(FidanValue::Integer(a)), Some(FidanValue::Integer(b))) => {
                Some(FidanValue::Integer(*a.min(b)))
            }
            (Some(a), Some(b)) => Some(float_val(to_f64(a).min(to_f64(b)))),
            _ => Some(FidanValue::Nothing),
        },
        "max" => match (args.first(), args.get(1)) {
            (Some(FidanValue::Integer(a)), Some(FidanValue::Integer(b))) => {
                Some(FidanValue::Integer(*a.max(b)))
            }
            (Some(a), Some(b)) => Some(float_val(to_f64(a).max(to_f64(b)))),
            _ => Some(FidanValue::Nothing),
        },
        "clamp" => {
            let lo = arg1(&args);
            let hi = match args.get(2) {
                Some(FidanValue::Float(f)) => *f,
                Some(FidanValue::Integer(n)) => *n as f64,
                _ => f64::MAX,
            };
            Some(float_val(arg0(&args).clamp(lo, hi)))
        }
        "hypot" => Some(float_val(arg0(&args).hypot(arg1(&args)))),
        "pi" | "PI" => Some(float_val(std::f64::consts::PI)),
        "e" | "E" => Some(float_val(std::f64::consts::E)),
        "tau" | "TAU" => Some(float_val(std::f64::consts::TAU)),
        "inf" | "infinity" => Some(float_val(f64::INFINITY)),
        "nan" | "NaN" => Some(float_val(f64::NAN)),
        "isNan" | "isNaN" | "is_nan" => Some(FidanValue::Boolean(arg0(&args).is_nan())),
        "isInfinite" | "is_infinite" => Some(FidanValue::Boolean(arg0(&args).is_infinite())),
        "isFinite" | "is_finite" => Some(FidanValue::Boolean(arg0(&args).is_finite())),
        "random" => {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(12345);
            let lcg = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            Some(float_val((lcg as f64) / (u32::MAX as f64)))
        }
        "randomInt" | "random_int" => {
            let lo = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let hi = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n,
                _ => 100,
            };
            if hi <= lo {
                return Some(FidanValue::Integer(lo));
            }
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(42);
            let lcg = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let range = (hi - lo) as u64;
            Some(FidanValue::Integer(
                lo + (lcg as i64).unsigned_abs() as i64 % range as i64,
            ))
        }
        "toDeg" | "to_deg" | "degrees" => Some(float_val(arg0(&args).to_degrees())),
        "toRad" | "to_rad" | "radians" => Some(float_val(arg0(&args).to_radians())),
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "atan2",
        "sinh",
        "cosh",
        "tanh",
        "sqrt",
        "cbrt",
        "pow",
        "exp",
        "exp2",
        "log",
        "log2",
        "log10",
        "logN",
        "log_n",
        "floor",
        "ceil",
        "round",
        "trunc",
        "fract",
        "abs",
        "sign",
        "signum",
        "min",
        "max",
        "clamp",
        "hypot",
        "pi",
        "PI",
        "e",
        "E",
        "tau",
        "TAU",
        "inf",
        "infinity",
        "nan",
        "NaN",
        "isNan",
        "isNaN",
        "is_nan",
        "isInfinite",
        "is_infinite",
        "isFinite",
        "is_finite",
        "random",
        "randomInt",
        "random_int",
        "toDeg",
        "to_deg",
        "degrees",
        "toRad",
        "to_rad",
        "radians",
    ]
}
