//! Validation of compiler-supplied MIR type descriptors at boxed call boundaries.
use crate::{FidanValue, stdlib::StdlibRuntimeError};
use fidan_diagnostics::diag_code;
use serde_json::Value;

fn accepts(value: &FidanValue, contract: &Value) -> bool {
    if value.is_nothing() {
        return true;
    } // Fidan's universal nullable value.
    if let Some(name) = contract.as_str() {
        return match name {
            "Dynamic" | "Error" => true,
            "Integer" | "Float" => matches!(value, FidanValue::Integer(_) | FidanValue::Float(_)),
            "Boolean" => matches!(value, FidanValue::Boolean(_)),
            "String" => matches!(value, FidanValue::String(_)),
            "Handle" => matches!(value, FidanValue::Handle(_)),
            "Function" => matches!(
                value,
                FidanValue::Function(_) | FidanValue::Closure { .. } | FidanValue::StdlibFn(..)
            ),
            "Nothing" => false,
            _ => false,
        };
    }
    let Some((kind, inner)) = contract.as_object().and_then(|map| map.iter().next()) else {
        return false;
    };
    match (kind.as_str(), value) {
        ("List", FidanValue::List(list)) => list.borrow().iter().all(|item| accepts(item, inner)),
        ("Dict", FidanValue::Dict(dict)) => inner.as_array().is_some_and(|types| {
            types.len() == 2
                && dict
                    .borrow()
                    .iter()
                    .all(|(key, value)| accepts(key, &types[0]) && accepts(value, &types[1]))
        }),
        ("HashSet", FidanValue::HashSet(set)) => {
            set.borrow().iter().all(|item| accepts(item, inner))
        }
        ("Tuple", FidanValue::Tuple(items)) => inner.as_array().is_some_and(|types| {
            types.is_empty()
                || types.len() == items.len()
                    && items.iter().zip(types).all(|(item, ty)| accepts(item, ty))
        }),
        ("Shared", FidanValue::Shared(_))
        | ("WeakShared", FidanValue::WeakShared(_))
        | ("Pending", FidanValue::Pending(_) | FidanValue::PendingTask(_)) => true,
        ("Object", FidanValue::Object(_)) | ("Enum", FidanValue::EnumVariant { .. }) => true,
        ("Object", FidanValue::Dict(dict)) => matches!(
            dict.borrow()
                .get(&FidanValue::String(crate::FidanString::new("__class__"))),
            Ok(Some(FidanValue::String(_)))
        ),
        _ => false,
    }
}

/// Check assignability, then apply the existing scalar numeric coercions.
/// Collection members are validated recursively; this does not change equality.
pub fn prepare(value: FidanValue, descriptor: &str) -> Result<FidanValue, StdlibRuntimeError> {
    let contract: Value = serde_json::from_str(descriptor).map_err(|error| {
        StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!("invalid compiler type contract: {error}"),
        )
    })?;
    if !accepts(&value, &contract) {
        return Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!(
                "type error: expected {descriptor}, got `{}`",
                value.type_name()
            ),
        ));
    }
    Ok(match (contract.as_str(), value) {
        (Some("Integer"), FidanValue::Float(value)) => FidanValue::Integer(value as i64),
        (Some("Float"), FidanValue::Integer(value)) => FidanValue::Float(value as f64),
        (_, value) => value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FidanList, OwnedRef};

    #[test]
    fn recursive_contracts_reject_incompatible_values_and_preserve_nullable_coercions() {
        let valid = FidanValue::List(OwnedRef::new(FidanList::from_vec(vec![
            FidanValue::Integer(1),
            FidanValue::Nothing,
        ])));
        assert!(prepare(valid, r#"{"List":"Integer"}"#).is_ok());
        let invalid = FidanValue::List(OwnedRef::new(FidanList::from_vec(vec![
            FidanValue::Boolean(true),
        ])));
        assert!(prepare(invalid, r#"{"List":"Integer"}"#).is_err());
        assert!(matches!(
            prepare(FidanValue::Integer(2), r#""Float""#),
            Ok(FidanValue::Float(2.0))
        ));
        assert!(matches!(
            prepare(FidanValue::Nothing, r#""Integer""#),
            Ok(FidanValue::Nothing)
        ));
        assert!(prepare(FidanValue::Boolean(true), r#""Integer""#).is_err());
    }
}
