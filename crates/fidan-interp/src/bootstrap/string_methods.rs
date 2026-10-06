//! String receiver methods for interpreted execution.

use fidan_config::{ReceiverBuiltinKind, infer_receiver_member};
use fidan_runtime::{FidanList, FidanString, FidanValue, OwnedRef};

pub fn dispatch(s: FidanString, method: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    let method = infer_receiver_member(ReceiverBuiltinKind::String, method)?.canonical_name;
    match method {
        "upper" => Some(FidanValue::String(FidanString::new(
            &s.as_str().to_uppercase(),
        ))),
        "lower" => Some(FidanValue::String(FidanString::new(
            &s.as_str().to_lowercase(),
        ))),
        "trim" => Some(FidanValue::String(FidanString::new(s.as_str().trim()))),
        "trimStart" => Some(FidanValue::String(FidanString::new(
            s.as_str().trim_start(),
        ))),
        "trimEnd" => Some(FidanValue::String(FidanString::new(s.as_str().trim_end()))),
        "len" => Some(FidanValue::Integer(s.char_len() as i64)),
        "contains" => {
            let pat = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::String(p) = pat {
                Some(FidanValue::Boolean(s.as_str().contains(p.as_str())))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "startsWith" => {
            let pat = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::String(p) = pat {
                Some(FidanValue::Boolean(s.as_str().starts_with(p.as_str())))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "endsWith" => {
            let pat = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::String(p) = pat {
                Some(FidanValue::Boolean(s.as_str().ends_with(p.as_str())))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "replace" => {
            let mut iter = args.into_iter();
            let from = iter.next().unwrap_or(FidanValue::Nothing);
            let to = iter.next().unwrap_or(FidanValue::Nothing);
            if let (FidanValue::String(f), FidanValue::String(t)) = (from, to) {
                Some(FidanValue::String(FidanString::new(
                    &s.as_str().replace(f.as_str(), t.as_str()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "split" => {
            let delim = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let sep = match delim {
                FidanValue::String(d) => d.as_str().to_string(),
                _ => " ".to_string(),
            };
            let parts: Vec<FidanValue> = s
                .as_str()
                .split(sep.as_str())
                .map(|p| FidanValue::String(FidanString::new(p)))
                .collect();
            let mut list = FidanList::new();
            for p in parts {
                list.append(p);
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "indexOf" | "lastIndexOf" | "substring" | "charAt" => {
            let mut values = Vec::with_capacity(args.len() + 1);
            values.push(FidanValue::String(s));
            values.extend(args);
            fidan_runtime::stdlib::string::dispatch(method, values)
        }
        // Returns a new string with characters in reversed order.
        // Strings are immutable so this always produces a fresh value.
        "reverse" => {
            let rev: String = s.as_str().chars().rev().collect();
            Some(FidanValue::String(FidanString::new(&rev)))
        }
        _ => None,
    }
}
