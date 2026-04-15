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

    pub fn with_capacity(capacity: usize) -> Self {
        FidanList {
            inner: Rc::new(Vec::with_capacity(capacity)),
        }
    }

    pub fn from_vec(values: Vec<FidanValue>) -> Self {
        FidanList {
            inner: Rc::new(values),
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

    pub fn pop(&mut self) -> Option<FidanValue> {
        Rc::make_mut(&mut self.inner).pop()
    }

    pub fn remove(&mut self, idx: usize) -> Option<FidanValue> {
        let values = Rc::make_mut(&mut self.inner);
        if idx < values.len() {
            Some(values.remove(idx))
        } else {
            None
        }
    }

    /// Set value at index. No-op if out of bounds.
    pub fn set_at(&mut self, idx: usize, val: FidanValue) {
        let v = Rc::make_mut(&mut self.inner);
        if idx < v.len() {
            v[idx] = val;
        }
    }

    pub fn reverse(&mut self) {
        Rc::make_mut(&mut self.inner).reverse();
    }

    pub fn sort_by<F>(&mut self, compare: F)
    where
        F: FnMut(&FidanValue, &FidanValue) -> std::cmp::Ordering,
    {
        Rc::make_mut(&mut self.inner).sort_by(compare);
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

#[cfg(test)]
mod tests {
    use super::FidanList;
    use crate::FidanValue;

    #[test]
    fn cow_pop_and_remove_preserve_shared_original() {
        let mut original = FidanList::with_capacity(3);
        original.append(FidanValue::Integer(1));
        original.append(FidanValue::Integer(2));
        original.append(FidanValue::Integer(3));
        let shared = original.clone();

        assert!(matches!(original.pop(), Some(FidanValue::Integer(3))));
        assert!(matches!(original.remove(0), Some(FidanValue::Integer(1))));
        assert_eq!(original.len(), 1);

        assert_eq!(shared.len(), 3);
        assert!(matches!(shared.get(0), Some(FidanValue::Integer(1))));
        assert!(matches!(shared.get(2), Some(FidanValue::Integer(3))));
    }
}
