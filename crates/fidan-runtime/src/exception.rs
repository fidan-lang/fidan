//! Native exception provenance. Catches receive the original value; only an
//! uncaught boundary renders a diagnostic. Diagnostic identity never comes from
//! parsing user-controlled exception text.
use crate::{FidanValue, parallel::ParallelCapture};
use fidan_diagnostics::{DiagCode, diag_code};

#[derive(Clone, Debug)]
pub enum ExceptionOrigin {
    UserPanic,
    Runtime { code: DiagCode, message: String },
    Internal,
}

#[derive(Debug)]
pub struct RuntimeException {
    pub value: FidanValue,
    pub origin: ExceptionOrigin,
}

impl RuntimeException {
    pub fn internal(value: FidanValue) -> Self {
        Self {
            value,
            origin: ExceptionOrigin::Internal,
        }
    }

    pub fn runtime(code: DiagCode, message: String) -> Self {
        Self {
            value: FidanValue::String(crate::FidanString::new(&format!(
                "error [{code}]: {message}"
            ))),
            origin: ExceptionOrigin::Runtime { code, message },
        }
    }

    pub fn is_catchable(&self) -> bool {
        match &self.origin {
            ExceptionOrigin::Internal => false,
            ExceptionOrigin::Runtime { code, .. } => *code != diag_code!("R9001"),
            ExceptionOrigin::UserPanic => true,
        }
    }

    pub fn diagnostic(&self) -> (DiagCode, String) {
        match &self.origin {
            ExceptionOrigin::UserPanic => (
                diag_code!("R1002"),
                format!(
                    "unhandled exception: {}",
                    crate::display_exception(&self.value, None)
                ),
            ),
            ExceptionOrigin::Runtime { code, message } => (*code, message.clone()),
            ExceptionOrigin::Internal => (
                diag_code!("R0001"),
                crate::display_exception(&self.value, None),
            ),
        }
    }

    pub fn capture(self) -> CapturedException {
        CapturedException {
            message: crate::display_exception(&self.value, None),
            value: ParallelCapture::new(&self.value),
            origin: self.origin,
        }
    }
}

/// Uses the existing graph snapshot boundary; mutable Rc values never alias
/// across threads. Error origin travels with the payload, including through await.
pub struct CapturedException {
    message: String,
    value: ParallelCapture,
    origin: ExceptionOrigin,
}

impl CapturedException {
    pub(crate) fn with_context(mut self, context: &str) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    pub fn restore(self) -> RuntimeException {
        RuntimeException {
            value: self.value.into_inner(),
            origin: self.origin,
        }
    }
}

impl From<String> for CapturedException {
    fn from(message: String) -> Self {
        RuntimeException::internal(FidanValue::String(crate::FidanString::new(&message))).capture()
    }
}

impl std::fmt::Display for CapturedException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep the existing String-based pending API's message contract.
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_identity_is_provenance_not_exception_text() {
        let value = FidanValue::String(crate::FidanString::new("error [R2001]: user text"));
        let panic = RuntimeException {
            value: value.clone(),
            origin: ExceptionOrigin::UserPanic,
        };
        assert_eq!(panic.diagnostic().0, diag_code!("R1002"));
        assert_eq!(
            RuntimeException::internal(value).diagnostic().0,
            diag_code!("R0001")
        );
        let arithmetic = RuntimeException::runtime(diag_code!("R2001"), "zero divisor".into());
        assert_eq!(
            arithmetic.diagnostic(),
            (diag_code!("R2001"), "zero divisor".into())
        );
    }

    #[test]
    fn captured_panics_keep_their_original_value_and_origin() {
        let panic = RuntimeException {
            value: FidanValue::Integer(42),
            origin: ExceptionOrigin::UserPanic,
        };
        let captured = panic.capture();
        let restored = std::thread::spawn(move || captured.restore().capture())
            .join()
            .unwrap()
            .restore();
        assert!(matches!(restored.value, FidanValue::Integer(42)));
        assert_eq!(restored.diagnostic().0, diag_code!("R1002"));
    }
}

/// A deterministic per-thread language call budget, independent of host stack
/// size. Foreign code and host crashes are outside this diagnostic contract.
pub const MAX_CALL_DEPTH: usize = 1024;
// The interpreter has substantially larger host frames than generated native
// code. Grow its stack at the existing call boundary before the OS guard page.
pub const NATIVE_STACK_RESERVE: usize = 128 * 1024;
pub const INTERPRETER_STACK_RED_ZONE: usize = 256 * 1024;
pub const INTERPRETER_STACK_SEGMENT: usize = 2 * 1024 * 1024;
thread_local! { static CALL_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub fn enter_call() -> Result<(), crate::stdlib::StdlibRuntimeError> {
    CALL_DEPTH.with(|depth| {
        if stacker::remaining_stack().is_some_and(|bytes| bytes < NATIVE_STACK_RESERVE) {
            return Err(crate::stdlib::StdlibRuntimeError::new(
                diag_code!("R1001"),
                "Fidan call stack reserve exhausted",
            ));
        }
        if depth.get() >= MAX_CALL_DEPTH {
            return Err(crate::stdlib::StdlibRuntimeError::new(
                diag_code!("R1001"),
                format!("maximum Fidan call depth ({MAX_CALL_DEPTH}) exceeded"),
            ));
        }
        depth.set(depth.get() + 1);
        Ok(())
    })
}

pub fn leave_call() {
    CALL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
}

/// Interpreter calls, including selective JIT calls through its existing bridge.
pub struct CallDepthGuard;
impl CallDepthGuard {
    pub fn enter() -> Result<Self, crate::stdlib::StdlibRuntimeError> {
        enter_call()?;
        Ok(Self)
    }
}
impl Drop for CallDepthGuard {
    fn drop(&mut self) {
        leave_call();
    }
}
