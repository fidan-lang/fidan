use crate::{FidanList, FidanValue, OwnedRef};

use super::common::{coerce_string, string_value};

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "fromChars" | "from_chars" => {
            let list_val = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::List(l) = list_val {
                let s: String = l
                    .borrow()
                    .iter()
                    .filter_map(|v| {
                        if let FidanValue::String(cs) = v {
                            cs.as_str().chars().next()
                        } else {
                            None
                        }
                    })
                    .collect();
                Some(string_value(&s))
            } else {
                Some(string_value(""))
            }
        }
        "fromCharCode" | "from_char_code" => {
            let code = match args.first() {
                Some(FidanValue::Integer(n)) => *n as u32,
                _ => 0,
            };
            let ch = char::from_u32(code).unwrap_or('\0');
            Some(string_value(&ch.to_string()))
        }
        "toUpper" | "upper" | "to_upper" => Some(string_value(
            &coerce_string(args.first().unwrap_or(&FidanValue::Nothing)).to_uppercase(),
        )),
        "toLower" | "lower" | "to_lower" => Some(string_value(
            &coerce_string(args.first().unwrap_or(&FidanValue::Nothing)).to_lowercase(),
        )),
        "capitalize" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut c = s.chars();
            let capped = match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            };
            Some(string_value(&capped))
        }
        "trim" => Some(string_value(
            coerce_string(args.first().unwrap_or(&FidanValue::Nothing)).trim(),
        )),
        "trimStart" | "ltrim" | "trim_start" => Some(string_value(
            coerce_string(args.first().unwrap_or(&FidanValue::Nothing)).trim_start(),
        )),
        "trimEnd" | "rtrim" | "trim_end" => Some(string_value(
            coerce_string(args.first().unwrap_or(&FidanValue::Nothing)).trim_end(),
        )),
        "split" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let sep = args
                .get(1)
                .map(coerce_string)
                .unwrap_or_else(|| " ".to_string());
            let mut list = FidanList::new();
            for part in s.split(sep.as_str()) {
                list.append(string_value(part));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "join" => {
            let sep = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let list = match args.get(1) {
                Some(FidanValue::List(l)) => {
                    l.borrow().iter().map(coerce_string).collect::<Vec<_>>()
                }
                _ => return Some(string_value("")),
            };
            Some(string_value(&list.join(&sep)))
        }
        "lines" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            for part in s.lines() {
                list.append(string_value(part));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "contains" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.contains(pat.as_str())))
        }
        "startsWith" | "starts_with" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.starts_with(pat.as_str())))
        }
        "endsWith" | "ends_with" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.ends_with(pat.as_str())))
        }
        "indexOf" | "index_of" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Integer(
                s.find(pat.as_str()).map(|i| i as i64).unwrap_or(-1),
            ))
        }
        "lastIndexOf" | "last_index_of" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Integer(
                s.rfind(pat.as_str()).map(|i| i as i64).unwrap_or(-1),
            ))
        }
        "replace" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let from = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(2).unwrap_or(&FidanValue::Nothing));
            Some(string_value(&s.replace(from.as_str(), to.as_str())))
        }
        "replaceFirst" | "replace_first" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let from = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(2).unwrap_or(&FidanValue::Nothing));
            Some(string_value(&s.replacen(from.as_str(), to.as_str(), 1)))
        }
        "slice" | "substr" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len();
            let start = match args.get(1) {
                Some(FidanValue::Integer(n)) => (*n).max(0) as usize,
                _ => 0,
            };
            let end = match args.get(2) {
                Some(FidanValue::Integer(n)) => (*n as usize).min(len),
                _ => len,
            };
            let sub: String = chars[start.min(len)..end.min(len)].iter().collect();
            Some(string_value(&sub))
        }
        "padStart" | "pad_start" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let width = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            let pad = args
                .get(2)
                .map(coerce_string)
                .unwrap_or_else(|| " ".to_string());
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.len() >= width {
                Some(string_value(&s))
            } else {
                let padding: String = std::iter::repeat_n(pad_char, width - s.len()).collect();
                Some(string_value(&format!("{padding}{s}")))
            }
        }
        "padEnd" | "pad_end" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let width = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            let pad = args
                .get(2)
                .map(coerce_string)
                .unwrap_or_else(|| " ".to_string());
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.len() >= width {
                Some(string_value(&s))
            } else {
                let padding: String = std::iter::repeat_n(pad_char, width - s.len()).collect();
                Some(string_value(&format!("{s}{padding}")))
            }
        }
        "repeat" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let n = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            Some(string_value(&s.repeat(n)))
        }
        "reverse" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(string_value(&s.chars().rev().collect::<String>()))
        }
        "len" | "length" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Integer(s.chars().count() as i64))
        }
        "isEmpty" | "is_empty" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.is_empty()))
        }
        "format" => {
            let template = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut result = template.clone();
            for arg in args.iter().skip(1) {
                if let Some(pos) = result.find("{}") {
                    result.replace_range(pos..pos + 2, &coerce_string(arg));
                }
            }
            Some(string_value(&result))
        }
        "parseInt" | "parse_int" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(
                s.trim()
                    .parse::<i64>()
                    .map(FidanValue::Integer)
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "parseFloat" | "parse_float" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(
                s.trim()
                    .parse::<f64>()
                    .map(FidanValue::Float)
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "chars" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            for ch in s.chars() {
                list.append(string_value(&ch.to_string()));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "bytes" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            for byte in s.bytes() {
                list.append(FidanValue::Integer(byte as i64));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "charCode" | "char_code" => {
            let s = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Integer(
                s.chars().next().map(|c| c as i64).unwrap_or(0),
            ))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "toUpper",
        "upper",
        "to_upper",
        "toLower",
        "lower",
        "to_lower",
        "capitalize",
        "trim",
        "trimStart",
        "ltrim",
        "trim_start",
        "trimEnd",
        "rtrim",
        "trim_end",
        "split",
        "join",
        "lines",
        "contains",
        "startsWith",
        "starts_with",
        "endsWith",
        "ends_with",
        "indexOf",
        "index_of",
        "lastIndexOf",
        "last_index_of",
        "replace",
        "replaceFirst",
        "replace_first",
        "slice",
        "substr",
        "padStart",
        "pad_start",
        "padEnd",
        "pad_end",
        "repeat",
        "reverse",
        "len",
        "length",
        "isEmpty",
        "is_empty",
        "format",
        "parseInt",
        "parse_int",
        "parseFloat",
        "parse_float",
        "chars",
        "bytes",
        "fromChars",
        "from_chars",
        "charCode",
        "char_code",
        "fromCharCode",
        "from_char_code",
    ]
}
