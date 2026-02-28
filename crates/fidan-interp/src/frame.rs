use fidan_runtime::FidanValue;

/// Non-local control-flow signals propagated up the call stack.
///
/// These are returned as `Err(Signal::…)` from `exec_stmt`/`eval_expr`.
/// The interpreter catches them at the appropriate boundary (loop, function,
/// try-block) and converts them back to a normal `Ok(value)` or re-propagates.
#[derive(Debug)]
pub enum Signal {
    /// `return expr` — carries the returned value to the call site.
    Return(FidanValue),

    /// `break` — terminates the nearest enclosing loop.
    Break,

    /// `continue` — skips to the next iteration of the nearest enclosing loop.
    Continue,

    /// `panic(expr)` / runtime error — carries the error value to the nearest
    /// `attempt / catch` handler, or terminates the program if uncaught.
    ///
    /// `trace` is the call stack at the moment the panic was raised,
    /// ordered **innermost frame first**.  Empty when the panic originates
    /// outside any named function (e.g. top-level code).
    Panic { value: FidanValue, trace: Vec<String> },
}

/// Convenience alias: `Ok(value)` for normal execution,
/// `Err(Signal)` for non-local jumps.
pub type InterpResult<T = FidanValue> = Result<T, Signal>;
