use crate::{FidanList, FidanValue, OwnedRef, current_program_args};

use super::common::{coerce_string, display_string, string_value};

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "get" | "getVar" | "get_var" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            match std::env::var(&key) {
                Ok(value) => Some(string_value(&value)),
                Err(_) => Some(FidanValue::Nothing),
            }
        }
        "set" | "setVar" | "set_var" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let value = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var(&key, &value)
            };
            Some(FidanValue::Nothing)
        }
        "args" => {
            let mut list = FidanList::new();
            for arg in current_program_args().into_iter().skip(1) {
                list.append(string_value(&arg));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "get", "getVar", "get_var", "set", "setVar", "set_var", "args",
    ]
}
