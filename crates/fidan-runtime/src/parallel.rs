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

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::FidanValue;

type PendingJoinHandle = std::thread::JoinHandle<Result<ParallelCapture, String>>;
type DeferredPendingThunk = Box<dyn FnOnce() -> Result<ParallelCapture, String> + Send + 'static>;

struct DeferredScheduler {
    next_id: u64,
    queue: VecDeque<u64>,
    entries: HashMap<u64, DeferredSchedulerEntry>,
}

enum DeferredSchedulerEntry {
    Queued(DeferredPendingThunk),
    Sleeping(Instant),
    Running,
    Ready(Result<ParallelCapture, String>),
}

#[derive(Clone, Copy)]
enum DeferredSchedulerState {
    Queued,
    Sleeping(Instant),
    Running,
    Ready,
    Missing,
}

impl Default for DeferredScheduler {
    fn default() -> Self {
        Self {
            next_id: 1,
            queue: VecDeque::new(),
            entries: HashMap::new(),
        }
    }
}

impl DeferredScheduler {
    fn push(&mut self, thunk: DeferredPendingThunk) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries
            .insert(id, DeferredSchedulerEntry::Queued(thunk));
        self.queue.push_back(id);
        id
    }

    fn push_sleep(&mut self, deadline: Instant) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries
            .insert(id, DeferredSchedulerEntry::Sleeping(deadline));
        id
    }
}

thread_local! {
    static DEFERRED_SCHEDULER: RefCell<DeferredScheduler> =
        RefCell::new(DeferredScheduler::default());
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        std::thread::sleep(deadline.duration_since(now));
    }
}

fn deferred_scheduler_wake_sleepers() {
    DEFERRED_SCHEDULER.with(|scheduler| {
        let mut scheduler = scheduler.borrow_mut();
        let now = Instant::now();
        for entry in scheduler.entries.values_mut() {
            if let DeferredSchedulerEntry::Sleeping(deadline) = entry
                && *deadline <= now
            {
                *entry = DeferredSchedulerEntry::Ready(Ok(ParallelCapture(FidanValue::Nothing)));
            }
        }
    });
}

fn deferred_scheduler_next_deadline() -> Option<Instant> {
    DEFERRED_SCHEDULER.with(|scheduler| {
        scheduler
            .borrow()
            .entries
            .values()
            .filter_map(|entry| match entry {
                DeferredSchedulerEntry::Sleeping(deadline) => Some(*deadline),
                _ => None,
            })
            .min()
    })
}

fn deferred_scheduler_state(id: u64) -> DeferredSchedulerState {
    deferred_scheduler_wake_sleepers();
    DEFERRED_SCHEDULER.with(|scheduler| {
        let scheduler = scheduler.borrow();
        match scheduler.entries.get(&id) {
            Some(DeferredSchedulerEntry::Queued(_)) => DeferredSchedulerState::Queued,
            Some(DeferredSchedulerEntry::Sleeping(deadline)) => {
                DeferredSchedulerState::Sleeping(*deadline)
            }
            Some(DeferredSchedulerEntry::Running) => DeferredSchedulerState::Running,
            Some(DeferredSchedulerEntry::Ready(_)) => DeferredSchedulerState::Ready,
            None => DeferredSchedulerState::Missing,
        }
    })
}

fn deferred_scheduler_take_ready(id: u64) -> Result<ParallelCapture, String> {
    deferred_scheduler_wake_sleepers();
    DEFERRED_SCHEDULER.with(|scheduler| {
        let mut scheduler = scheduler.borrow_mut();
        let Some(DeferredSchedulerEntry::Ready(result)) = scheduler.entries.remove(&id) else {
            unreachable!("pending task state changed while taking ready result");
        };
        result
    })
}

fn deferred_scheduler_try_take_ready(id: u64) -> Option<Result<ParallelCapture, String>> {
    deferred_scheduler_wake_sleepers();
    DEFERRED_SCHEDULER.with(|scheduler| {
        let mut scheduler = scheduler.borrow_mut();
        match scheduler.entries.remove(&id) {
            Some(DeferredSchedulerEntry::Ready(result)) => Some(result),
            Some(entry) => {
                scheduler.entries.insert(id, entry);
                None
            }
            None => Some(Ok(ParallelCapture(FidanValue::Nothing))),
        }
    })
}

fn deferred_scheduler_pop_next_queued() -> Option<(u64, DeferredPendingThunk)> {
    DEFERRED_SCHEDULER.with(|scheduler| {
        let mut scheduler = scheduler.borrow_mut();
        while let Some(id) = scheduler.queue.pop_front() {
            let Some(entry) = scheduler.entries.remove(&id) else {
                continue;
            };
            match entry {
                DeferredSchedulerEntry::Queued(thunk) => {
                    scheduler
                        .entries
                        .insert(id, DeferredSchedulerEntry::Running);
                    return Some((id, thunk));
                }
                DeferredSchedulerEntry::Sleeping(deadline) => {
                    scheduler
                        .entries
                        .insert(id, DeferredSchedulerEntry::Sleeping(deadline));
                }
                DeferredSchedulerEntry::Running => {
                    scheduler
                        .entries
                        .insert(id, DeferredSchedulerEntry::Running);
                }
                DeferredSchedulerEntry::Ready(result) => {
                    scheduler
                        .entries
                        .insert(id, DeferredSchedulerEntry::Ready(result));
                }
            }
        }
        None
    })
}

fn deferred_scheduler_store_ready(id: u64, result: Result<ParallelCapture, String>) {
    DEFERRED_SCHEDULER.with(|scheduler| {
        scheduler
            .borrow_mut()
            .entries
            .insert(id, DeferredSchedulerEntry::Ready(result));
    });
}

fn deferred_scheduler_run_next() -> bool {
    deferred_scheduler_wake_sleepers();
    let Some((id, thunk)) = deferred_scheduler_pop_next_queued() else {
        return false;
    };
    deferred_scheduler_store_ready(id, thunk());
    true
}

fn deferred_scheduler_resolve(id: u64) -> Result<ParallelCapture, String> {
    loop {
        match deferred_scheduler_state(id) {
            DeferredSchedulerState::Queued | DeferredSchedulerState::Running => {
                if !deferred_scheduler_run_next() {
                    if let Some(deadline) = deferred_scheduler_next_deadline() {
                        sleep_until(deadline);
                        continue;
                    }
                    return Err(
                        "same-thread task scheduler deadlocked while awaiting a running task"
                            .to_string(),
                    );
                }
            }
            DeferredSchedulerState::Sleeping(deadline) => {
                if deferred_scheduler_run_next() {
                    continue;
                }
                sleep_until(deadline);
            }
            DeferredSchedulerState::Ready => return deferred_scheduler_take_ready(id),
            DeferredSchedulerState::Missing => return Ok(ParallelCapture(FidanValue::Nothing)),
        }
    }
}

enum PendingStateEntry {
    Thread(PendingJoinHandle),
    Deferred(u64),
    Ready(Result<ParallelCapture, String>),
}

type PendingState = Arc<Mutex<Option<PendingStateEntry>>>;

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

/// A value that is being computed asynchronously.
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
pub struct FidanPending(PendingState);

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
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Thread(
            handle,
        )))))
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
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Thread(
            handle,
        )))))
    }

    /// Schedule a closure to run lazily on the calling thread the first time the
    /// handle is awaited or joined.
    pub fn defer_with_args<F>(args: ParallelArgs, f: F) -> Self
    where
        F: FnOnce(ParallelArgs) -> FidanValue + Send + 'static,
    {
        let thunk: DeferredPendingThunk = Box::new(move || Ok(ParallelCapture(f(args))));
        let id = DEFERRED_SCHEDULER.with(|scheduler| scheduler.borrow_mut().push(thunk));
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Deferred(id)))))
    }

    /// Schedule a fallible closure to run lazily on the calling thread the first
    /// time the handle is awaited or joined.
    pub fn defer_fallible<F>(args: ParallelArgs, f: F) -> Self
    where
        F: FnOnce(ParallelArgs) -> Result<FidanValue, String> + Send + 'static,
    {
        let thunk: DeferredPendingThunk = Box::new(move || f(args).map(ParallelCapture));
        let id = DEFERRED_SCHEDULER.with(|scheduler| scheduler.borrow_mut().push(thunk));
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Deferred(id)))))
    }

    /// Create a same-thread timer pending handle that resolves to `nothing`
    /// after the requested duration.
    pub fn sleep(duration_ms: u64) -> Self {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(duration_ms))
            .unwrap_or_else(Instant::now);
        let id = DEFERRED_SCHEDULER.with(|scheduler| scheduler.borrow_mut().push_sleep(deadline));
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Deferred(id)))))
    }

    /// Create a pending handle that is already resolved on the current thread.
    ///
    /// This is used for same-thread structured `concurrent` tasks in AOT mode:
    /// callers still observe a `Pending`, but no OS thread is involved.
    pub fn ready_result(result: Result<FidanValue, String>) -> Self {
        FidanPending(Arc::new(Mutex::new(Some(PendingStateEntry::Ready(
            result.map(ParallelCapture),
        )))))
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
            Some(PendingStateEntry::Thread(h)) => match h.join() {
                Ok(Ok(cap)) => Ok(cap.into_inner()),
                Ok(Err(e)) => Err(e),
                // The Rust thread itself panicked (shouldn't happen in normal
                // operation, but handle defensively).
                Err(_) => Err("task thread panicked unexpectedly".to_string()),
            },
            Some(PendingStateEntry::Deferred(id)) => {
                deferred_scheduler_resolve(id).map(ParallelCapture::into_inner)
            }
            Some(PendingStateEntry::Ready(result)) => result.map(ParallelCapture::into_inner),
            None => Ok(FidanValue::Nothing),
        }
    }

    /// Try to consume the handle only if it is already ready.
    ///
    /// Returns `None` when the handle is still pending.
    pub fn try_take_ready(&self) -> Option<Result<FidanValue, String>> {
        let mut guard = self.0.lock().unwrap();
        match guard.as_ref() {
            Some(PendingStateEntry::Thread(handle)) if handle.is_finished() => {
                let entry = guard.take();
                drop(guard);
                match entry {
                    Some(PendingStateEntry::Thread(handle)) => Some(match handle.join() {
                        Ok(Ok(cap)) => Ok(cap.into_inner()),
                        Ok(Err(err)) => Err(err),
                        Err(_) => Err("task thread panicked unexpectedly".to_string()),
                    }),
                    _ => unreachable!("pending state changed while taking ready thread result"),
                }
            }
            Some(PendingStateEntry::Deferred(id)) => {
                let id = *id;
                match deferred_scheduler_try_take_ready(id) {
                    Some(result) => {
                        guard.take();
                        Some(result.map(ParallelCapture::into_inner))
                    }
                    None => None,
                }
            }
            Some(PendingStateEntry::Ready(_)) => {
                let entry = guard.take();
                drop(guard);
                match entry {
                    Some(PendingStateEntry::Ready(result)) => {
                        Some(result.map(ParallelCapture::into_inner))
                    }
                    _ => unreachable!("pending state changed while taking ready result"),
                }
            }
            Some(PendingStateEntry::Thread(_)) | None => None,
        }
    }

    /// Block the calling thread until the spawned computation finishes.
    /// Returns `FidanValue::Nothing` if the handle was already consumed or
    /// if the spawned thread errored (use [`Self::try_join`] to observe errors).
    pub fn join(&self) -> FidanValue {
        self.try_join().unwrap_or(FidanValue::Nothing)
    }

    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

impl std::fmt::Debug for FidanPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<pending>")
    }
}
