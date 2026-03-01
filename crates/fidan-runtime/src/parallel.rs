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
        self.0.into_iter().map(ParallelCapture::into_inner).collect()
    }
}

// ── FidanPending ───────────────────────────────────────────────────────────────

/// A value that is being computed on a background thread (`spawn` expression).
///
/// Cheap to clone — all clones share the same `Arc<Mutex<…>>` slot.
/// The first call to [`FidanPending::join`] consumes the `JoinHandle`; all
/// subsequent calls return `FidanValue::Nothing`.
#[derive(Clone)]
pub struct FidanPending(
    pub Arc<Mutex<Option<std::thread::JoinHandle<ParallelCapture>>>>,
);

impl FidanPending {
    /// Spawn a closure on a new OS thread and wrap the result in a `FidanPending`.
    ///
    /// `f` receives a [`ParallelArgs`] bundle and returns a [`FidanValue`].
    /// Build `args` via `ParallelArgs::from_captures(…)` before calling this.
    pub fn spawn_with_args<F>(args: ParallelArgs, f: F) -> Self
    where
        F: FnOnce(ParallelArgs) -> FidanValue + Send + 'static,
    {
        let handle = std::thread::spawn(move || ParallelCapture(f(args)));
        FidanPending(Arc::new(Mutex::new(Some(handle))))
    }

    /// Block the calling thread until the spawned computation finishes.
    /// Returns `FidanValue::Nothing` if the handle was already consumed or
    /// if the spawned thread panicked.
    pub fn join(&self) -> FidanValue {
        let maybe = {
            let mut guard = self.0.lock().unwrap();
            guard.take()
        };
        match maybe {
            Some(h) => h.join().map(ParallelCapture::into_inner).unwrap_or(FidanValue::Nothing),
            None => FidanValue::Nothing,
        }
    }
}

impl std::fmt::Debug for FidanPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<pending>")
    }
}
