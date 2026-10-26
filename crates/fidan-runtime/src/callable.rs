//! Read-only callable metadata and invocation argument validation. No dispatch registry.
use crate::{FidanList, FidanString, FidanValue, FunctionId, OwnedRef, stdlib::StdlibRuntimeError};
use fidan_diagnostics::diag_code;
use serde_json::Value;
use std::sync::Arc;

fn invalid_metadata() -> StdlibRuntimeError {
    StdlibRuntimeError::new(diag_code!("R0001"), "invalid compiler callable metadata")
}

fn default_value(value: &Value) -> Result<FidanValue, StdlibRuntimeError> {
    if value.is_null() || value.as_str() == Some("Nothing") {
        return Ok(FidanValue::Nothing);
    }
    let (tag, data) = value
        .as_object()
        .filter(|map| map.len() == 1)
        .and_then(|map| map.iter().next())
        .ok_or_else(invalid_metadata)?;
    Ok(match tag.as_str() {
        "Int" => FidanValue::Integer(data.as_i64().ok_or_else(invalid_metadata)?),
        "FloatBits" => {
            FidanValue::Float(f64::from_bits(data.as_u64().ok_or_else(invalid_metadata)?))
        }
        "Bool" => FidanValue::Boolean(data.as_bool().ok_or_else(invalid_metadata)?),
        "Str" => FidanValue::String(FidanString::new(
            data.as_str().ok_or_else(invalid_metadata)?,
        )),
        "FunctionRef" => FidanValue::Function(FunctionId(
            u32::try_from(data.as_u64().ok_or_else(invalid_metadata)?)
                .map_err(|_| invalid_metadata())?,
        )),
        "Namespace" => {
            FidanValue::Namespace(Arc::from(data.as_str().ok_or_else(invalid_metadata)?))
        }
        "EnumType" => FidanValue::EnumType(Arc::from(data.as_str().ok_or_else(invalid_metadata)?)),
        "ClassType" => {
            FidanValue::ClassType(Arc::from(data.as_str().ok_or_else(invalid_metadata)?))
        }
        "StdlibFn" => FidanValue::StdlibFn(
            Arc::from(data["module"].as_str().ok_or_else(invalid_metadata)?),
            Arc::from(data["name"].as_str().ok_or_else(invalid_metadata)?),
        ),
        _ => return Err(invalid_metadata()),
    })
}

/// Creates fresh values, so caller mutations cannot change a function's contract.
/// Closure capture parameters are an implementation detail and are excluded.
pub fn field(
    metadata: &Value,
    captures: usize,
    name: &str,
) -> Result<Option<FidanValue>, StdlibRuntimeError> {
    match name {
        "name" => Ok(Some(FidanValue::String(FidanString::new(
            metadata["name"].as_str().ok_or_else(invalid_metadata)?,
        )))),
        "parameters" => {
            let parameters = metadata["parameters"]
                .as_array()
                .ok_or_else(invalid_metadata)?;
            let parameters = parameters.get(captures..).ok_or_else(invalid_metadata)?;
            let values = parameters
                .iter()
                .map(|parameter| {
                    let mut object = parameter.as_object().ok_or_else(invalid_metadata)?.clone();
                    let default = default_value(&object.remove("default").unwrap_or(Value::Null))?;
                    let FidanValue::Dict(result) =
                        crate::stdlib::json::json_to_fidan(Value::Object(object))
                    else {
                        return Err(invalid_metadata());
                    };
                    result
                        .borrow_mut()
                        .insert(FidanValue::String(FidanString::new("default")), default)
                        .map_err(|_| invalid_metadata())?;
                    Ok(FidanValue::Dict(result))
                })
                .collect::<Result<Vec<_>, StdlibRuntimeError>>()?;
            Ok(Some(FidanValue::List(OwnedRef::new(FidanList::from_vec(
                values,
            )))))
        }
        _ => Ok(None),
    }
}

/// Stdlib arity/names come from the single central declarative source. Its erased
/// signatures do not encode types/default values: expose unknowns honestly.
pub fn stdlib_metadata(module: &str, name: &str) -> Result<Value, StdlibRuntimeError> {
    let params =
        fidan_config::stdlib::member_call_params(module, name).ok_or_else(invalid_metadata)?;
    let canonical = fidan_config::stdlib::member_info(module, name)
        .and_then(|info| info.names.first().copied())
        .unwrap_or(name);
    let parameters = params
        .into_iter()
        .map(|param| {
            serde_json::json!({
                "name": param.name, "type": "dynamic", "certain": false,
                "required": !param.optional && !param.variadic, "default_known": false,
                "default": null, "variadic": param.variadic,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({"name": canonical, "parameters": parameters}))
}

/// Reuses normal dynamic dispatch after validating this method's own argument list.
pub fn invocation_arguments(
    mut args: Vec<FidanValue>,
) -> Result<Vec<FidanValue>, StdlibRuntimeError> {
    if args.len() != 1 {
        return Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            "action.invoke expects exactly one argument list",
        ));
    }
    match args.pop().unwrap() {
        FidanValue::List(values) => Ok(values.borrow().iter().cloned().collect()),
        value => Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!("action.invoke expects a list, got {}", value.type_name()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_preserves_float_bits_captures_and_independent_copies() {
        let metadata = serde_json::json!({"name":"example", "parameters":[
            {"name":"capture", "default":null},
            {"name":"value", "type":"float", "default":{"FloatBits":(-0.0_f64).to_bits()}, "default_known":true},
        ]});
        let Some(FidanValue::List(first)) = field(&metadata, 1, "parameters").unwrap() else {
            panic!("missing parameter list")
        };
        let FidanValue::Dict(parameter) = first.borrow().get(0).unwrap().clone() else {
            panic!("missing parameter")
        };
        let key = FidanValue::String(FidanString::new("default"));
        assert!(
            matches!(parameter.borrow().get(&key).unwrap(), Some(FidanValue::Float(value)) if value.to_bits() == (-0.0_f64).to_bits())
        );
        parameter
            .borrow_mut()
            .insert(key.clone(), FidanValue::Nothing)
            .unwrap();
        let Some(FidanValue::List(second)) = field(&metadata, 1, "parameters").unwrap() else {
            panic!("missing second list")
        };
        let FidanValue::Dict(second) = second.borrow().get(0).unwrap().clone() else {
            panic!("missing second parameter")
        };
        assert!(matches!(
            second.borrow().get(&key).unwrap(),
            Some(FidanValue::Float(_))
        ));
        assert!(field(&metadata, 3, "parameters").is_err());
        assert!(invocation_arguments(vec![FidanValue::Integer(1)]).is_err());
        let nonfinite = serde_json::json!({"FloatBits":f64::INFINITY.to_bits()});
        assert!(
            matches!(default_value(&nonfinite).unwrap(), FidanValue::Float(value) if value.is_infinite())
        );
    }
}
