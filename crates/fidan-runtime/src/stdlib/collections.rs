use crate::{FidanDict, FidanList, FidanString, FidanValue, OwnedRef, display};

use super::common::{list_value, string_value};

fn as_key(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Float(f) => f.to_string(),
        _ => display(v),
    }
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "range" => {
            let start = match args.first() {
                Some(FidanValue::Integer(n)) => *n,
                _ => 0,
            };
            let stop = match args.get(1) {
                Some(FidanValue::Integer(n)) => *n,
                _ => return Some(FidanValue::Nothing),
            };
            let step = match args.get(2) {
                Some(FidanValue::Integer(n)) => *n,
                _ => {
                    if stop >= start {
                        1
                    } else {
                        -1
                    }
                }
            };
            let mut list = FidanList::new();
            if step == 0 {
                return Some(FidanValue::List(OwnedRef::new(list)));
            }
            let mut i = start;
            while if step > 0 { i < stop } else { i > stop } {
                list.append(FidanValue::Integer(i));
                i += step;
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "Set" => {
            let mut dict = FidanDict::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for value in l.borrow().iter() {
                    dict.insert(FidanString::new(&as_key(value)), FidanValue::Boolean(true));
                }
            }
            Some(FidanValue::Dict(OwnedRef::new(dict)))
        }
        "setAdd" | "set_add" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                d.borrow_mut()
                    .insert(FidanString::new(&as_key(&value)), FidanValue::Boolean(true));
                Some(FidanValue::Dict(d))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setRemove" | "set_remove" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                d.borrow_mut().remove(&FidanString::new(&as_key(&value)));
                Some(FidanValue::Dict(d))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setContains" | "set_contains" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                Some(FidanValue::Boolean(
                    d.borrow().get(&FidanString::new(&as_key(&value))).is_some(),
                ))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "setToList" | "set_to_list" => {
            if let Some(FidanValue::Dict(d)) = args.first() {
                let mut list = FidanList::new();
                for (key, _) in d.borrow().iter() {
                    list.append(FidanValue::String(key.clone()));
                }
                Some(FidanValue::List(OwnedRef::new(list)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setLen" | "set_len" => match args.first() {
            Some(FidanValue::Dict(d)) => Some(FidanValue::Integer(d.borrow().len() as i64)),
            _ => Some(FidanValue::Integer(0)),
        },
        "setUnion" | "set_union" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let mut result = FidanDict::new();
                for (key, value) in a.borrow().iter() {
                    result.insert(key.clone(), value.clone());
                }
                for (key, value) in b.borrow().iter() {
                    result.insert(key.clone(), value.clone());
                }
                Some(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setIntersect" | "set_intersect" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let b_ref = b.borrow();
                let mut result = FidanDict::new();
                for (key, value) in a.borrow().iter() {
                    if b_ref.get(key).is_some() {
                        result.insert(key.clone(), value.clone());
                    }
                }
                Some(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setDiff" | "set_diff" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let b_ref = b.borrow();
                let mut result = FidanDict::new();
                for (key, value) in a.borrow().iter() {
                    if b_ref.get(key).is_none() {
                        result.insert(key.clone(), value.clone());
                    }
                }
                Some(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "Queue" | "Stack" => {
            let mut list = FidanList::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for value in l.borrow().iter() {
                    list.append(value.clone());
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "enqueue" | "push" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let value = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
                l.borrow_mut().append(value);
            }
            Some(FidanValue::Nothing)
        }
        "dequeue" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let mut borrow = l.borrow_mut();
                let items: Vec<FidanValue> = borrow.iter().cloned().collect();
                if items.is_empty() {
                    return Some(FidanValue::Nothing);
                }
                let first = items[0].clone();
                let mut new_list = FidanList::new();
                for item in items.into_iter().skip(1) {
                    new_list.append(item);
                }
                *borrow = new_list;
                Some(first)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "pop" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let mut borrow = l.borrow_mut();
                let items: Vec<FidanValue> = borrow.iter().cloned().collect();
                let len = items.len();
                if len == 0 {
                    return Some(FidanValue::Nothing);
                }
                let top = items[len - 1].clone();
                let mut new_list = FidanList::new();
                for item in items.into_iter().take(len - 1) {
                    new_list.append(item);
                }
                *borrow = new_list;
                Some(top)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "peek" => match args.first() {
            Some(FidanValue::List(l)) => {
                Some(l.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            }
            _ => Some(FidanValue::Nothing),
        },
        "stackPeek" | "stack_peek" | "top" => match args.first() {
            Some(FidanValue::List(l)) => {
                let borrow = l.borrow();
                Some(
                    borrow
                        .get(borrow.len().saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            }
            _ => Some(FidanValue::Nothing),
        },
        "flatten" => {
            if let Some(FidanValue::List(outer)) = args.first() {
                let mut result = FidanList::new();
                for item in outer.borrow().iter() {
                    if let FidanValue::List(inner) = item {
                        for value in inner.borrow().iter() {
                            result.append(value.clone());
                        }
                    } else {
                        result.append(item.clone());
                    }
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "zip" => {
            if let (Some(FidanValue::List(a)), Some(FidanValue::List(b))) =
                (args.first(), args.get(1))
            {
                let a_ref = a.borrow();
                let b_ref = b.borrow();
                let len = a_ref.len().min(b_ref.len());
                let mut result = FidanList::new();
                for index in 0..len {
                    result.append(list_value([
                        a_ref.get(index).cloned().unwrap_or(FidanValue::Nothing),
                        b_ref.get(index).cloned().unwrap_or(FidanValue::Nothing),
                    ]));
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "enumerate" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let mut result = FidanList::new();
                for (index, value) in list.borrow().iter().cloned().enumerate() {
                    result.append(list_value([FidanValue::Integer(index as i64), value]));
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "chunk" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let size = match args.get(1) {
                    Some(FidanValue::Integer(n)) if *n > 0 => *n as usize,
                    _ => 0,
                };
                if size == 0 {
                    return Some(FidanValue::List(OwnedRef::new(FidanList::new())));
                }
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let mut result = FidanList::new();
                for chunk in items.chunks(size) {
                    result.append(list_value(chunk.iter().cloned()));
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "window" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let size = match args.get(1) {
                    Some(FidanValue::Integer(n)) if *n > 0 => *n as usize,
                    _ => 0,
                };
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let mut result = FidanList::new();
                if size == 0 || size > items.len() {
                    return Some(FidanValue::List(OwnedRef::new(result)));
                }
                for window in items.windows(size) {
                    result.append(list_value(window.iter().cloned()));
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "partition" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let mut truthy = FidanList::new();
                let mut falsy = FidanList::new();
                for value in list.borrow().iter().cloned() {
                    if value.truthy() {
                        truthy.append(value);
                    } else {
                        falsy.append(value);
                    }
                }
                Some(list_value([
                    FidanValue::List(OwnedRef::new(truthy)),
                    FidanValue::List(OwnedRef::new(falsy)),
                ]))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "groupBy" | "group_by" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let mut groups = FidanDict::new();
                for value in list.borrow().iter().cloned() {
                    let key = FidanString::new(&as_key(&value));
                    match groups.get(&key).cloned() {
                        Some(FidanValue::List(existing)) => existing.borrow_mut().append(value),
                        _ => {
                            let mut bucket = FidanList::new();
                            bucket.append(value);
                            groups.insert(key, FidanValue::List(OwnedRef::new(bucket)));
                        }
                    }
                }
                Some(FidanValue::Dict(OwnedRef::new(groups)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "unique" | "dedup" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let mut seen = std::collections::HashSet::<String>::new();
                let mut result = FidanList::new();
                for value in list.borrow().iter() {
                    let key = as_key(value);
                    if seen.insert(key) {
                        result.append(value.clone());
                    }
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "reverse" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let items: Vec<FidanValue> = list.borrow().iter().rev().cloned().collect();
                Some(list_value(items))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "sort" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let mut items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                items.sort_by(|a, b| match (a, b) {
                    (FidanValue::Integer(x), FidanValue::Integer(y)) => x.cmp(y),
                    (FidanValue::Float(x), FidanValue::Float(y)) => {
                        x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (FidanValue::String(x), FidanValue::String(y)) => x.as_str().cmp(y.as_str()),
                    _ => std::cmp::Ordering::Equal,
                });
                Some(list_value(items))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "count" | "length" | "len" => match args.first() {
            Some(FidanValue::List(list)) => Some(FidanValue::Integer(list.borrow().len() as i64)),
            Some(FidanValue::Dict(dict)) => Some(FidanValue::Integer(dict.borrow().len() as i64)),
            _ => Some(FidanValue::Integer(0)),
        },
        "isEmpty" | "is_empty" => match args.first() {
            Some(FidanValue::List(list)) => Some(FidanValue::Boolean(list.borrow().is_empty())),
            Some(FidanValue::Dict(dict)) => Some(FidanValue::Boolean(dict.borrow().is_empty())),
            _ => Some(FidanValue::Boolean(true)),
        },
        "concat" => {
            let mut result = FidanList::new();
            for arg in &args {
                if let FidanValue::List(list) = arg {
                    for value in list.borrow().iter() {
                        result.append(value.clone());
                    }
                }
            }
            Some(FidanValue::List(OwnedRef::new(result)))
        }
        "slice" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let items: Vec<FidanValue> = list.borrow().iter().cloned().collect();
                let len = items.len();
                let start = match args.get(1) {
                    Some(FidanValue::Integer(n)) => (*n).max(0) as usize,
                    _ => 0,
                };
                let end = match args.get(2) {
                    Some(FidanValue::Integer(n)) => (*n as usize).min(len),
                    _ => len,
                };
                Some(list_value(
                    items[start.min(len)..end.min(len)].iter().cloned(),
                ))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "first" => match args.first() {
            Some(FidanValue::List(list)) => {
                Some(list.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            }
            _ => Some(FidanValue::Nothing),
        },
        "last" => match args.first() {
            Some(FidanValue::List(list)) => {
                let borrow = list.borrow();
                Some(
                    borrow
                        .get(borrow.len().saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            }
            _ => Some(FidanValue::Nothing),
        },
        "join" => {
            let list = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let sep = args.get(1).map(as_key).unwrap_or_default();
            if let FidanValue::List(list) = list {
                let parts: Vec<String> = list.borrow().iter().map(as_key).collect();
                Some(string_value(&parts.join(&sep)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "sum" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let total: f64 = list
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        FidanValue::Integer(n) => *n as f64,
                        FidanValue::Float(f) => *f,
                        _ => 0.0,
                    })
                    .sum();
                if total == total.floor() && total.abs() < i64::MAX as f64 {
                    Some(FidanValue::Integer(total as i64))
                } else {
                    Some(FidanValue::Float(total))
                }
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "product" => {
            if let Some(FidanValue::List(list)) = args.first() {
                let total: f64 = list
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        FidanValue::Integer(n) => *n as f64,
                        FidanValue::Float(f) => *f,
                        _ => 1.0,
                    })
                    .product();
                if total == total.floor() && total.abs() < i64::MAX as f64 {
                    Some(FidanValue::Integer(total as i64))
                } else {
                    Some(FidanValue::Float(total))
                }
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "min" => {
            if let Some(FidanValue::List(list)) = args.first() {
                list.borrow()
                    .iter()
                    .cloned()
                    .reduce(|a, b| match (&a, &b) {
                        (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                            if x <= y {
                                a
                            } else {
                                b
                            }
                        }
                        _ => a,
                    })
                    .or(Some(FidanValue::Nothing))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "max" => {
            if let Some(FidanValue::List(list)) = args.first() {
                list.borrow()
                    .iter()
                    .cloned()
                    .reduce(|a, b| match (&a, &b) {
                        (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                            if x >= y {
                                a
                            } else {
                                b
                            }
                        }
                        _ => a,
                    })
                    .or(Some(FidanValue::Nothing))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "range",
        "Set",
        "setAdd",
        "set_add",
        "setRemove",
        "set_remove",
        "setContains",
        "set_contains",
        "setToList",
        "set_to_list",
        "setLen",
        "set_len",
        "setUnion",
        "set_union",
        "setIntersect",
        "set_intersect",
        "setDiff",
        "set_diff",
        "Queue",
        "enqueue",
        "dequeue",
        "peek",
        "Stack",
        "push",
        "pop",
        "top",
        "stackPeek",
        "stack_peek",
        "flatten",
        "zip",
        "enumerate",
        "chunk",
        "window",
        "partition",
        "groupBy",
        "group_by",
        "unique",
        "dedup",
        "reverse",
        "sort",
        "count",
        "length",
        "len",
        "isEmpty",
        "is_empty",
        "concat",
        "slice",
        "first",
        "last",
        "join",
        "sum",
        "product",
        "min",
        "max",
    ]
}
