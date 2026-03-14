//! Bootstrap dict methods — placeholder until `std.collections` (Phase 7).

use fidan_runtime::{FidanDict, FidanList, FidanValue, OwnedRef};

pub fn dispatch(d: OwnedRef<FidanDict>, method: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match method {
        "get" => {
            if let Some(FidanValue::String(k)) = args.first() {
                Some(d.borrow().get(k).cloned().unwrap_or(FidanValue::Nothing))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "set" | "insert" => {
            if let (Some(FidanValue::String(k)), Some(v)) = (args.first(), args.get(1)) {
                d.borrow_mut().insert(k.clone(), v.clone());
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        "len" | "length" => Some(FidanValue::Integer(d.borrow().len() as i64)),
        "keys" => {
            let mut list = FidanList::new();
            for (k, _) in d.borrow().iter() {
                list.append(FidanValue::String(k.clone()));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "values" => {
            let mut list = FidanList::new();
            for (_, v) in d.borrow().iter() {
                list.append(v.clone());
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "contains" | "has_key" => {
            if let Some(FidanValue::String(k)) = args.first() {
                Some(FidanValue::Boolean(d.borrow().get(k).is_some()))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        _ => None,
    }
}
