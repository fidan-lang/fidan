use crate::{FidanDict, FidanHashSet, FidanList, FidanValue, OwnedRef, display};

use super::common::{list_value, string_value};

fn compare_collection_values(left: &FidanValue, right: &FidanValue) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (FidanValue::Integer(lhs), FidanValue::Integer(rhs)) => Some(lhs.cmp(rhs)),
        (FidanValue::Float(lhs), FidanValue::Float(rhs)) => lhs.partial_cmp(rhs),
        (FidanValue::Integer(lhs), FidanValue::Float(rhs)) => (*lhs as f64).partial_cmp(rhs),
        (FidanValue::Float(lhs), FidanValue::Integer(rhs)) => lhs.partial_cmp(&(*rhs as f64)),
        (FidanValue::String(lhs), FidanValue::String(rhs)) => Some(lhs.as_str().cmp(rhs.as_str())),
        _ => None,
    }
}

fn set_from_value(value: &FidanValue) -> Option<FidanHashSet> {
    match value {
        FidanValue::HashSet(set) => Some(set.borrow().clone()),
        FidanValue::List(list) => FidanHashSet::from_values(list.borrow().iter().cloned()).ok(),
        FidanValue::Nothing => Some(FidanHashSet::new()),
        _ => None,
    }
}

fn set_to_list(set: &FidanHashSet) -> FidanValue {
    let mut list = FidanList::new();
    for value in set.values_sorted() {
        list.append(value);
    }
    FidanValue::List(OwnedRef::new(list))
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
        "hashset" => {
            let source = args.first().cloned().unwrap_or(FidanValue::Nothing);
            set_from_value(&source).map(|set| FidanValue::HashSet(OwnedRef::new(set)))
        }
        "setAdd" | "set_add" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::HashSet(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                let _ = d.borrow_mut().insert(value);
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setRemove" | "set_remove" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::HashSet(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                let _ = d.borrow_mut().remove(&value);
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setContains" | "set_contains" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::HashSet(d) = set {
                let value = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                Some(FidanValue::Boolean(
                    d.borrow().contains(&value).unwrap_or(false),
                ))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "setToList" | "set_to_list" => {
            if let Some(FidanValue::HashSet(d)) = args.first() {
                Some(set_to_list(&d.borrow()))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setLen" | "set_len" => match args.first() {
            Some(FidanValue::HashSet(d)) => Some(FidanValue::Integer(d.borrow().len() as i64)),
            _ => Some(FidanValue::Integer(0)),
        },
        "setUnion" | "set_union" => {
            if let (Some(FidanValue::HashSet(a)), Some(FidanValue::HashSet(b))) =
                (args.first(), args.get(1))
            {
                Some(FidanValue::HashSet(OwnedRef::new(
                    a.borrow().union(&b.borrow()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setIntersect" | "set_intersect" => {
            if let (Some(FidanValue::HashSet(a)), Some(FidanValue::HashSet(b))) =
                (args.first(), args.get(1))
            {
                Some(FidanValue::HashSet(OwnedRef::new(
                    a.borrow().intersection(&b.borrow()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setDiff" | "set_diff" => {
            if let (Some(FidanValue::HashSet(a)), Some(FidanValue::HashSet(b))) =
                (args.first(), args.get(1))
            {
                Some(FidanValue::HashSet(OwnedRef::new(
                    a.borrow().difference(&b.borrow()),
                )))
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
                    result.append(FidanValue::Tuple(vec![
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
                    result.append(FidanValue::Tuple(vec![
                        FidanValue::Integer(index as i64),
                        value,
                    ]));
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
                Some(FidanValue::Tuple(vec![
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
                    match groups.get(&value).ok().flatten().cloned() {
                        Some(FidanValue::List(existing)) => existing.borrow_mut().append(value),
                        _ => {
                            let mut bucket = FidanList::new();
                            bucket.append(value.clone());
                            let _ = groups.insert(value, FidanValue::List(OwnedRef::new(bucket)));
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
                let mut seen = FidanHashSet::new();
                let mut result = FidanList::new();
                for value in list.borrow().iter() {
                    if seen.insert(value.clone()).unwrap_or(false) {
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
                items.sort_by(|a, b| {
                    compare_collection_values(a, b).unwrap_or(std::cmp::Ordering::Equal)
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
            let sep = args.get(1).map(display).unwrap_or_default();
            if let FidanValue::List(list) = list {
                let parts: Vec<String> = list.borrow().iter().map(display).collect();
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
                    .reduce(|a, b| match compare_collection_values(&a, &b) {
                        Some(std::cmp::Ordering::Greater) => b,
                        Some(_) => a,
                        None => a,
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
                    .reduce(|a, b| match compare_collection_values(&a, &b) {
                        Some(std::cmp::Ordering::Less) => b,
                        Some(_) => a,
                        None => a,
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
        "hashset",
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
