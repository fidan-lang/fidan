use std::sync::{Arc, Mutex, Weak};

/// ARC reference — only for `Shared oftype T` values.
/// Uses Arc<Mutex<T>> for safe cross-thread shared mutation.
#[derive(Debug, Clone)]
pub struct SharedRef<T>(pub Arc<Mutex<T>>);

impl<T> SharedRef<T> {
    pub fn new(val: T) -> Self {
        SharedRef(Arc::new(Mutex::new(val)))
    }
    pub fn clone_ref(&self) -> Self {
        SharedRef(Arc::clone(&self.0))
    }

    pub fn downgrade(&self) -> WeakSharedRef<T> {
        WeakSharedRef(Arc::downgrade(&self.0))
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

    pub fn is_alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}
