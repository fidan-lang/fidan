use crate::{FidanDict, FidanHashSet, FidanString, FidanValue, OwnedRef, display};
use serde_json::{Map as JsonMap, Value as JsonValue};

use super::common::{coerce_string, list_value, string_value};

const FIDAN_TAG_KEY: &str = "$fidan";
const FIDAN_DICT_TAG: &str = "dict";
const FIDAN_HASHSET_TAG: &str = "hashset";
const FIDAN_TUPLE_TAG: &str = "tuple";
const FIDAN_ENTRIES_KEY: &str = "entries";
const FIDAN_ITEMS_KEY: &str = "items";

fn tagged_json_value(tag: &str, payload_key: &str, payload: JsonValue) -> JsonValue {
    let mut map = JsonMap::new();
    map.insert(
        FIDAN_TAG_KEY.to_string(),
        JsonValue::String(tag.to_string()),
    );
    map.insert(payload_key.to_string(), payload);
    JsonValue::Object(map)
}

fn decode_tagged_dict(entries: &JsonValue) -> Option<FidanValue> {
    let JsonValue::Array(items) = entries else {
        return None;
    };

    let mut dict = FidanDict::new();
    for item in items {
        let JsonValue::Array(pair) = item else {
            return None;
        };
        if pair.len() != 2 {
            return None;
        }

        let key = json_to_fidan(pair[0].clone());
        let value = json_to_fidan(pair[1].clone());
        dict.insert(key, value).ok()?;
    }

    Some(FidanValue::Dict(OwnedRef::new(dict)))
}

fn decode_tagged_hashset(items: &JsonValue) -> Option<FidanValue> {
    let JsonValue::Array(items) = items else {
        return None;
    };
    let set = FidanHashSet::from_values(items.iter().cloned().map(json_to_fidan)).ok()?;
    Some(FidanValue::HashSet(OwnedRef::new(set)))
}

fn decode_tagged_tuple(items: &JsonValue) -> Option<FidanValue> {
    let JsonValue::Array(items) = items else {
        return None;
    };
    Some(FidanValue::Tuple(
        items.iter().cloned().map(json_to_fidan).collect(),
    ))
}

fn decode_tagged_json_object(entries: &JsonMap<String, JsonValue>) -> Option<FidanValue> {
    let tag = entries.get(FIDAN_TAG_KEY)?.as_str()?;
    if entries.len() != 2 {
        return None;
    }

    match tag {
        FIDAN_DICT_TAG => decode_tagged_dict(entries.get(FIDAN_ENTRIES_KEY)?),
        FIDAN_HASHSET_TAG => decode_tagged_hashset(entries.get(FIDAN_ITEMS_KEY)?),
        FIDAN_TUPLE_TAG => decode_tagged_tuple(entries.get(FIDAN_ITEMS_KEY)?),
        _ => None,
    }
}

fn plain_json_object(dict: &FidanDict) -> Option<JsonMap<String, JsonValue>> {
    let mut map = JsonMap::new();
    for (key, value) in dict.entries_sorted_refs() {
        let FidanValue::String(key) = key else {
            return None;
        };
        if key.as_str() == FIDAN_TAG_KEY {
            return None;
        }
        map.insert(key.as_str().to_string(), fidan_to_json(value));
    }
    Some(map)
}

fn fidan_dict_to_json(dict: &FidanDict) -> JsonValue {
    if let Some(map) = plain_json_object(dict) {
        return JsonValue::Object(map);
    }

    let entries = dict
        .entries_sorted_refs()
        .into_iter()
        .map(|(key, value)| JsonValue::Array(vec![fidan_to_json(key), fidan_to_json(value)]))
        .collect();
    tagged_json_value(FIDAN_DICT_TAG, FIDAN_ENTRIES_KEY, JsonValue::Array(entries))
}

fn fidan_hashset_to_json(set: &FidanHashSet) -> JsonValue {
    let items = set
        .values_sorted_refs()
        .into_iter()
        .map(fidan_to_json)
        .collect();
    tagged_json_value(FIDAN_HASHSET_TAG, FIDAN_ITEMS_KEY, JsonValue::Array(items))
}

fn fidan_tuple_to_json(items: &[FidanValue]) -> JsonValue {
    tagged_json_value(
        FIDAN_TUPLE_TAG,
        FIDAN_ITEMS_KEY,
        JsonValue::Array(items.iter().map(fidan_to_json).collect()),
    )
}

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
            decode_tagged_json_object(&entries).unwrap_or_else(|| {
                let mut dict = FidanDict::new();
                for (key, value) in entries {
                    let _ = dict.insert(
                        FidanValue::String(FidanString::new(&key)),
                        json_to_fidan(value),
                    );
                }
                FidanValue::Dict(OwnedRef::new(dict))
            })
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
        FidanValue::Dict(entries) => fidan_dict_to_json(&entries.borrow()),
        FidanValue::HashSet(set) => fidan_hashset_to_json(&set.borrow()),
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
        FidanValue::Tuple(values) => fidan_tuple_to_json(values),
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
            dict.borrow()
                .get(&FidanValue::String(FidanString::new("ok"))),
            Ok(Some(FidanValue::Boolean(true)))
        ));

        let rendered =
            dispatch("dumps", vec![FidanValue::Dict(dict.clone())]).expect("stringify result");
        let FidanValue::String(rendered) = rendered else {
            panic!("expected stringify to return string");
        };
        assert!(rendered.as_str().contains("\"items\""));
        assert!(!rendered.as_str().contains(FIDAN_TAG_KEY));
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
            dict.borrow().get(&FidanValue::String(FidanString::new("name"))),
            Ok(Some(FidanValue::String(name))) if name.as_str() == "fidan"
        ));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tagged_round_trip_preserves_typed_dict_keys_and_hashsets() {
        let mut dict = FidanDict::new();
        let tuple_key = FidanValue::Tuple(vec![FidanValue::Integer(1), FidanValue::Boolean(true)]);
        let _ = dict.insert(
            tuple_key.clone(),
            FidanValue::String(FidanString::new("ok")),
        );

        let mut set = FidanHashSet::new();
        assert!(
            set.insert(tuple_key.clone())
                .expect("insert tuple into set")
        );
        assert!(
            set.insert(FidanValue::Integer(7))
                .expect("insert int into set")
        );

        let payload = FidanValue::Tuple(vec![
            FidanValue::Dict(OwnedRef::new(dict)),
            FidanValue::HashSet(OwnedRef::new(set)),
        ]);

        let rendered = dispatch("dumps", vec![payload]).expect("stringify tagged payload");
        let FidanValue::String(rendered) = rendered else {
            panic!("expected dumps to return string");
        };
        assert!(rendered.as_str().contains("\"$fidan\":\"dict\""));
        assert!(rendered.as_str().contains("\"$fidan\":\"hashset\""));
        assert!(rendered.as_str().contains("\"$fidan\":\"tuple\""));

        let reparsed =
            dispatch("loads", vec![FidanValue::String(rendered)]).expect("reparse payload");
        let FidanValue::Tuple(items) = reparsed else {
            panic!("expected tagged payload to round-trip as tuple");
        };

        let FidanValue::Dict(round_trip_dict) = &items[0] else {
            panic!("expected first tuple item to be dict");
        };
        assert!(matches!(
            round_trip_dict.borrow().get(&tuple_key),
            Ok(Some(FidanValue::String(value))) if value.as_str() == "ok"
        ));

        let FidanValue::HashSet(round_trip_set) = &items[1] else {
            panic!("expected second tuple item to be hashset");
        };
        assert!(
            round_trip_set
                .borrow()
                .contains(&tuple_key)
                .expect("contains tuple key")
        );
        assert!(
            round_trip_set
                .borrow()
                .contains(&FidanValue::Integer(7))
                .expect("contains integer key")
        );
    }
}
