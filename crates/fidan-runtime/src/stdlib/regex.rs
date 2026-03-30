use std::sync::{Arc, LazyLock};

use dashmap::DashMap;
use regex::Regex;

use crate::{FidanList, FidanValue, OwnedRef};

use super::common::{coerce_string, string_value};

static REGEX_CACHE: LazyLock<DashMap<String, Arc<Regex>>> = LazyLock::new(DashMap::new);

fn compile(pattern: &str) -> Option<Arc<Regex>> {
    if let Some(cached) = REGEX_CACHE.get(pattern) {
        return Some(Arc::clone(&*cached));
    }
    match Regex::new(pattern) {
        Ok(re) => {
            let arc = Arc::new(re);
            REGEX_CACHE.insert(pattern.to_string(), Arc::clone(&arc));
            Some(arc)
        }
        Err(_) => None,
    }
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "test" | "isMatch" | "is_match" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(
                compile(&pattern)
                    .map(|re| re.is_match(&subject))
                    .unwrap_or(false),
            ))
        }
        "match" | "find" | "find_first" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern).and_then(|re| re.find(&subject).map(|m| m.as_str().to_string()))
            {
                Some(found) => Some(string_value(&found)),
                None => Some(FidanValue::Nothing),
            }
        }
        "findAll" | "find_all" | "matches" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Some(re) = compile(&pattern) {
                for found in re.find_iter(&subject) {
                    list.append(string_value(found.as_str()));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "capture" | "exec" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => match re.captures(&subject) {
                    Some(caps) => {
                        let mut list = FidanList::new();
                        for group in caps.iter() {
                            match group {
                                Some(m) => list.append(string_value(m.as_str())),
                                None => list.append(FidanValue::Nothing),
                            }
                        }
                        Some(FidanValue::List(OwnedRef::new(list)))
                    }
                    None => Some(FidanValue::Nothing),
                },
                None => Some(FidanValue::Nothing),
            }
        }
        "captureAll" | "capture_all" | "execAll" | "exec_all" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut outer = FidanList::new();
            if let Some(re) = compile(&pattern) {
                for caps in re.captures_iter(&subject) {
                    let mut inner = FidanList::new();
                    for group in caps.iter() {
                        match group {
                            Some(m) => inner.append(string_value(m.as_str())),
                            None => inner.append(FidanValue::Nothing),
                        }
                    }
                    outer.append(FidanValue::List(OwnedRef::new(inner)));
                }
            }
            Some(FidanValue::List(OwnedRef::new(outer)))
        }
        "replace" | "replaceFirst" | "replace_first" | "sub" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = coerce_string(args.get(2).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => Some(string_value(&re.replacen(
                    &subject,
                    1,
                    replacement.as_str(),
                ))),
                None => Some(string_value(&subject)),
            }
        }
        "replaceAll" | "replace_all" | "gsub" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = coerce_string(args.get(2).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => Some(string_value(
                    &re.replace_all(&subject, replacement.as_str()),
                )),
                None => Some(string_value(&subject)),
            }
        }
        "split" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            match compile(&pattern) {
                Some(re) => {
                    for part in re.split(&subject) {
                        list.append(string_value(part));
                    }
                }
                None => list.append(string_value(&subject)),
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "isValid" | "is_valid" => {
            let pattern = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(compile(&pattern).is_some()))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "test",
        "isMatch",
        "is_match",
        "match",
        "find",
        "find_first",
        "findAll",
        "find_all",
        "matches",
        "capture",
        "exec",
        "captureAll",
        "capture_all",
        "execAll",
        "exec_all",
        "replace",
        "replaceFirst",
        "replace_first",
        "sub",
        "replaceAll",
        "replace_all",
        "gsub",
        "split",
        "isValid",
        "is_valid",
    ]
}
