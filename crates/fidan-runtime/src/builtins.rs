//! Shared core builtin contracts for interpreted and native calls.
use crate::{FidanHashSet, FidanString, FidanValue, OwnedRef, display, stdlib::StdlibRuntimeError};
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

/// Nothing is the existing optional constructor default (an empty set).
pub fn hashset(source: FidanValue) -> Result<FidanValue, StdlibRuntimeError> {
    let set = match source {
        FidanValue::Nothing => FidanHashSet::new(),
        FidanValue::List(list) => FidanHashSet::from_values(list.borrow().iter().cloned())
            .map_err(|error| StdlibRuntimeError::new(diag_code!("R0001"), error.to_string()))?,
        FidanValue::HashSet(existing) => existing.borrow().clone(),
        other => {
            return Err(StdlibRuntimeError::new(
                diag_code!("R0001"),
                format!(
                    "hashset(items) expects a list or hashset, got {}",
                    other.type_name()
                ),
            ));
        }
    };
    Ok(FidanValue::HashSet(OwnedRef::new(set)))
}

pub fn weak_shared(value: FidanValue) -> Result<FidanValue, StdlibRuntimeError> {
    Ok(match value {
        FidanValue::Shared(shared) => FidanValue::WeakShared(shared.downgrade()),
        FidanValue::WeakShared(weak) => FidanValue::WeakShared(weak),
        other => {
            return Err(StdlibRuntimeError::new(
                diag_code!("R0001"),
                format!(
                    "WeakShared(shared) expects a Shared value, got {}",
                    other.type_name()
                ),
            ));
        }
    })
}

/// EOF is a successful empty line; read errors never become partial input.
pub fn read_input_line(
    reader: &mut impl std::io::BufRead,
) -> Result<FidanValue, StdlibRuntimeError> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|error| {
        StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!("failed to read input: {error}"),
        )
    })?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(FidanValue::String(FidanString::new(&line)))
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
    #[test]
    fn shared_constructor_contracts_preserve_values_and_errors() {
        assert!(
            matches!(hashset(FidanValue::Nothing), Ok(FidanValue::HashSet(set)) if set.borrow().is_empty())
        );
        let values = FidanValue::List(OwnedRef::new(crate::FidanList::from_vec(vec![
            FidanValue::Integer(1),
            FidanValue::Integer(1),
        ])));
        let set = hashset(values).unwrap();
        assert!(matches!(hashset(set), Ok(FidanValue::HashSet(set)) if set.borrow().len() == 1));
        let shared = crate::SharedRef::new(FidanValue::Integer(7));
        let weak = weak_shared(FidanValue::Shared(shared.clone())).unwrap();
        assert!(matches!(weak_shared(weak), Ok(FidanValue::WeakShared(weak)) if weak.is_alive()));
        for result in [
            hashset(FidanValue::Integer(42)),
            weak_shared(FidanValue::Integer(42)),
        ] {
            assert_eq!(result.unwrap_err().code, diag_code!("R0001"));
        }
    }

    #[test]
    fn input_read_contract_preserves_eof_newlines_and_host_errors() {
        for (bytes, expected) in [
            (b"".as_slice(), ""),
            (b"line\n", "line"),
            (b"line\r\n", "line"),
            (b"line\r", "line\r"),
        ] {
            assert!(
                matches!(read_input_line(&mut std::io::Cursor::new(bytes)), Ok(FidanValue::String(line)) if line.as_str() == expected)
            );
        }
        let error = read_input_line(&mut std::io::Cursor::new(b"\xff\n")).unwrap_err();
        assert_eq!(error.code, diag_code!("R0001"));
        assert!(error.message.contains("failed to read input"));
    }
}
