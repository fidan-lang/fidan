use crate::{FidanList, FidanValue, OwnedRef};

#[derive(Debug, Clone)]
pub enum AsyncOp {
    Sleep { ms: u64 },
    Ready { value: FidanValue },
    Gather { values: Vec<FidanValue> },
    WaitAny { values: Vec<FidanValue> },
    Timeout { handle: FidanValue, ms: u64 },
}

#[derive(Debug, Clone)]
pub enum AsyncDispatch {
    Value(FidanValue),
    Op(AsyncOp),
}

fn to_ms(value: Option<&FidanValue>) -> u64 {
    match value {
        Some(FidanValue::Integer(n)) => (*n).max(0) as u64,
        Some(FidanValue::Float(f)) => f.max(0.0) as u64,
        _ => 0,
    }
}

fn list_values(arg: Option<&FidanValue>) -> Vec<FidanValue> {
    match arg {
        Some(FidanValue::List(list)) => list.borrow().iter().cloned().collect(),
        _ => Vec::new(),
    }
}

pub fn wait_any_result(index: i64, value: FidanValue) -> FidanValue {
    let mut list = FidanList::new();
    list.append(FidanValue::Integer(index));
    list.append(value);
    FidanValue::List(OwnedRef::new(list))
}

pub fn timeout_result(completed: bool, value: FidanValue) -> FidanValue {
    let mut list = FidanList::new();
    list.append(FidanValue::Boolean(completed));
    list.append(value);
    FidanValue::List(OwnedRef::new(list))
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<AsyncDispatch> {
    match name {
        "sleep" | "wait" => Some(AsyncDispatch::Op(AsyncOp::Sleep {
            ms: to_ms(args.first()),
        })),
        "ready" => Some(AsyncDispatch::Op(AsyncOp::Ready {
            value: args.first().cloned().unwrap_or(FidanValue::Nothing),
        })),
        "gather" | "waitAll" | "wait_all" => Some(AsyncDispatch::Op(AsyncOp::Gather {
            values: list_values(args.first()),
        })),
        "waitAny" | "wait_any" => Some(AsyncDispatch::Op(AsyncOp::WaitAny {
            values: list_values(args.first()),
        })),
        "timeout" => Some(AsyncDispatch::Op(AsyncOp::Timeout {
            handle: args.first().cloned().unwrap_or(FidanValue::Nothing),
            ms: to_ms(args.get(1)),
        })),
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "sleep", "wait", "ready", "gather", "waitAll", "wait_all", "waitAny", "wait_any", "timeout",
    ]
}
