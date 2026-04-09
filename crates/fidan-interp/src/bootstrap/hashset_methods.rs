use fidan_config::{ReceiverBuiltinKind, ReceiverMethodOp, infer_receiver_member};
use fidan_runtime::{FidanHashSet, FidanList, FidanValue, OwnedRef};

fn list_from_set(set: &FidanHashSet) -> FidanValue {
    let mut list = FidanList::new();
    for value in set.values_sorted() {
        list.append(value);
    }
    FidanValue::List(OwnedRef::new(list))
}

pub fn dispatch(
    set: OwnedRef<FidanHashSet>,
    method: &str,
    args: Vec<FidanValue>,
) -> Option<FidanValue> {
    let operation = infer_receiver_member(ReceiverBuiltinKind::HashSet, method)?.operation?;
    match operation {
        ReceiverMethodOp::Len => Some(FidanValue::Integer(set.borrow().len() as i64)),
        ReceiverMethodOp::IsEmpty => Some(FidanValue::Boolean(set.borrow().is_empty())),
        ReceiverMethodOp::Insert => {
            if let Some(value) = args.first() {
                let _ = set.borrow_mut().insert(value.clone());
            }
            Some(FidanValue::Nothing)
        }
        ReceiverMethodOp::Remove => {
            if let Some(value) = args.first() {
                let _ = set.borrow_mut().remove(value);
            }
            Some(FidanValue::Nothing)
        }
        ReceiverMethodOp::Contains => {
            if let Some(value) = args.first() {
                Some(FidanValue::Boolean(
                    set.borrow().contains(value).unwrap_or(false),
                ))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        ReceiverMethodOp::ToList => Some(list_from_set(&set.borrow())),
        ReceiverMethodOp::Union => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                Some(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().union(&other.borrow()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Intersect => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                Some(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().intersection(&other.borrow()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::Diff => {
            if let Some(FidanValue::HashSet(other)) = args.first() {
                Some(FidanValue::HashSet(OwnedRef::new(
                    set.borrow().difference(&other.borrow()),
                )))
            } else {
                Some(FidanValue::Nothing)
            }
        }
        ReceiverMethodOp::ToString => Some(FidanValue::String(fidan_runtime::FidanString::new(
            &fidan_runtime::display(&FidanValue::HashSet(set.clone())),
        ))),
        _ => None,
    }
}
