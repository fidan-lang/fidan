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
                Some(Err(args.get(2).map(format_val).unwrap_or_else(|| {
                    format!("expected `{}` == `{}`", format_val(&a), format_val(&b))
                })))
            }
        }
        "assertNe" | "assert_ne" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if !values_equal(&a, &b) {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(args.get(2).map(format_val).unwrap_or_else(|| {
                    format!("expected `{}` != `{}`", format_val(&a), format_val(&b))
                })))
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
    crate::ffi::values_equal_with(a, b, |x, y| x == y || (x - y).abs() < 1e-12)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdlib::common::list_value;

    #[test]
    fn equality_assertions_honor_optional_messages() {
        for name in ["assertEq", "assert_eq", "assertNe", "assert_ne"] {
            let eq = matches!(name, "assertEq" | "assert_eq");
            let arguments = vec![
                FidanValue::Integer(1),
                FidanValue::Integer(if eq { 2 } else { 1 }),
            ];
            assert_eq!(
                dispatch(name, arguments.clone()).unwrap().unwrap_err(),
                if eq {
                    "expected `1` == `2`"
                } else {
                    "expected `1` != `1`"
                }
            );
            for (message, expected) in [
                (
                    FidanValue::String(crate::FidanString::new("custom")),
                    "custom",
                ),
                (FidanValue::String(crate::FidanString::new("")), ""),
                (FidanValue::Nothing, "nothing"),
            ] {
                let mut arguments = arguments.clone();
                arguments.push(message);
                assert_eq!(dispatch(name, arguments).unwrap().unwrap_err(), expected);
            }
            let close_floats = vec![
                FidanValue::Float(1.0),
                FidanValue::Float(1.0 + 5e-13),
                FidanValue::String(crate::FidanString::new("custom")),
            ];
            let result = dispatch(name, close_floats).unwrap();
            if eq {
                assert!(matches!(result, Ok(FidanValue::Nothing)));
            } else {
                assert_eq!(result.unwrap_err(), "custom");
            }
        }
    }

    #[test]
    fn float_tolerance_is_recursive_and_separate_from_value_equality() {
        let left = FidanValue::Float(1.0);
        let right = FidanValue::Float(1.0 + 5e-13);
        assert!(values_equal(&left, &right));
        let left = list_value([FidanValue::Tuple(vec![left])]);
        let right = list_value([FidanValue::Tuple(vec![right])]);
        assert!(values_equal(&left, &right));
        assert!(!crate::ffi::values_equal(&left, &right));
        let mut left_dict = crate::FidanDict::new();
        let mut right_dict = crate::FidanDict::new();
        let key = FidanValue::String(crate::FidanString::new("nested"));
        left_dict.insert(key.clone(), left).unwrap();
        right_dict.insert(key, right).unwrap();
        assert!(values_equal(
            &FidanValue::Dict(crate::OwnedRef::new(left_dict)),
            &FidanValue::Dict(crate::OwnedRef::new(right_dict))
        ));
    }

    #[test]
    fn assertions_compare_nested_collections_structurally() {
        let lhs = list_value([FidanValue::Tuple(vec![
            FidanValue::Integer(42),
            FidanValue::Nothing,
        ])]);
        let rhs = list_value([FidanValue::Tuple(vec![
            FidanValue::Integer(42),
            FidanValue::Nothing,
        ])]);
        assert!(
            dispatch("assertEq", vec![lhs.clone(), rhs])
                .unwrap()
                .is_ok()
        );
        assert!(
            dispatch("assertNe", vec![lhs, list_value([])])
                .unwrap()
                .is_ok()
        );
    }
}
