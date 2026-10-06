use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

thread_local! {
    static HELD_SHARED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Exclusive Shared access. Tracks ownership on this thread to reject recursive
/// access from callbacks instead of blocking forever on a non-reentrant mutex.
pub struct SharedGuard<'a, T> {
    value: MutexGuard<'a, T>,
    identity: usize,
}

impl<T> Deref for SharedGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> DerefMut for SharedGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
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
pub struct SharedRef<T>(pub Arc<Mutex<T>>);

impl<T> SharedRef<T> {
    pub fn new(val: T) -> Self {
        SharedRef(Arc::new(Mutex::new(val)))
    }

    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub fn lock(&self) -> Result<SharedGuard<'_, T>, &'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
pub struct WeakSharedRef<T>(pub Weak<Mutex<T>>);

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
