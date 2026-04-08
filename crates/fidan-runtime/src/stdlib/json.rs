use crate::{FidanDict, FidanString, FidanValue, OwnedRef, display};

use super::common::{coerce_string, list_value, string_value};

fn json_to_fidan(value: serde_json::Value) -> FidanValue {
    match value {
        serde_json::Value::Null => FidanValue::Nothing,
        serde_json::Value::Bool(value) => FidanValue::Boolean(value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(FidanValue::Integer)
            .or_else(|| value.as_f64().map(FidanValue::Float))
            .unwrap_or(FidanValue::Nothing),
        serde_json::Value::String(value) => string_value(&value),
        serde_json::Value::Array(values) => list_value(values.into_iter().map(json_to_fidan)),
        serde_json::Value::Object(entries) => {
            let mut dict = FidanDict::new();
            for (key, value) in entries {
                dict.insert(FidanString::new(&key), json_to_fidan(value));
            }
            FidanValue::Dict(OwnedRef::new(dict))
        }
    }
}

fn fidan_to_json(value: &FidanValue) -> serde_json::Value {
    match value {
        FidanValue::Integer(value) => serde_json::Value::Number((*value).into()),
        FidanValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::Value::String(display(&FidanValue::Float(*value)))),
        FidanValue::Boolean(value) => serde_json::Value::Bool(*value),
        FidanValue::Nothing => serde_json::Value::Null,
        FidanValue::String(value) => serde_json::Value::String(value.as_str().to_string()),
        FidanValue::List(values) => {
            serde_json::Value::Array(values.borrow().iter().map(fidan_to_json).collect())
        }
        FidanValue::Dict(entries) => {
            let mut map = serde_json::Map::new();
            for (key, value) in entries.borrow().iter() {
                map.insert(key.as_str().to_string(), fidan_to_json(value));
            }
            serde_json::Value::Object(map)
        }
        FidanValue::Shared(shared) => {
            let inner = shared.0.lock().expect("shared json lock poisoned");
            fidan_to_json(&inner)
        }
        FidanValue::WeakShared(weak) => weak
            .upgrade()
            .map(|shared| {
                let inner = shared.0.lock().expect("shared json lock poisoned");
                fidan_to_json(&inner)
            })
            .unwrap_or(serde_json::Value::Null),
        FidanValue::Tuple(values) => {
            serde_json::Value::Array(values.iter().map(fidan_to_json).collect())
        }
        other => serde_json::Value::String(display(other)),
    }
}

fn parse_json_text(text: &str) -> FidanValue {
    serde_json::from_str::<serde_json::Value>(text)
        .map(json_to_fidan)
        .unwrap_or(FidanValue::Nothing)
}

fn render_json_text(value: &FidanValue, pretty: bool) -> String {
    let json = fidan_to_json(value);
    if pretty {
        serde_json::to_string_pretty(&json).unwrap_or_else(|_| "null".to_string())
    } else {
        json.to_string()
    }
}

fn load_json_file(path: &str) -> FidanValue {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| parse_json_text(&text))
        .unwrap_or(FidanValue::Nothing)
}

fn dump_json_file(path: &str, value: &FidanValue, pretty: bool) -> bool {
    std::fs::write(path, render_json_text(value, pretty)).is_ok()
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "parse" | "loads" => {
            let text = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(parse_json_text(&text))
        }
        "load" | "readFile" | "read_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(load_json_file(&path))
        }
        "stringify" | "dumps" => {
            let value = args.first().unwrap_or(&FidanValue::Nothing);
            Some(string_value(&render_json_text(value, false)))
        }
        "dump" | "writeFile" | "write_file" => {
            let value = args.first().unwrap_or(&FidanValue::Nothing);
            let path = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(dump_json_file(&path, value, false)))
        }
        "pretty" | "prettyPrint" | "pretty_print" => {
            let value = args.first().unwrap_or(&FidanValue::Nothing);
            Some(string_value(&render_json_text(value, true)))
        }
        "isValid" | "is_valid" => {
            let text = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(
                serde_json::from_str::<serde_json::Value>(&text).is_ok(),
            ))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "parse",
        "loads",
        "load",
        "readFile",
        "read_file",
        "stringify",
        "dumps",
        "dump",
        "writeFile",
        "write_file",
        "pretty",
        "prettyPrint",
        "pretty_print",
        "isValid",
        "is_valid",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_stringify_cover_nested_json_values() {
        let parsed = dispatch(
            "loads",
            vec![string_value(r#"{"ok":true,"items":[1,2,"x"]}"#)],
        )
        .expect("parse result");

        let FidanValue::Dict(dict) = parsed else {
            panic!("expected json object to parse into dict");
        };
        assert!(matches!(
            dict.borrow().get(&FidanString::new("ok")),
            Some(FidanValue::Boolean(true))
        ));

        let rendered =
            dispatch("dumps", vec![FidanValue::Dict(dict.clone())]).expect("stringify result");
        let FidanValue::String(rendered) = rendered else {
            panic!("expected stringify to return string");
        };
        assert!(rendered.as_str().contains("\"items\""));
    }

    #[test]
    fn invalid_json_returns_nothing_and_false() {
        assert!(matches!(
            dispatch("loads", vec![string_value("{not json")]),
            Some(FidanValue::Nothing)
        ));
        assert!(matches!(
            dispatch("isValid", vec![string_value("{not json")]),
            Some(FidanValue::Boolean(false))
        ));
    }

    #[test]
    fn load_and_dump_round_trip_json_files() {
        let path = std::env::temp_dir().join("fidan-runtime-json-roundtrip.json");
        let path_str = path.to_string_lossy().to_string();

        let value =
            dispatch("loads", vec![string_value(r#"{"name":"fidan"}"#)]).expect("loads result");
        assert!(matches!(
            dispatch("dump", vec![value.clone(), string_value(&path_str)],),
            Some(FidanValue::Boolean(true))
        ));

        let loaded = dispatch("load", vec![string_value(&path_str)]).expect("load result");
        let FidanValue::Dict(dict) = loaded else {
            panic!("expected load to return a dict");
        };
        assert!(matches!(
            dict.borrow().get(&FidanString::new("name")),
            Some(FidanValue::String(name)) if name.as_str() == "fidan"
        ));

        let _ = std::fs::remove_file(path);
    }
}
