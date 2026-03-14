//! `std.collections` — Higher-order collection data structures for Fidan.
//!
//! Provides:
//! - `Set` — unordered set of unique values
//! - `Queue` — FIFO queue
//! - `Stack` — LIFO stack (alias: deque push-back/pop-back)
//! - `range(start, stop, step?)` — lazy integer range -> List
//! - Higher-order: `map`, `filter`, `reduce`, `forEach`, `any`, `all`, `zip`, `flatten`
//!
//! Object-style constructors: `Set()`, `Queue()` are emitted into the `collections` namespace.
//! Higher-order functions operate on `List` values.

use fidan_runtime::{FidanDict, FidanList, FidanString, FidanValue, OwnedRef};

fn as_str(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Float(f) => f.to_string(),
        _ => String::new(),
    }
}

/// Dispatch a `collections.<name>(args)` call.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── Range ──────────────────────────────────────────────────────────
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

        // ── Set constructor ────────────────────────────────────────────────
        // Returns a Dict with only true-valued entries (set semantics).
        // `collections.Set()` or `collections.Set(existingList)` to build from list.
        "Set" => {
            let mut dict = FidanDict::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    let key = FidanString::new(&as_str(v));
                    dict.insert(key, FidanValue::Boolean(true));
                }
            }
            Some(FidanValue::Dict(OwnedRef::new(dict)))
        }
        "setAdd" | "set_add" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                let key = FidanString::new(&as_str(&val));
                d.borrow_mut().insert(key, FidanValue::Boolean(true));
                Some(FidanValue::Dict(d))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setRemove" | "set_remove" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                let key = FidanString::new(&as_str(&val));
                d.borrow_mut().remove(&key);
                Some(FidanValue::Dict(d))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setContains" | "set_contains" => {
            let set = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Dict(d) = set {
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                let key = FidanString::new(&as_str(&val));
                Some(FidanValue::Boolean(d.borrow().get(&key).is_some()))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        "setToList" | "set_to_list" => {
            if let Some(FidanValue::Dict(d)) = args.first() {
                let mut list = FidanList::new();
                for (k, _) in d.borrow().iter() {
                    list.append(FidanValue::String(k.clone()));
                }
                Some(FidanValue::List(OwnedRef::new(list)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "setLen" | "set_len" => {
            if let Some(FidanValue::Dict(d)) = args.first() {
                Some(FidanValue::Integer(d.borrow().len() as i64))
            } else {
                Some(FidanValue::Integer(0))
            }
        }
        "setUnion" | "set_union" => {
            if let (Some(FidanValue::Dict(a)), Some(FidanValue::Dict(b))) =
                (args.first(), args.get(1))
            {
                let mut result = FidanDict::new();
                for (k, v) in a.borrow().iter() {
                    result.insert(k.clone(), v.clone());
                }
                for (k, v) in b.borrow().iter() {
                    result.insert(k.clone(), v.clone());
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
                let mut result = FidanDict::new();
                let b_ref = b.borrow();
                for (k, v) in a.borrow().iter() {
                    if b_ref.get(k).is_some() {
                        result.insert(k.clone(), v.clone());
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
                let mut result = FidanDict::new();
                let b_ref = b.borrow();
                for (k, v) in a.borrow().iter() {
                    if b_ref.get(k).is_none() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Some(FidanValue::Dict(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }

        // ── Queue ─────────────────────────────────────────────────────────
        // Queue is a List used in FIFO order; we provide helpers that make this idiomatic.
        "Queue" => {
            let mut list = FidanList::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    list.append(v.clone());
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "enqueue" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let l = l.clone();
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                l.borrow_mut().append(val);
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
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
        "peek" => {
            if let Some(FidanValue::List(l)) = args.first() {
                Some(l.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            } else {
                Some(FidanValue::Nothing)
            }
        }

        // ── Stack ─────────────────────────────────────────────────────────
        "Stack" => {
            let mut list = FidanList::new();
            if let Some(FidanValue::List(l)) = args.first() {
                for v in l.borrow().iter() {
                    list.append(v.clone());
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "push" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let l = l.clone();
                let val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
                l.borrow_mut().append(val);
                Some(FidanValue::Nothing)
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
        "stackPeek" | "top" | "stack_peek" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let borrow = l.borrow();
                let len = borrow.len();
                Some(
                    borrow
                        .get(len.saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                Some(FidanValue::Nothing)
            }
        }

        // ── Higher-order (non-callback; callback-based versions need MIR dispatch) ──
        // These operate purely on List values without callbacks.
        "flatten" => {
            if let Some(FidanValue::List(outer)) = args.first() {
                let mut result = FidanList::new();
                for item in outer.borrow().iter() {
                    if let FidanValue::List(inner) = item {
                        for v in inner.borrow().iter() {
                            result.append(v.clone());
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
                for i in 0..len {
                    let pair = vec![
                        a_ref.get(i).cloned().unwrap_or(FidanValue::Nothing),
                        b_ref.get(i).cloned().unwrap_or(FidanValue::Nothing),
                    ];
                    let mut inner_list = FidanList::new();
                    for v in pair {
                        inner_list.append(v);
                    }
                    result.append(FidanValue::List(OwnedRef::new(inner_list)));
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "unique" | "dedup" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let mut seen = std::collections::HashSet::<String>::new();
                let mut result = FidanList::new();
                for v in l.borrow().iter() {
                    let key = as_str(v);
                    if seen.insert(key) {
                        result.append(v.clone());
                    }
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "reverse" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let items: Vec<FidanValue> = l.borrow().iter().rev().cloned().collect();
                let mut result = FidanList::new();
                for v in items {
                    result.append(v);
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "sort" => {
            // sort(list) — sorts integers/floats numerically, strings lexicographically.
            if let Some(FidanValue::List(l)) = args.first() {
                let mut items: Vec<FidanValue> = l.borrow().iter().cloned().collect();
                items.sort_by(|a, b| match (a, b) {
                    (FidanValue::Integer(x), FidanValue::Integer(y)) => x.cmp(y),
                    (FidanValue::Float(x), FidanValue::Float(y)) => {
                        x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    (FidanValue::String(x), FidanValue::String(y)) => x.as_str().cmp(y.as_str()),
                    _ => std::cmp::Ordering::Equal,
                });
                let mut result = FidanList::new();
                for v in items {
                    result.append(v);
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "count" | "length" | "len" => match args.first() {
            Some(FidanValue::List(l)) => Some(FidanValue::Integer(l.borrow().len() as i64)),
            Some(FidanValue::Dict(d)) => Some(FidanValue::Integer(d.borrow().len() as i64)),
            _ => Some(FidanValue::Integer(0)),
        },
        "isEmpty" | "is_empty" => match args.first() {
            Some(FidanValue::List(l)) => Some(FidanValue::Boolean(l.borrow().len() == 0)),
            Some(FidanValue::Dict(d)) => Some(FidanValue::Boolean(d.borrow().len() == 0)),
            _ => Some(FidanValue::Boolean(true)),
        },
        "concat" => {
            let mut result = FidanList::new();
            for arg in &args {
                if let FidanValue::List(l) = arg {
                    for v in l.borrow().iter() {
                        result.append(v.clone());
                    }
                }
            }
            Some(FidanValue::List(OwnedRef::new(result)))
        }
        "slice" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let items: Vec<FidanValue> = l.borrow().iter().cloned().collect();
                let len = items.len();
                let start = match args.get(1) {
                    Some(FidanValue::Integer(n)) => (*n).max(0) as usize,
                    _ => 0,
                };
                let end = match args.get(2) {
                    Some(FidanValue::Integer(n)) => (*n as usize).min(len),
                    _ => len,
                };
                let mut result = FidanList::new();
                for v in items[start.min(len)..end.min(len)].iter() {
                    result.append(v.clone());
                }
                Some(FidanValue::List(OwnedRef::new(result)))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "first" => match args.first() {
            Some(FidanValue::List(l)) => {
                Some(l.borrow().get(0).cloned().unwrap_or(FidanValue::Nothing))
            }
            _ => Some(FidanValue::Nothing),
        },
        "last" => match args.first() {
            Some(FidanValue::List(l)) => {
                let borrow = l.borrow();
                let len = borrow.len();
                Some(
                    borrow
                        .get(len.saturating_sub(1))
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            }
            _ => Some(FidanValue::Nothing),
        },
        "join" => {
            let list = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let sep = args.get(1).map(|v| as_str(v)).unwrap_or_default();
            if let FidanValue::List(l) = list {
                let parts: Vec<String> = l.borrow().iter().map(|v| as_str(v)).collect();
                Some(FidanValue::String(FidanString::new(&parts.join(&sep))))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "sum" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let total: f64 = l
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
            if let Some(FidanValue::List(l)) = args.first() {
                let prod: f64 = l
                    .borrow()
                    .iter()
                    .map(|v| match v {
                        FidanValue::Integer(n) => *n as f64,
                        FidanValue::Float(f) => *f,
                        _ => 1.0,
                    })
                    .product();
                if prod == prod.floor() && prod.abs() < i64::MAX as f64 {
                    Some(FidanValue::Integer(prod as i64))
                } else {
                    Some(FidanValue::Float(prod))
                }
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "min" => {
            if let Some(FidanValue::List(l)) = args.first() {
                let items: Vec<FidanValue> = l.borrow().iter().cloned().collect();
                items
                    .into_iter()
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
            if let Some(FidanValue::List(l)) = args.first() {
                let items: Vec<FidanValue> = l.borrow().iter().cloned().collect();
                items
                    .into_iter()
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
