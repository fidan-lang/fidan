//! Bootstrap dict methods — placeholder until `std.collections` (Phase 7).

use fidan_config::{ReceiverBuiltinKind, ReceiverMethodOp, infer_receiver_member};
use fidan_runtime::{FidanDict, FidanList, FidanValue, OwnedRef};

pub fn dispatch(d: OwnedRef<FidanDict>, method: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    let operation = infer_receiver_member(ReceiverBuiltinKind::Dict, method)?.operation?;
    match operation {
        ReceiverMethodOp::IsEmpty => Some(FidanValue::Boolean(d.borrow().is_empty())),
        ReceiverMethodOp::Get => {
            if let Some(key) = args.first() {
                Some(
                    d.borrow()
                        .get(key)
                        .ok()
                        .flatten()
                        .cloned()
                        .unwrap_or(FidanValue::Nothing),
                )
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Set => {
            if let (Some(k), Some(v)) = (args.first(), args.get(1)) {
                let _ = d.borrow_mut().insert(k.clone(), v.clone());
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Len => Some(FidanValue::Integer(d.borrow().len() as i64)),
        ReceiverMethodOp::Keys => {
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (k, _) in borrow.iter() {
                list.append(k.clone());
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Values => {
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (_, v) in borrow.iter() {
                list.append(v.clone());
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Entries => {
            let borrow = d.borrow();
            let mut list = FidanList::with_capacity(borrow.len());
            for (k, v) in borrow.iter() {
                let mut pair = FidanList::with_capacity(2);
                pair.append(k.clone());
                pair.append(v.clone());
                list.append(FidanValue::List(OwnedRef::new(pair)));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        ReceiverMethodOp::Contains => {
            if let Some(key) = args.first() {
                Some(FidanValue::Boolean(
                    d.borrow().get(key).ok().flatten().is_some(),
                ))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        ReceiverMethodOp::Remove => {
            if let Some(key) = args.first() {
                let _ = d.borrow_mut().remove(key);
                Some(FidanValue::Nothing)
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::ToString => Some(FidanValue::String(fidan_runtime::FidanString::new(
            &fidan_runtime::display(&FidanValue::Dict(d.clone())),
        ))),
        _ => None,
    }
}
