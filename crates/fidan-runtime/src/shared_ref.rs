use std::sync::{Arc, Mutex};

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
}
