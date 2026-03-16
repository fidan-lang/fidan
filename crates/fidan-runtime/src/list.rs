use crate::FidanValue;
use std::rc::Rc;

/// Copy-on-Write list. Cheap to clone; physical copy on first mutation.
#[derive(Debug, Clone)]
pub struct FidanList {
    // Rc for shared single-threaded backing storage (COW).
    inner: Rc<Vec<FidanValue>>,
}

impl FidanList {
    pub fn new() -> Self {
        FidanList {
            inner: Rc::new(Vec::new()),
        }
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn get(&self, idx: usize) -> Option<&FidanValue> {
        self.inner.get(idx)
    }

    /// Append a value. Clones the inner Vec only if there are other owners.
    pub fn append(&mut self, val: FidanValue) {
        Rc::make_mut(&mut self.inner).push(val);
    }

    /// Set value at index. No-op if out of bounds.
    pub fn set_at(&mut self, idx: usize, val: FidanValue) {
        let v = Rc::make_mut(&mut self.inner);
        if idx < v.len() {
            v[idx] = val;
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, FidanValue> {
        self.inner.iter()
    }
}

impl Default for FidanList {
    fn default() -> Self {
        Self::new()
    }
}
