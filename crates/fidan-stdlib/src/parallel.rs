//! Existing collection callback operations share runtime-owned validation and
//! execution. Callable invocation stays with the interpreter/native dispatcher.
pub use fidan_runtime::stdlib::parallel::{ParallelOp, dispatch_op, exported_names};
