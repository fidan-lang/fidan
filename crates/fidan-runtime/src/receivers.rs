//! Shared pure receiver operations. Callback dispatch stays in each execution engine.
use crate::stdlib::StdlibRuntimeError;
use crate::{FidanList, FidanString, FidanValue, OwnedRef, display, stdlib};
use fidan_config::{ReceiverBuiltinKind, infer_receiver_member};
fn error(message: impl Into<String>) -> StdlibRuntimeError {
    StdlibRuntimeError::new(fidan_diagnostics::diag_code!("R0001"), message)
}

pub fn string_method(
    s: FidanString,
    method: &str,
    args: Vec<FidanValue>,
) -> Result<FidanValue, StdlibRuntimeError> {
    let str_val = s.as_str().to_owned();
    let Some(method) =
        infer_receiver_member(ReceiverBuiltinKind::String, method).map(|info| info.canonical_name)
    else {
        return Err(error(format!("no method `{method}` found for `string`")));
    };
    match method {
        // ── Case ──────────────────────────────────────────────────────────────
        "lower" => Ok(FidanValue::String(FidanString::new(
            &str_val.to_lowercase(),
        ))),
        "upper" => Ok(FidanValue::String(FidanString::new(
            &str_val.to_uppercase(),
        ))),
        "capitalize" => {
            let mut chars = str_val.chars();
            let capitalized = match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            };
            Ok(FidanValue::String(FidanString::new(&capitalized)))
        }

        // ── Trim ──────────────────────────────────────────────────────────────
        "trim" => Ok(FidanValue::String(FidanString::new(str_val.trim()))),
        "trimStart" => Ok(FidanValue::String(FidanString::new(str_val.trim_start()))),
        "trimEnd" => Ok(FidanValue::String(FidanString::new(str_val.trim_end()))),

        // ── Length ────────────────────────────────────────────────────────────
        "len" => Ok(FidanValue::Integer(str_val.chars().count() as i64)),
        "byteLen" => Ok(FidanValue::Integer(str_val.len() as i64)),
        "isEmpty" => Ok(FidanValue::Boolean(str_val.is_empty())),

        // ── Split / lines ─────────────────────────────────────────────────────
        "split" => {
            let sep = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_else(|| " ".to_owned());
            let parts = str_val.split(sep.as_str());
            let (lower, upper) = parts.size_hint();
            let mut list = FidanList::with_capacity(upper.unwrap_or(lower));
            for part in parts {
                list.append(FidanValue::String(FidanString::new(part)));
            }
            Ok(FidanValue::List(OwnedRef::new(list)))
        }
        "lines" => {
            let lines = str_val.lines();
            let (lower, upper) = lines.size_hint();
            let mut list = FidanList::with_capacity(upper.unwrap_or(lower));
            for line in lines {
                list.append(FidanValue::String(FidanString::new(line)));
            }
            Ok(FidanValue::List(OwnedRef::new(list)))
        }
        "chars" => {
            let mut list = FidanList::with_capacity(str_val.chars().count());
            for c in str_val.chars() {
                list.append(FidanValue::String(FidanString::new(&c.to_string())));
            }
            Ok(FidanValue::List(OwnedRef::new(list)))
        }

        // ── Join (receiver is separator) ──────────────────────────────────────
        "join" => {
            let collection = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            match collection {
                FidanValue::List(l) => {
                    let borrow = l.borrow();
                    let mut joined =
                        String::with_capacity(str_val.len().saturating_mul(borrow.len()));
                    for (index, value) in borrow.iter().enumerate() {
                        if index > 0 {
                            joined.push_str(&str_val);
                        }
                        joined.push_str(&crate::stdlib::common::display_string(value));
                    }
                    Ok(FidanValue::String(FidanString::new(&joined)))
                }
                _ => Ok(FidanValue::String(FidanString::new(""))),
            }
        }

        // ── Search ────────────────────────────────────────────────────────────
        "contains" => {
            let pat = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::Boolean(str_val.contains(pat.as_str())))
        }
        "startsWith" => {
            let pat = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::Boolean(str_val.starts_with(pat.as_str())))
        }
        "endsWith" => {
            let pat = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::Boolean(str_val.ends_with(pat.as_str())))
        }
        "indexOf" | "lastIndexOf" => {
            let mut values = Vec::with_capacity(args.len() + 1);
            values.push(FidanValue::String(s));
            values.extend(args);
            Ok(stdlib::string::dispatch(method, values).unwrap_or(FidanValue::Nothing))
        }

        // ── Mutation / transformation ─────────────────────────────────────────
        "replace" => {
            let from = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            let to = args
                .get(1)
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::String(FidanString::new(
                &str_val.replace(from.as_str(), to.as_str()),
            )))
        }
        "replaceAll" => {
            let from = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            let to = args
                .get(1)
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::String(FidanString::new(
                &str_val.replace(from.as_str(), to.as_str()),
            )))
        }
        "replaceFirst" => {
            let from = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            let to = args
                .get(1)
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            Ok(FidanValue::String(FidanString::new(&str_val.replacen(
                from.as_str(),
                to.as_str(),
                1,
            ))))
        }
        "reverse" => {
            let rev: String = str_val.chars().rev().collect();
            Ok(FidanValue::String(FidanString::new(&rev)))
        }

        // ── Indexing / slicing ────────────────────────────────────────────────
        "charAt" | "substring" | "padStart" | "padEnd" | "repeat" => {
            let mut values = Vec::with_capacity(args.len() + 1);
            values.push(FidanValue::String(s));
            values.extend(args);
            Ok(stdlib::string::dispatch(method, values).unwrap_or(FidanValue::Nothing))
        }

        // ── Bytes / char codes ────────────────────────────────────────────────
        "bytes" => {
            let mut list = FidanList::with_capacity(str_val.len());
            for b in str_val.bytes() {
                list.append(FidanValue::Integer(b as i64));
            }
            Ok(FidanValue::List(OwnedRef::new(list)))
        }
        "charCode" => {
            let code = str_val.chars().next().map(|c| c as i64).unwrap_or(0);
            Ok(FidanValue::Integer(code))
        }
        // ── format (string.format with {} placeholders) ────────────────────
        "format" => {
            let mut result = str_val.clone();
            for arg in &args {
                if let Some(pos) = result.find("{}") {
                    result.replace_range(pos..pos + 2, &crate::stdlib::common::display_string(arg));
                }
            }
            Ok(FidanValue::String(FidanString::new(&result)))
        }
        _ => Err(error(format!(
            "no implementation for string method `{method}`"
        ))),
    }
}

/// Non-callback list operations shared by interpreted and native execution.
pub fn list_method(
    list: &OwnedRef<FidanList>,
    method: &str,
    args: Vec<FidanValue>,
) -> Result<Option<FidanValue>, StdlibRuntimeError> {
    let Some(method) =
        infer_receiver_member(ReceiverBuiltinKind::List, method).map(|info| info.canonical_name)
    else {
        return Ok(None);
    };
    Ok(match method {
        "len" => Some(FidanValue::Integer(list.borrow().len() as i64)),
        "isEmpty" => Some(FidanValue::Boolean(list.borrow().is_empty())),
        "append" => {
            for value in args {
                list.borrow_mut().append(value);
            }
            Some(FidanValue::Nothing)
        }
        "pop" => {
            let mut b = list.borrow_mut();
            Some(b.pop().unwrap_or(FidanValue::Nothing))
        }
        "first" => Some(list.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing)),
        "last" => {
            let b = list.borrow();
            let len = b.len();
            Some(
                b.get(len.saturating_sub(1))
                    .cloned()
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "get" => {
            let idx = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let b = list.borrow();
            let i = if idx < 0 {
                usize::try_from(b.len() as i128 + i128::from(idx)).unwrap_or(usize::MAX)
            } else {
                idx as usize
            };
            Some(b.get(i).cloned().unwrap_or(FidanValue::Nothing))
        }
        "contains" => {
            let target = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = list.borrow();
            let found = b.iter().any(|v| crate::ffi::values_equal(v, &target));
            Some(FidanValue::Boolean(found))
        }
        "indexOf" | "find" => {
            let target = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = list.borrow();
            let idx = b
                .iter()
                .position(|v| crate::ffi::values_equal(v, &target))
                .map(|i| i as i64)
                .unwrap_or(-1);
            Some(FidanValue::Integer(idx))
        }
        "reverse" => {
            list.borrow_mut().reverse();
            Some(FidanValue::Nothing)
        }
        "reversed" => {
            let b = list.borrow();
            let mut items: Vec<FidanValue> = b.iter().cloned().collect();
            items.reverse();
            Some(FidanValue::List(OwnedRef::new(FidanList::from_vec(items))))
        }
        "sort" => {
            list.borrow_mut().sort_by(crate::ffi::compare_values);
            Some(FidanValue::Nothing)
        }
        "join" => {
            let sep = args
                .first()
                .map(crate::stdlib::common::display_string)
                .unwrap_or_default();
            let b = list.borrow();
            let mut joined = String::with_capacity(sep.len().saturating_mul(b.len()));
            for (index, value) in b.iter().enumerate() {
                if index > 0 {
                    joined.push_str(&sep);
                }
                joined.push_str(&crate::stdlib::common::display_string(value));
            }
            Some(FidanValue::String(FidanString::new(&joined)))
        }
        "slice" => {
            let start = args
                .first()
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .unwrap_or(0)
                .max(0) as usize;
            let b = list.borrow();
            let end = args
                .get(1)
                .and_then(|v| {
                    if let FidanValue::Integer(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                })
                .map(|e| (e.max(0) as usize).min(b.len()))
                .unwrap_or(b.len());
            let start = start.min(b.len());
            let mut new_list = FidanList::with_capacity(end.saturating_sub(start));
            for i in start..end {
                if let Some(v) = b.get(i) {
                    new_list.append(v.clone());
                }
            }
            Some(FidanValue::List(OwnedRef::new(new_list)))
        }
        "flatten" => {
            let b = list.borrow();
            let capacity = b
                .iter()
                .map(|v| match v {
                    FidanValue::List(inner) => inner.borrow().len(),
                    _ => 1,
                })
                .sum();
            let mut new_list = FidanList::with_capacity(capacity);
            for v in b.iter() {
                match v {
                    FidanValue::List(inner) => {
                        for item in inner.borrow().iter() {
                            new_list.append(item.clone());
                        }
                    }
                    other => new_list.append(other.clone()),
                }
            }
            Some(FidanValue::List(OwnedRef::new(new_list)))
        }
        "extend" => {
            let other = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            match other {
                FidanValue::List(other_l) => {
                    // Snapshot before mutation: extending a list with itself is valid.
                    let values: Vec<_> = other_l.borrow().iter().cloned().collect();
                    for value in values {
                        list.borrow_mut().append(value);
                    }
                    Some(FidanValue::Nothing)
                }
                _ => return Err(error("List.extend expects a list")),
            }
        }
        "remove" => {
            let Some(FidanValue::Integer(index)) = args.first() else {
                return Err(error("List.remove expects an integer index"));
            };
            let mut values = list.borrow_mut();
            let offset = if *index < 0 {
                values.len() as i128 + i128::from(*index)
            } else {
                i128::from(*index)
            };
            Some(
                usize::try_from(offset)
                    .ok()
                    .and_then(|index| values.remove(index))
                    .unwrap_or(FidanValue::Nothing),
            )
        }
        "toString" => {
            let b = list.borrow();
            let items: Vec<String> = b.iter().map(display).collect();
            Some(FidanValue::String(FidanString::new(&format!(
                "[{}]",
                items.join(", ")
            ))))
        }
        _ => None,
    })
}

/// Callbacks run against a snapshot, outside the list borrow. Engines provide only
/// their existing generic invocation; errors stop iteration without substitution.
pub fn list_callback<E: From<StdlibRuntimeError>>(
    list: &OwnedRef<FidanList>,
    method: &str,
    args: Vec<FidanValue>,
    mut invoke: impl FnMut(FidanValue, Vec<FidanValue>) -> Result<FidanValue, E>,
) -> Result<Option<FidanValue>, E> {
    let Some(method) =
        infer_receiver_member(ReceiverBuiltinKind::List, method).map(|info| info.canonical_name)
    else {
        return Ok(None);
    };
    if !matches!(
        method,
        "map" | "filter" | "forEach" | "firstWhere" | "reduce"
    ) {
        return Ok(None);
    }
    let Some(
        callback
        @ (FidanValue::Function(_) | FidanValue::Closure { .. } | FidanValue::StdlibFn(..)),
    ) = args.first()
    else {
        return Err(error(format!("List.{method} expects an action")).into());
    };
    let items: Vec<_> = list.borrow().iter().cloned().collect();
    let mut output = Vec::new();
    let mut accumulator = args.get(1).cloned();
    for item in items {
        if method == "reduce" {
            accumulator = Some(match accumulator.take() {
                Some(value) => invoke(callback.clone(), vec![value, item])?,
                None => item,
            });
            continue;
        }
        let result = invoke(callback.clone(), vec![item.clone()])?;
        match method {
            "map" => output.push(result),
            "filter" if result.truthy() => output.push(item),
            "firstWhere" if result.truthy() => return Ok(Some(item)),
            _ => {}
        }
    }
    Ok(Some(match method {
        "map" | "filter" => FidanValue::List(OwnedRef::new(FidanList::from_vec(output))),
        "reduce" => accumulator.unwrap_or(FidanValue::Nothing),
        _ => FidanValue::Nothing,
    }))
}
