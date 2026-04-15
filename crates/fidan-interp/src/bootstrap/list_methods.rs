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
        "pop" => Some(r.borrow_mut().pop().unwrap_or(FidanValue::Nothing)),
        "remove" => {
            let idx = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            if let FidanValue::Integer(i) = idx {
                Some(
                    r.borrow_mut()
                        .remove(i as usize)
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "reverse" => {
            r.borrow_mut().reverse();
            Some(FidanValue::Nothing)
        }
        "sort" => {
            r.borrow_mut().sort_by(|a, b| match (a, b) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => x.cmp(y),
                (FidanValue::Float(x), FidanValue::Float(y)) => {
                    x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                }
                (FidanValue::String(x), FidanValue::String(y)) => x.as_str().cmp(y.as_str()),
                _ => std::cmp::Ordering::Equal,
            });
            Some(FidanValue::Nothing)
        }
        "join" => {
            let sep = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let sep_str = match sep {
                FidanValue::String(s) => s.as_str().to_string(),
                _ => String::new(),
            };
            let borrow = r.borrow();
            let mut joined = String::with_capacity(sep_str.len().saturating_mul(borrow.len()));
            for (index, value) in borrow.iter().enumerate() {
                if index > 0 {
                    joined.push_str(&sep_str);
                }
                match value {
                    FidanValue::String(s) => joined.push_str(s.as_str()),
                    FidanValue::Integer(n) => joined.push_str(&n.to_string()),
                    FidanValue::Float(f) => joined.push_str(&f.to_string()),
                    FidanValue::Boolean(b) => joined.push_str(&b.to_string()),
                    FidanValue::Nothing => joined.push_str("nothing"),
                    _ => {}
                }
            }
            Some(FidanValue::String(FidanString::new(&joined)))
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
            let mut new_list = FidanList::with_capacity(items.len());
            for v in items.into_iter().rev() {
                new_list.append(v);
            }
            Some(FidanValue::List(OwnedRef::new(new_list)))
        }
        _ => None,
    }
}
