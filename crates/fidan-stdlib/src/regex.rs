//! `std.regex` — Regular expression functions for Fidan.
//!
//! Available via:
//!   `use std.regex`                     → `regex.test(pattern, str)`, etc.
//!   `use std.regex.{test}`              → `test(pattern, str)` directly in scope.
//!
//! All functions take a `pattern` string as the first argument and a `subject`
//! string as the second (except `replace` which also takes a `replacement`).
//!
//! Invalid patterns return `nothing` / `false` rather than panicking.
//!
//! ## Performance
//! Compiled regex objects are cached in a process-wide `DashMap<String, Arc<Regex>>`.
//! Each unique pattern string is compiled exactly once; subsequent calls with the
//! same pattern return the cached `Arc<Regex>` with no recompilation.

use std::sync::{Arc, LazyLock};

use dashmap::DashMap;
use fidan_runtime::{FidanList, FidanString, FidanValue, OwnedRef};
use regex::Regex;

/// Process-wide cache: pattern string → compiled `Regex`.
/// `DashMap` allows concurrent reads without a global lock.
static REGEX_CACHE: LazyLock<DashMap<String, Arc<Regex>>> = LazyLock::new(DashMap::new);

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

/// Compile a regex pattern, caching the result for reuse.
/// Returns `None` for invalid patterns (silently; callers return `nothing`/`false`).
fn compile(pattern: &str) -> Option<Arc<Regex>> {
    // Fast path: pattern already in cache (shared read, no lock).
    if let Some(cached) = REGEX_CACHE.get(pattern) {
        return Some(Arc::clone(&*cached));
    }
    // Slow path: first time this pattern is seen — compile and insert.
    match Regex::new(pattern) {
        Ok(re) => {
            let arc = Arc::new(re);
            REGEX_CACHE.insert(pattern.to_string(), Arc::clone(&arc));
            Some(arc)
        }
        Err(_) => None,
    }
}

/// Dispatch a `regex.<name>(args)` free-function call.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── Test / match ─────────────────────────────────────────────────

        // `regex.test(pattern, subject)` → boolean
        // Returns true if the pattern matches anywhere in the subject string.
        "test" | "isMatch" | "is_match" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let result = compile(&pattern)
                .map(|re| re.is_match(&subject))
                .unwrap_or(false);
            Some(FidanValue::Boolean(result))
        }

        // `regex.match(pattern, subject)` → string | nothing
        // Returns the first match as a string, or `nothing` if no match.
        "match" | "find" | "find_first" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern).and_then(|re| re.find(&subject).map(|m| m.as_str().to_string()))
            {
                Some(s) => Some(str_val(&s)),
                None => Some(FidanValue::Nothing),
            }
        }

        // `regex.findAll(pattern, subject)` → list[string]
        // Returns a list of all non-overlapping matches.
        "findAll" | "find_all" | "matches" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Some(re) = compile(&pattern) {
                for m in re.find_iter(&subject) {
                    list.append(str_val(m.as_str()));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }

        // ── Capture groups ───────────────────────────────────────────────

        // `regex.capture(pattern, subject)` → list[string] | nothing
        // Returns a list of capture group strings for the first match
        // (index 0 is the full match, 1+ are capture groups).
        // Returns `nothing` if no match.
        "capture" | "exec" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => match re.captures(&subject) {
                    Some(caps) => {
                        let mut list = FidanList::new();
                        for g in caps.iter() {
                            match g {
                                Some(m) => list.append(str_val(m.as_str())),
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

        // `regex.captureAll(pattern, subject)` → list[list[string]]
        // Returns all matches with their capture groups.
        "captureAll" | "capture_all" | "execAll" | "exec_all" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut outer = FidanList::new();
            if let Some(re) = compile(&pattern) {
                for caps in re.captures_iter(&subject) {
                    let mut inner = FidanList::new();
                    for g in caps.iter() {
                        match g {
                            Some(m) => inner.append(str_val(m.as_str())),
                            None => inner.append(FidanValue::Nothing),
                        }
                    }
                    outer.append(FidanValue::List(OwnedRef::new(inner)));
                }
            }
            Some(FidanValue::List(OwnedRef::new(outer)))
        }

        // ── Replace ──────────────────────────────────────────────────────

        // `regex.replace(pattern, subject, replacement)` → string
        // Replaces the first occurrence of the pattern with `replacement`.
        // Use `$1`, `$2`, ... to refer to capture groups in `replacement`.
        "replace" | "replaceFirst" | "replace_first" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = as_str(args.get(2).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => Some(str_val(&re.replacen(&subject, 1, replacement.as_str()))),
                None => Some(str_val(&subject)),
            }
        }

        // `regex.replaceAll(pattern, subject, replacement)` → string
        // Replaces every occurrence of the pattern with `replacement`.
        "replaceAll" | "replace_all" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let replacement = as_str(args.get(2).unwrap_or(&FidanValue::Nothing));
            match compile(&pattern) {
                Some(re) => Some(str_val(&re.replace_all(&subject, replacement.as_str()))),
                None => Some(str_val(&subject)),
            }
        }

        // ── Utility ──────────────────────────────────────────────────────

        // `regex.split(pattern, subject)` → list[string]
        // Splits the subject string by the pattern.
        "split" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let subject = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            match compile(&pattern) {
                Some(re) => {
                    for part in re.split(&subject) {
                        list.append(str_val(part));
                    }
                }
                None => list.append(str_val(&subject)),
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }

        // `regex.isValid(pattern)` → boolean
        // Returns true if `pattern` is a valid regular expression.
        "isValid" | "is_valid" => {
            let pattern = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(compile(&pattern).is_some()))
        }

        _ => None,
    }
}
