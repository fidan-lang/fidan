//! `std.parallel` — Parallel and concurrent collection utilities for Fidan.
//!
//! Provides:
//!   `parallel.parallelMap(list, fn)` — parallel map over a list  
//!   `parallel.parallelFilter(list, fn)` — parallel filter
//!   `parallel.parallelForEach(list, fn)` — parallel for-each with side effects
//!   `parallel.parallelReduce(list, init, fn)` — parallel reduce (associative only)
//!
//! Callback functions are passed as `FidanValue::Function(FunctionId)` — these
//! are dispatched by the caller (MIR interpreter). The functions in this module
//! return a `ParallelOp` enum telling the interpreter what callback dispatching to perform.

use fidan_runtime::{FidanValue, FunctionId};

/// A higher-order parallel operation needing MIR-level callback dispatch.
#[derive(Debug)]
pub enum ParallelOp {
    /// `parallelMap(list, fn)` — apply fn to each element in parallel, collect results in order.
    Map {
        list: Vec<FidanValue>,
        fn_id: FunctionId,
    },
    /// `parallelFilter(list, fn)` — keep elements where fn(elem) is truthy.
    Filter {
        list: Vec<FidanValue>,
        fn_id: FunctionId,
    },
    /// `parallelForEach(list, fn)` — call fn(elem) for each element; results ignored.
    ForEach {
        list: Vec<FidanValue>,
        fn_id: FunctionId,
    },
    /// `parallelReduce(list, init, fn)` — fold with fn(acc, elem); uses tree reduction.
    Reduce {
        list: Vec<FidanValue>,
        init: FidanValue,
        fn_id: FunctionId,
    },
}

/// Dispatch a `parallel.<name>(args)` call.
/// Returns `None` for unknown functions.
/// Returns `Some(Ok(Some(ParallelOp)))` for callback-based operations that the MIR
/// interpreter must complete.
pub fn dispatch_op(
    name: &str,
    args: Vec<FidanValue>,
) -> Option<Result<Option<ParallelOp>, String>> {
    match name {
        "parallelMap" | "parallel_map" => {
            let list = extract_list(&args, 0)?;
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
            match fn_val {
                FidanValue::Function(fid) => Some(Ok(Some(ParallelOp::Map { list, fn_id: fid }))),
                _ => Some(Err(
                    "parallelMap requires a function as the second argument".into(),
                )),
            }
        }
        "parallelFilter" | "parallel_filter" => {
            let list = extract_list(&args, 0)?;
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
            match fn_val {
                FidanValue::Function(fid) => {
                    Some(Ok(Some(ParallelOp::Filter { list, fn_id: fid })))
                }
                _ => Some(Err(
                    "parallelFilter requires a function as the second argument".into(),
                )),
            }
        }
        "parallelForEach" | "parallel_for_each" => {
            let list = extract_list(&args, 0)?;
            let fn_val = args.into_iter().nth(1).unwrap_or(FidanValue::Nothing);
            match fn_val {
                FidanValue::Function(fid) => {
                    Some(Ok(Some(ParallelOp::ForEach { list, fn_id: fid })))
                }
                _ => Some(Err(
                    "parallelForEach requires a function as the second argument".into(),
                )),
            }
        }
        "parallelReduce" | "parallel_reduce" => {
            let list = extract_list(&args, 0)?;
            let init = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            let fn_val = args.into_iter().nth(2).unwrap_or(FidanValue::Nothing);
            match fn_val {
                FidanValue::Function(fid) => Some(Ok(Some(ParallelOp::Reduce {
                    list,
                    init,
                    fn_id: fid,
                }))),
                _ => Some(Err(
                    "parallelReduce requires a function as the third argument".into(),
                )),
            }
        }
        _ => None,
    }
}

fn extract_list(args: &[FidanValue], idx: usize) -> Option<Vec<FidanValue>> {
    match args.get(idx) {
        Some(FidanValue::List(l)) => Some(l.borrow().iter().cloned().collect()),
        _ => None,
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
