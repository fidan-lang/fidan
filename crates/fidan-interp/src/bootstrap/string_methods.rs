//! Bootstrap string methods — placeholder until `std.string` (Phase 7).

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
        "len" => Some(FidanValue::Integer(s.len() as i64)),
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
        "indexOf" => {
            let target = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::String(pat) = target {
                let idx = s
                    .as_str()
                    .find(pat.as_str())
                    .map(|i| FidanValue::Integer(i as i64))
                    .unwrap_or(FidanValue::Integer(-1));
                Some(idx)
            } else {
                Some(FidanValue::Integer(-1))
            }
        }
        "substring" => {
            let mut iter = args.into_iter();
            let start = iter.next().unwrap_or(FidanValue::Nothing);
            let end = iter.next().unwrap_or(FidanValue::Nothing);
            let chars: Vec<char> = s.as_str().chars().collect();
            let len = chars.len();
            let si = match start {
                FidanValue::Integer(n) => n.max(0) as usize,
                _ => 0,
            };
            let ei = match end {
                FidanValue::Integer(n) => (n as usize).min(len),
                _ => len,
            };
            let sub: String = chars[si.min(len)..ei.min(len)].iter().collect();
            Some(FidanValue::String(FidanString::new(&sub)))
        }
        "charAt" => {
            let idx = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Integer(i) = idx {
                Some(
                    s.as_str()
                        .chars()
                        .nth(i as usize)
                        .map(|c| FidanValue::String(FidanString::new(&c.to_string())))
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                Some(FidanValue::Nothing)
            }
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
