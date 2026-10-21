use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

thread_local! {
    static HELD_SHARED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Exclusive Shared access. Tracks ownership on this thread to reject recursive
/// access from callbacks instead of blocking forever on a non-reentrant mutex.
pub struct SharedGuard<'a, T> {
    value: MutexGuard<'a, SharedCell<T>>,
    identity: usize,
}

impl<T> Deref for SharedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value.0
    }
}

impl<T> DerefMut for SharedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value.0
    }
}

impl<T> Drop for SharedGuard<'_, T> {
    fn drop(&mut self) {
        HELD_SHARED.with(|held| held.borrow_mut().retain(|id| *id != self.identity));
    }
}

/// ARC reference — only for `Shared oftype T` values.
/// Uses Arc<Mutex<T>> for safe cross-thread shared mutation.
#[derive(Debug, Clone)]
pub struct SharedRef<T>(Arc<Mutex<SharedCell<T>>>);

/// Private payload: construction either requires T: Send, or snapshots a Fidan
/// value graph. The latter's ordinary Rc values never escape the mutex by alias;
/// public reads and callback commits cross the boundary only through snapshots.
#[derive(Debug)]
struct SharedCell<T>(T);

// SAFETY: only the constructors above/below create this private payload. Generic
// construction requires Send; FidanValue construction/reads/writes/transform are
// encapsulated and detach every ordinary mutable graph while holding the mutex.
unsafe impl<T> Send for SharedCell<T> {}

impl<T: Send> SharedRef<T> {
    pub fn new(val: T) -> Self {
        SharedRef(Arc::new(Mutex::new(SharedCell(val))))
    }
}

impl<T> SharedRef<T> {
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub(crate) fn lock(&self) -> Result<SharedGuard<'_, T>, &'static str> {
        let identity = self.identity();
        if HELD_SHARED.with(|held| held.borrow().contains(&identity)) {
            return Err("recursive access to the same Shared value during update");
        }
        let value = self.0.lock().map_err(|_| "Shared mutex is poisoned")?;
        HELD_SHARED.with(|held| held.borrow_mut().push(identity));
        Ok(SharedGuard { value, identity })
    }

    pub fn clone_ref(&self) -> Self {
        SharedRef(Arc::clone(&self.0))
    }

    pub fn downgrade(&self) -> WeakSharedRef<T> {
        WeakSharedRef(Arc::downgrade(&self.0))
    }
}

impl SharedRef<crate::FidanValue> {
    /// Shared storage never retains ordinary mutable aliases owned by a caller.
    pub fn from_value(value: crate::FidanValue) -> Self {
        Self(Arc::new(Mutex::new(SharedCell(value.parallel_capture()))))
    }

    /// Hold exclusive access over the callback and commit only a successful,
    /// detached result. User code never receives an alias into locked storage.
    pub fn transform<E>(
        &self,
        callback: impl FnOnce(crate::FidanValue) -> Result<crate::FidanValue, E>,
    ) -> Result<crate::FidanValue, E>
    where
        E: From<crate::stdlib::StdlibRuntimeError>,
    {
        let mut current = self.lock().map_err(|error| {
            crate::stdlib::StdlibRuntimeError::new(fidan_diagnostics::diag_code!("R0001"), error)
        })?;
        // ponytail: mutable graphs are copied for isolation and rollback, O(graph size).
        // Reducing that cost requires a persistent thread-safe value representation.
        let result = callback(current.parallel_capture())?;
        *current = result.parallel_capture();
        Ok(result)
    }

    pub fn snapshot(&self) -> Result<crate::FidanValue, &'static str> {
        Ok(self.lock()?.parallel_capture())
    }

    pub fn store_value(&self, value: crate::FidanValue) -> Result<(), &'static str> {
        let value = value.parallel_capture();
        *self.lock()? = value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutable_shared_values_never_escape_the_lock_by_alias() {
        use crate::{FidanList, FidanValue, OwnedRef};
        let source = OwnedRef::new(FidanList::from_vec(vec![FidanValue::Integer(1)]));
        let shared = SharedRef::from_value(FidanValue::List(source.clone()));
        source.borrow_mut().append(FidanValue::Integer(2));
        let FidanValue::List(snapshot) = shared.snapshot().unwrap() else {
            panic!("list")
        };
        assert_eq!(snapshot.borrow().len(), 1);
        snapshot.borrow_mut().append(FidanValue::Integer(3));
        let FidanValue::List(next) = shared.snapshot().unwrap() else {
            panic!("list")
        };
        assert_eq!(next.borrow().len(), 1);
        shared
            .store_value(FidanValue::List(source.clone()))
            .unwrap();
        source.borrow_mut().append(FidanValue::Integer(4));
        let FidanValue::List(stored) = shared.snapshot().unwrap() else {
            panic!("list")
        };
        assert_eq!(stored.borrow().len(), 2);
    }

    #[test]
    fn mutable_transform_is_atomic_under_contention_and_failure() {
        use crate::{FidanList, FidanValue, OwnedRef, ParallelCapture, stdlib::StdlibRuntimeError};
        let shared =
            SharedRef::from_value(FidanValue::List(OwnedRef::new(FidanList::from_vec(vec![
                FidanValue::Integer(0),
            ]))));
        let mut workers = Vec::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        for _ in 0..8 {
            let capture = ParallelCapture::new(&FidanValue::Shared(shared.clone_ref()));
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let FidanValue::Shared(shared) = capture.into_inner() else {
                    panic!("Shared")
                };
                barrier.wait();
                for _ in 0..250 {
                    shared
                        .transform::<StdlibRuntimeError>(|value| {
                            let FidanValue::List(list) = &value else {
                                panic!("list")
                            };
                            let FidanValue::Integer(previous) =
                                list.borrow().get(0).unwrap().clone()
                            else {
                                panic!("integer")
                            };
                            std::thread::yield_now();
                            list.borrow_mut()
                                .set_at(0, FidanValue::Integer(previous + 1));
                            Ok(value)
                        })
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let result = shared.transform::<StdlibRuntimeError>(|value| {
            let FidanValue::List(list) = &value else {
                panic!("list")
            };
            list.borrow_mut().set_at(0, FidanValue::Integer(-1));
            Err(StdlibRuntimeError::new(
                fidan_diagnostics::diag_code!("R0001"),
                "callback failure",
            ))
        });
        assert!(result.is_err());
        let FidanValue::List(snapshot) = shared.snapshot().unwrap() else {
            panic!("list")
        };
        assert!(matches!(
            snapshot.borrow().get(0),
            Some(FidanValue::Integer(2000))
        ));
    }

    #[test]
    fn atomic_read_modify_write_under_contention() {
        let shared = SharedRef::new(0);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..1000 {
                        let mut value = shared.lock().unwrap();
                        let previous = *value;
                        std::thread::yield_now();
                        *value = previous + 1;
                    }
                });
            }
        });
        assert_eq!(*shared.lock().unwrap(), 8000);
    }

    #[test]
    fn recursive_alias_access_is_rejected_and_guard_is_released() {
        let shared = SharedRef::new(1);
        let alias = shared.downgrade().upgrade().unwrap();
        let other = SharedRef::new(2);
        {
            let mut value = shared.lock().unwrap();
            assert!(alias.lock().is_err());
            assert_eq!(*other.lock().unwrap(), 2);
            *value = 3;
        }
        assert_eq!(*alias.lock().unwrap(), 3);
    }
}

/// Weak ARC reference — non-owning companion to `SharedRef<T>`.
/// Upgrading succeeds only while at least one `SharedRef<T>` still exists.
#[derive(Debug, Clone)]
pub struct WeakSharedRef<T>(Weak<Mutex<SharedCell<T>>>);

impl<T> WeakSharedRef<T> {
    pub fn from_shared(shared: &SharedRef<T>) -> Self {
        shared.downgrade()
    }

    pub fn clone_ref(&self) -> Self {
        WeakSharedRef(Weak::clone(&self.0))
    }

    pub fn upgrade(&self) -> Option<SharedRef<T>> {
        self.0.upgrade().map(SharedRef)
    }

    pub fn identity(&self) -> usize {
        self.0.as_ptr() as usize
    }

    pub fn is_alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}
