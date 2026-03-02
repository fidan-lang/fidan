// fidan-runtime/src/parallel.rs
//
// Phase 8: parallel/concurrent value capture helpers.
//
// # Safety model
// `FidanValue` is not `Send` because `OwnedRef<T>` is `Rc<RefCell<T>>`.
// For parallel execution we create *fresh* `Rc<RefCell<T>>` wrappers around
// the CoW `Arc<…>` inner data.  No `Rc` is *shared* across threads; only the
// underlying `Arc` data is shared (and it is genuinely read-safe until
// mutation triggers a CoW copy).  The `unsafe impl Send` on `ParallelCapture`
// and `ParallelArgs` is sound under this invariant.
//
// # Rust 2021 partial-capture pitfall
// In Rust 2021 edition, a `move` closure that accesses a *field* of a moved
// value (e.g. `item_cap.0`) will capture the **field** type, not the wrapper
// type.  This bypasses the `unsafe impl Send` on the wrapper.  All closure
// bodies must therefore consume captured wrappers via **method calls**
// (`into_inner()`, `into_vec()`) rather than direct field accesses (`.0`).

use std::sync::{Arc, Mutex};

use crate::FidanValue;

// ── ParallelCapture ────────────────────────────────────────────────────────────

/// Thread-safe wrapper for a single [`FidanValue`] being sent into a parallel task.
///
/// ## Safety invariant
/// The wrapped value **must be exclusively owned** by exactly one thread at a
/// time.  Always create via [`FidanValue::parallel_capture`], which produces
/// fresh `Rc<RefCell<T>>` wrappers (one per thread) around the shared CoW
/// `Arc<…>` data.
pub struct ParallelCapture(pub FidanValue);

// SAFETY: upheld by the invariant above.
unsafe impl Send for ParallelCapture {}

impl ParallelCapture {
    /// Consume the wrapper and return the inner value.
    ///
    /// **Must be called as a method** inside spawned closures — never access
    /// `.0` directly there, to prevent Rust 2021 partial-capture from
    /// extracting the `!Send` `FidanValue` as the captured type.
    #[inline]
    pub fn into_inner(self) -> FidanValue {
        self.0
    }
}

// ── ParallelArgs ───────────────────────────────────────────────────────────────

/// Thread-safe bundle of [`ParallelCapture`] values for a single parallel task.
///
/// Wrapping the argument vec prevents Rust 2021 partial-capture from
/// extracting individual `FidanValue` fields inside spawned closures.
pub struct ParallelArgs(pub Vec<ParallelCapture>);

// SAFETY: each ParallelCapture is exclusively owned by one thread.
unsafe impl Send for ParallelArgs {}

impl ParallelArgs {
    /// Build a `ParallelArgs` from an iterator of captured values.
    pub fn from_captures(caps: impl IntoIterator<Item = ParallelCapture>) -> Self {
        ParallelArgs(caps.into_iter().collect())
    }

    /// Consume the bundle and return the raw `Vec<FidanValue>`.
    ///
    /// **Call as a method** inside spawned closures (see `ParallelCapture`).
    #[inline]
    pub fn into_vec(self) -> Vec<FidanValue> {
        self.0
            .into_iter()
            .map(ParallelCapture::into_inner)
            .collect()
    }
}

// ── FidanPending ───────────────────────────────────────────────────────────────

/// A value that is being computed on a background thread (`spawn` expression).
///
/// Cheap to clone — all clones share the same `Arc<Mutex<…>>` slot.
/// The first call to [`FidanPending::join`] consumes the `JoinHandle`; all
/// subsequent calls return `FidanValue::Nothing`.
///
/// The inner `JoinHandle` carries `Result<ParallelCapture, String>` so that
/// task failures (Fidan panics / throws) can be propagated to the join site
/// instead of being silently swallowed.  Use [`Self::try_join`] to observe
/// the error; [`Self::join`] is the backwards-compatible convenience that
/// maps errors to `FidanValue::Nothing`.
#[derive(Clone)]
pub struct FidanPending(
    pub Arc<Mutex<Option<std::thread::JoinHandle<Result<ParallelCapture, String>>>>>,
);

impl FidanPending {
    /// Spawn a closure on a new OS thread and wrap the result in a `FidanPending`.
    ///
    /// The closure returns a plain `FidanValue`; errors are not observable via
    /// this constructor — use [`Self::spawn_fallible`] when you need to catch
    /// Fidan panics / throws from a parallel task.
    pub fn spawn_with_args<F>(args: ParallelArgs, f: F) -> Self
    where
        F: FnOnce(ParallelArgs) -> FidanValue + Send + 'static,
    {
        let handle = std::thread::spawn(move || Ok::<_, String>(ParallelCapture(f(args))));
        FidanPending(Arc::new(Mutex::new(Some(handle))))
    }

    /// Spawn a fallible closure on a new OS thread.
    ///
    /// The closure returns `Result<FidanValue, String>`.  An `Err(msg)` is
    /// stored in the handle and re-surfaced by [`Self::try_join`].  Use this
    /// for `SpawnParallel` / `SpawnConcurrent` tasks where a Fidan panic
    /// inside the task must be reported to the outer `JoinAll` group.
    pub fn spawn_fallible<F>(args: ParallelArgs, f: F) -> Self
    where
        F: FnOnce(ParallelArgs) -> Result<FidanValue, String> + Send + 'static,
    {
        let handle = std::thread::spawn(move || f(args).map(ParallelCapture));
        FidanPending(Arc::new(Mutex::new(Some(handle))))
    }

    /// Block the calling thread until the spawned computation finishes.
    ///
    /// Returns `Ok(value)` on success or `Err(message)` if the task
    /// panicked / threw an uncaught error.  Returns `Ok(Nothing)` if the
    /// handle was already consumed (idempotent after the first call).
    pub fn try_join(&self) -> Result<FidanValue, String> {
        let maybe = {
            let mut guard = self.0.lock().unwrap();
            guard.take()
        };
        match maybe {
            Some(h) => match h.join() {
                Ok(Ok(cap)) => Ok(cap.into_inner()),
                Ok(Err(e)) => Err(e),
                // The Rust thread itself panicked (shouldn't happen in normal
                // operation, but handle defensively).
                Err(_) => Err("task thread panicked unexpectedly".to_string()),
            },
            None => Ok(FidanValue::Nothing),
        }
    }

    /// Block the calling thread until the spawned computation finishes.
    /// Returns `FidanValue::Nothing` if the handle was already consumed or
    /// if the spawned thread errored (use [`Self::try_join`] to observe errors).
    pub fn join(&self) -> FidanValue {
        self.try_join().unwrap_or(FidanValue::Nothing)
    }
}

impl std::fmt::Debug for FidanPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<pending>")
    }
}
