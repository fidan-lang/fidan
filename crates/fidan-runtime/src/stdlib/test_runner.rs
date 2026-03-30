use crate::{FidanValue, display as format_val};

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<Result<FidanValue, String>> {
    match name {
        "assert" => {
            let cond = args.first().map(|v| v.truthy()).unwrap_or(false);
            if cond {
                Some(Ok(FidanValue::Nothing))
            } else {
                let msg = args
                    .get(1)
                    .map(format_val)
                    .unwrap_or_else(|| "assertion failed".to_string());
                Some(Err(msg))
            }
        }
        "assertEq" | "assert_eq" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if values_equal(&a, &b) {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected `{}` == `{}`",
                    format_val(&a),
                    format_val(&b)
                )))
            }
        }
        "assertNe" | "assert_ne" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if !values_equal(&a, &b) {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected `{}` != `{}`",
                    format_val(&a),
                    format_val(&b)
                )))
            }
        }
        "assertGt" | "assert_gt" => {
            let ok = cmp_vals(args.first(), args.get(1)) == Some(std::cmp::Ordering::Greater);
            if ok {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected `{}` > `{}`",
                    format_val(args.first().unwrap_or(&FidanValue::Nothing)),
                    format_val(args.get(1).unwrap_or(&FidanValue::Nothing))
                )))
            }
        }
        "assertLt" | "assert_lt" => {
            let ok = cmp_vals(args.first(), args.get(1)) == Some(std::cmp::Ordering::Less);
            if ok {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected `{}` < `{}`",
                    format_val(args.first().unwrap_or(&FidanValue::Nothing)),
                    format_val(args.get(1).unwrap_or(&FidanValue::Nothing))
                )))
            }
        }
        "assertSome" | "assert_some" => {
            let value = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if !value.is_nothing() {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err("expected a non-nothing value, got nothing".to_string()))
            }
        }
        "assertNothing" | "assert_nothing" => {
            let value = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if value.is_nothing() {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected nothing, got `{}`",
                    format_val(&value)
                )))
            }
        }
        "assertType" | "assert_type" => {
            let value = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let expected = match args.get(1) {
                Some(FidanValue::String(s)) => s.as_str().to_string(),
                _ => return Some(Ok(FidanValue::Nothing)),
            };
            let actual = value.type_name().to_string();
            if actual == expected {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!(
                    "expected type `{}`, got `{}`",
                    expected, actual
                )))
            }
        }
        "fail" => {
            let msg = args
                .first()
                .map(format_val)
                .unwrap_or_else(|| "test failed".to_string());
            Some(Err(msg))
        }
        "skip" => {
            let msg = args
                .first()
                .map(format_val)
                .unwrap_or_else(|| "skipped".to_string());
            eprintln!("  skip: {msg}");
            Some(Ok(FidanValue::Nothing))
        }
        _ => None,
    }
}

fn values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x == y,
        (FidanValue::Float(x), FidanValue::Float(y)) => (x - y).abs() < 1e-12,
        (FidanValue::Boolean(x), FidanValue::Boolean(y)) => x == y,
        (FidanValue::String(x), FidanValue::String(y)) => x.as_str() == y.as_str(),
        (FidanValue::Nothing, FidanValue::Nothing) => true,
        (FidanValue::Integer(x), FidanValue::Float(y)) => (*x as f64 - y).abs() < 1e-12,
        (FidanValue::Float(x), FidanValue::Integer(y)) => (x - *y as f64).abs() < 1e-12,
        _ => false,
    }
}

fn cmp_vals(a: Option<&FidanValue>, b: Option<&FidanValue>) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Some(FidanValue::Integer(x)), Some(FidanValue::Integer(y))) => Some(x.cmp(y)),
        (Some(FidanValue::Float(x)), Some(FidanValue::Float(y))) => x.partial_cmp(y),
        (Some(FidanValue::Integer(x)), Some(FidanValue::Float(y))) => (*x as f64).partial_cmp(y),
        (Some(FidanValue::Float(x)), Some(FidanValue::Integer(y))) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "assert",
        "assertEq",
        "assert_eq",
        "assertNe",
        "assert_ne",
        "assertGt",
        "assert_gt",
        "assertLt",
        "assert_lt",
        "assertSome",
        "assert_some",
        "assertNothing",
        "assert_nothing",
        "assertType",
        "assert_type",
        "fail",
        "skip",
    ]
}
