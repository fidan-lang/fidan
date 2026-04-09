//! Bootstrap list methods — placeholder until `std.collections` (Phase 7).

use fidan_config::{ReceiverBuiltinKind, infer_receiver_member};
use fidan_runtime::{FidanList, FidanString, FidanValue, OwnedRef};

/// Value equality used internally for `contains` and `find`.
fn values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x == y,
        (FidanValue::Float(x), FidanValue::Float(y)) => x == y,
        (FidanValue::Boolean(x), FidanValue::Boolean(y)) => x == y,
        (FidanValue::String(x), FidanValue::String(y)) => x.as_str() == y.as_str(),
        (FidanValue::Nothing, FidanValue::Nothing) => true,
        _ => false,
    }
}

pub fn dispatch(r: OwnedRef<FidanList>, method: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    let method = infer_receiver_member(ReceiverBuiltinKind::List, method)?.canonical_name;
    match method {
        "append" => {
            for arg in args {
                r.borrow_mut().append(arg);
            }
            Some(FidanValue::Nothing)
        }
        "len" => Some(FidanValue::Integer(r.borrow().len() as i64)),
        "isEmpty" => Some(FidanValue::Boolean(r.borrow().is_empty())),
        "get" => {
            if let Some(FidanValue::Integer(i)) = args.first() {
                Some(
                    r.borrow()
                        .get(*i as usize)
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "pop" => {
            let len = r.borrow().len();
            if len > 0 {
                let val = r
                    .borrow()
                    .get(len - 1)
                    .cloned()
                    .unwrap_or(FidanValue::Nothing);
                let items: Vec<FidanValue> = r.borrow().iter().take(len - 1).cloned().collect();
                let mut new_list = FidanList::new();
                for item in items {
                    new_list.append(item);
                }
                *r.borrow_mut() = new_list;
                Some(val)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "remove" => {
            let idx = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Integer(i) = idx {
                let i = i as usize;
                let items: Vec<FidanValue> = r.borrow().iter().cloned().collect();
                let removed = items.get(i).cloned().unwrap_or(FidanValue::Nothing);
                let mut new_list = FidanList::new();
                for (pos, v) in items.into_iter().enumerate() {
                    if pos != i {
                        new_list.append(v);
                    }
                }
                *r.borrow_mut() = new_list;
                Some(removed)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "reverse" => {
            let items: Vec<FidanValue> = r.borrow().iter().cloned().collect();
            let mut new_list = FidanList::new();
            for v in items.into_iter().rev() {
                new_list.append(v);
            }
            *r.borrow_mut() = new_list;
            Some(FidanValue::Nothing)
        }
        "sort" => {
            let mut items: Vec<FidanValue> = r.borrow().iter().cloned().collect();
            items.sort_by(|a, b| match (a, b) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => x.cmp(y),
                (FidanValue::Float(x), FidanValue::Float(y)) => {
                    x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                }
                (FidanValue::String(x), FidanValue::String(y)) => x.as_str().cmp(y.as_str()),
                _ => std::cmp::Ordering::Equal,
            });
            let mut new_list = FidanList::new();
            for v in items {
                new_list.append(v);
            }
            *r.borrow_mut() = new_list;
            Some(FidanValue::Nothing)
        }
        "join" => {
            let sep = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let sep_str = match sep {
                FidanValue::String(s) => s.as_str().to_string(),
                _ => String::new(),
            };
            let parts: Vec<String> = r
                .borrow()
                .iter()
                .map(|v| match v {
                    FidanValue::String(s) => s.as_str().to_string(),
                    FidanValue::Integer(n) => n.to_string(),
                    FidanValue::Float(f) => f.to_string(),
                    FidanValue::Boolean(b) => b.to_string(),
                    FidanValue::Nothing => "nothing".to_string(),
                    _ => String::new(),
                })
                .collect();
            Some(FidanValue::String(FidanString::new(&parts.join(&sep_str))))
        }
        "toString" => Some(FidanValue::String(FidanString::new(
            &fidan_runtime::display(&FidanValue::List(r.clone())),
        ))),
        "contains" => {
            let target = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let found = r.borrow().iter().any(|v| values_equal(v, &target));
            Some(FidanValue::Boolean(found))
        }
        "indexOf" | "find" => {
            let target = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let pos = r
                .borrow()
                .iter()
                .position(|v| values_equal(v, &target))
                .map(|i| FidanValue::Integer(i as i64))
                .unwrap_or(FidanValue::Integer(-1));
            Some(pos)
        }
        // Returns a new reversed list without mutating the original.
        // Complements the in-place `reverse()` for functional-style use.
        "reversed" => {
            let items: Vec<FidanValue> = r.borrow().iter().cloned().collect();
            let mut new_list = FidanList::new();
            for v in items.into_iter().rev() {
                new_list.append(v);
            }
            Some(FidanValue::List(OwnedRef::new(new_list)))
        }
        _ => None,
    }
}
