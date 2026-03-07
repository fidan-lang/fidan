//! `std.string` — String manipulation functions for Fidan.
//!
//! Available via:
//!   `use std.string`             → `string.split(s, delim)`, etc.
//!   `use std.string.{split}`     → `split(s, delim)` directly in scope.
//!
//! All functions also work as method syntax: `s.split(delim)` because
//! the bootstrap string_methods.rs covers them per the dispatch chain.

use fidan_runtime::{FidanList, FidanString, FidanValue, OwnedRef};

fn as_str(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => f.to_string(),
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => String::new(),
        _ => String::new(),
    }
}

fn str_val(s: &str) -> FidanValue {
    FidanValue::String(FidanString::new(s))
}

#[allow(dead_code)]
fn list_of_strings(v: Vec<&str>) -> FidanValue {
    let mut list = FidanList::new();
    for s in v {
        list.append(str_val(s));
    }
    FidanValue::List(OwnedRef::new(list))
}

/// Dispatch a `string.<name>(args)` free-function call.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── Case ─────────────────────────────────────────────────────────
        "toUpper" | "upper" | "to_upper" => Some(str_val(
            &as_str(args.first().unwrap_or(&FidanValue::Nothing)).to_uppercase(),
        )),
        "toLower" | "lower" | "to_lower" => Some(str_val(
            &as_str(args.first().unwrap_or(&FidanValue::Nothing)).to_lowercase(),
        )),
        "capitalize" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let mut c = s.chars();
            let capped = match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            };
            Some(str_val(&capped))
        }

        // ── Trim ─────────────────────────────────────────────────────────
        "trim" => Some(str_val(
            as_str(args.first().unwrap_or(&FidanValue::Nothing)).trim(),
        )),
        "trimStart" | "ltrim" | "trim_start" => Some(str_val(
            as_str(args.first().unwrap_or(&FidanValue::Nothing)).trim_start(),
        )),
        "trimEnd" | "rtrim" | "trim_end" => Some(str_val(
            as_str(args.first().unwrap_or(&FidanValue::Nothing)).trim_end(),
        )),

        // ── Split / join ─────────────────────────────────────────────────
        "split" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let sep = args
                .get(1)
                .map(|v| as_str(v))
                .unwrap_or_else(|| " ".to_string());
            let parts: Vec<&str> = s.split(sep.as_str()).collect();
            // Can't easily use list_of_strings because str lifetimes differ, so reborrow:
            let mut list = FidanList::new();
            for p in parts {
                list.append(str_val(p));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "join" => {
            let sep = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let list = match args.get(1) {
                Some(FidanValue::List(l)) => {
                    l.borrow().iter().map(|v| as_str(v)).collect::<Vec<_>>()
                }
                _ => return Some(str_val("")),
            };
            Some(str_val(&list.join(&sep)))
        }
        "lines" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let parts: Vec<&str> = s.lines().collect();
            let mut list = FidanList::new();
            for p in parts {
                list.append(str_val(p));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }

        // ── Search ───────────────────────────────────────────────────────
        "contains" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.contains(pat.as_str())))
        }
        "startsWith" | "starts_with" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.starts_with(pat.as_str())))
        }
        "endsWith" | "ends_with" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.ends_with(pat.as_str())))
        }
        "indexOf" | "index_of" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let idx = s.find(pat.as_str()).map(|i| i as i64).unwrap_or(-1);
            Some(FidanValue::Integer(idx))
        }
        "lastIndexOf" | "last_index_of" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let pat = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let idx = s.rfind(pat.as_str()).map(|i| i as i64).unwrap_or(-1);
            Some(FidanValue::Integer(idx))
        }

        // ── Replace ───────────────────────────────────────────────────────
        "replace" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let from = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let to = as_str(args.get(2).unwrap_or(&FidanValue::Nothing));
            Some(str_val(&s.replace(from.as_str(), to.as_str())))
        }
        "replaceFirst" | "replace_first" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let from = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let to = as_str(args.get(2).unwrap_or(&FidanValue::Nothing));
            Some(str_val(&s.replacen(from.as_str(), to.as_str(), 1)))
        }

        // ── Substring ─────────────────────────────────────────────────────
        "slice" | "substr" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
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
            Some(str_val(&sub))
        }

        // ── Padding ───────────────────────────────────────────────────────
        "padStart" | "pad_start" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let width = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            let pad = args
                .get(2)
                .map(|v| as_str(v))
                .unwrap_or_else(|| " ".to_string());
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.len() >= width {
                Some(str_val(&s))
            } else {
                let padding: String = std::iter::repeat(pad_char).take(width - s.len()).collect();
                Some(str_val(&format!("{padding}{s}")))
            }
        }
        "padEnd" | "pad_end" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let width = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            let pad = args
                .get(2)
                .map(|v| as_str(v))
                .unwrap_or_else(|| " ".to_string());
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.len() >= width {
                Some(str_val(&s))
            } else {
                let padding: String = std::iter::repeat(pad_char).take(width - s.len()).collect();
                Some(str_val(&format!("{s}{padding}")))
            }
        }

        // ── Misc ──────────────────────────────────────────────────────────
        "repeat" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let n = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n as usize,
                _ => 0,
            };
            Some(str_val(&s.repeat(n)))
        }
        "reverse" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(str_val(&s.chars().rev().collect::<String>()))
        }
        "len" | "length" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Integer(s.chars().count() as i64))
        }
        "isEmpty" | "is_empty" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(s.is_empty()))
        }
        "format" => {
            // string.format(template, ...args) -- replaces {} placeholders in order
            let template = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let mut result = template.clone();
            for arg in args.iter().skip(1) {
                if let Some(pos) = result.find("{}") {
                    result.replace_range(pos..pos + 2, &as_str(arg));
                }
            }
            Some(str_val(&result))
        }
        "parseInt" | "parse_int" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(
                s.trim()
                    .parse::<i64>()
                    .map(FidanValue::Integer)
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "parseFloat" | "parse_float" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(
                s.trim()
                    .parse::<f64>()
                    .map(FidanValue::Float)
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "chars" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            for ch in s.chars() {
                list.append(str_val(&ch.to_string()));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "bytes" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            for b in s.bytes() {
                list.append(FidanValue::Integer(b as i64));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
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
                Some(str_val(&s))
            } else {
                Some(str_val(""))
            }
        }
        "charCode" | "char_code" => {
            let s = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let code = s.chars().next().map(|c| c as i64).unwrap_or(0);
            Some(FidanValue::Integer(code))
        }
        "fromCharCode" | "from_char_code" => {
            let code = match args.first() {
                Some(FidanValue::Integer(n)) => *n as u32,
                _ => 0,
            };
            let ch = char::from_u32(code).unwrap_or('\0');
            Some(str_val(&ch.to_string()))
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
