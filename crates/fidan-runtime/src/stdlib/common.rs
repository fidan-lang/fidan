use crate::{FidanList, FidanString, FidanValue, OwnedRef, display};

pub fn string_value(s: &str) -> FidanValue {
    FidanValue::String(FidanString::new(s))
}

pub fn list_value(values: impl IntoIterator<Item = FidanValue>) -> FidanValue {
    let mut list = FidanList::new();
    for value in values {
        list.append(value);
    }
    FidanValue::List(OwnedRef::new(list))
}

pub fn coerce_string(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => f.to_string(),
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => String::new(),
        _ => String::new(),
    }
}

pub fn display_string(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_owned(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => {
            if f.fract() == 0.0 {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => "nothing".to_owned(),
        other => display(other),
    }
}
