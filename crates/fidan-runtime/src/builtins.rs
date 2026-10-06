//! Shared core conversion and length contracts for interpreted and native calls.
use crate::{FidanValue, display, stdlib::StdlibRuntimeError};
use fidan_diagnostics::diag_code;

fn invalid_conversion(target: &str, value: &FidanValue) -> StdlibRuntimeError {
    let rendered = match value {
        FidanValue::String(s) => format!("{:?}", s.as_str()),
        other => display(other),
    };
    StdlibRuntimeError::new(
        diag_code!("R0001"),
        format!(
            "cannot convert {rendered} ({}) to {target}",
            value.type_name()
        ),
    )
}

pub fn integer(value: &FidanValue) -> Result<FidanValue, StdlibRuntimeError> {
    Ok(match value {
        FidanValue::Integer(n) => FidanValue::Integer(*n),
        FidanValue::Float(f) => FidanValue::Integer(*f as i64),
        FidanValue::Boolean(b) => FidanValue::Integer(i64::from(*b)),
        FidanValue::String(s) => FidanValue::Integer(
            s.as_str()
                .parse()
                .map_err(|_| invalid_conversion("integer", value))?,
        ),
        _ => return Err(invalid_conversion("integer", value)),
    })
}

pub fn float(value: &FidanValue) -> Result<FidanValue, StdlibRuntimeError> {
    Ok(match value {
        FidanValue::Float(f) => FidanValue::Float(*f),
        FidanValue::Integer(n) => FidanValue::Float(*n as f64),
        FidanValue::String(s) => FidanValue::Float(
            s.as_str()
                .parse()
                .map_err(|_| invalid_conversion("float", value))?,
        ),
        _ => return Err(invalid_conversion("float", value)),
    })
}

pub fn len(value: &FidanValue) -> Result<i64, StdlibRuntimeError> {
    Ok(match value {
        FidanValue::String(s) => s.char_len() as i64,
        FidanValue::List(l) => l.borrow().len() as i64,
        FidanValue::Dict(d) => d.borrow().len() as i64,
        FidanValue::HashSet(s) => s.borrow().len() as i64,
        FidanValue::Tuple(t) => t.len() as i64,
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => crate::range_length(*start, *end, *inclusive)
            .map_err(|message| StdlibRuntimeError::new(diag_code!("R0001"), message))?,
        _ => {
            return Err(StdlibRuntimeError::new(
                diag_code!("R0001"),
                format!("len() is not supported for {}", value.type_name()),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FidanString;

    #[test]
    fn conversions_and_lengths_preserve_interpreter_contract() {
        let text = |s| FidanValue::String(FidanString::new(s));
        assert!(matches!(
            integer(&text("123")),
            Ok(FidanValue::Integer(123))
        ));
        assert!(matches!(
            integer(&FidanValue::Boolean(true)),
            Ok(FidanValue::Integer(1))
        ));
        assert!(matches!(
            integer(&FidanValue::Float(2.5)),
            Ok(FidanValue::Integer(2))
        ));
        assert!(matches!(float(&text("1.5")), Ok(FidanValue::Float(1.5))));
        for error in [
            integer(&text("abc")).unwrap_err(),
            float(&text("abc")).unwrap_err(),
            float(&FidanValue::Boolean(true)).unwrap_err(),
            len(&FidanValue::Integer(42)).unwrap_err(),
        ] {
            assert_eq!(error.code, diag_code!("R0001"));
        }
        assert_eq!(len(&text("λ中")).unwrap(), 2);
        assert!(
            len(&FidanValue::Range {
                start: i64::MIN,
                end: i64::MAX,
                inclusive: true
            })
            .is_err()
        );
    }
}
