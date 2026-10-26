//! Shared contracts and fail-fast execution for the existing serial collection
//! callback helpers. Structured parallel task execution is owned separately.
use super::StdlibRuntimeError;
use crate::{FidanList, FidanValue, OwnedRef};
use fidan_diagnostics::diag_code;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallbackKind {
    Map,
    Filter,
    ForEach,
    Reduce,
}

#[derive(Debug)]
pub enum ParallelOp {
    Map {
        list: Vec<FidanValue>,
        callback: FidanValue,
    },
    Filter {
        list: Vec<FidanValue>,
        callback: FidanValue,
    },
    ForEach {
        list: Vec<FidanValue>,
        callback: FidanValue,
    },
    Reduce {
        list: Vec<FidanValue>,
        init: FidanValue,
        callback: FidanValue,
    },
}

pub fn dispatch_op(
    name: &str,
    args: Vec<FidanValue>,
) -> Option<Result<ParallelOp, StdlibRuntimeError>> {
    let kind = match name {
        "parallelMap" | "parallel_map" => CallbackKind::Map,
        "parallelFilter" | "parallel_filter" => CallbackKind::Filter,
        "parallelForEach" | "parallel_for_each" => CallbackKind::ForEach,
        "parallelReduce" | "parallel_reduce" => CallbackKind::Reduce,
        _ => return None,
    };
    Some((|| {
        let list = match args.first() {
            Some(FidanValue::List(list)) => list.borrow().iter().cloned().collect(),
            _ => {
                return Err(StdlibRuntimeError::new(
                    diag_code!("R0001"),
                    format!("{name} expects a list as its first argument"),
                ));
            }
        };
        let callback = args
            .get(if kind == CallbackKind::Reduce { 2 } else { 1 })
            .cloned()
            .unwrap_or(FidanValue::Nothing);
        if !matches!(
            &callback,
            FidanValue::Function(_) | FidanValue::Closure { .. } | FidanValue::StdlibFn(..)
        ) {
            return Err(StdlibRuntimeError::new(
                diag_code!("R0001"),
                format!(
                    "{name} expects an action callback, got {}",
                    callback.type_name()
                ),
            ));
        }
        Ok(match kind {
            CallbackKind::Map => ParallelOp::Map { list, callback },
            CallbackKind::Filter => ParallelOp::Filter { list, callback },
            CallbackKind::ForEach => ParallelOp::ForEach { list, callback },
            CallbackKind::Reduce => ParallelOp::Reduce {
                list,
                callback,
                init: args.get(1).cloned().unwrap_or(FidanValue::Nothing),
            },
        })
    })())
}

pub fn execute<E>(
    op: ParallelOp,
    mut call: impl FnMut(FidanValue, Vec<FidanValue>) -> Result<FidanValue, E>,
) -> Result<FidanValue, E> {
    match op {
        ParallelOp::Map { list, callback } => {
            let mut values = FidanList::new();
            for value in list {
                values.append(call(callback.clone(), vec![value])?);
            }
            Ok(FidanValue::List(OwnedRef::new(values)))
        }
        ParallelOp::Filter { list, callback } => {
            let mut values = FidanList::new();
            for value in list {
                if call(callback.clone(), vec![value.clone()])?.truthy() {
                    values.append(value);
                }
            }
            Ok(FidanValue::List(OwnedRef::new(values)))
        }
        ParallelOp::ForEach { list, callback } => {
            for value in list {
                call(callback.clone(), vec![value])?;
            }
            Ok(FidanValue::Nothing)
        }
        ParallelOp::Reduce {
            list,
            init,
            callback,
        } => {
            let mut value = init;
            for item in list {
                value = call(callback.clone(), vec![value, item])?;
            }
            Ok(value)
        }
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "parallelMap",
        "parallel_map",
        "parallelFilter",
        "parallel_filter",
        "parallelForEach",
        "parallel_for_each",
        "parallelReduce",
        "parallel_reduce",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callbacks_stop_at_the_first_original_error() {
        let mut calls = 0;
        let result = execute(
            ParallelOp::Map {
                list: vec![FidanValue::Integer(1), FidanValue::Integer(2)],
                callback: FidanValue::Nothing,
            },
            |_, _| {
                calls += 1;
                Err::<FidanValue, _>((diag_code!("R2001"), "original failure"))
            },
        );
        assert_eq!(
            result.unwrap_err(),
            (diag_code!("R2001"), "original failure")
        );
        assert_eq!(calls, 1);
    }
}
