use crate::FidanString;
use crate::FidanValue;
use std::collections::HashMap;
use std::sync::Arc;

/// Copy-on-Write dictionary.
#[derive(Debug, Clone)]
pub struct FidanDict {
    inner: Arc<HashMap<FidanString, FidanValue>>,
}

impl FidanDict {
    pub fn new() -> Self {
        FidanDict {
            inner: Arc::new(HashMap::new()),
        }
    }
    pub fn get(&self, key: &FidanString) -> Option<&FidanValue> {
        self.inner.get(key)
    }
    pub fn insert(&mut self, key: FidanString, value: FidanValue) {
        Arc::make_mut(&mut self.inner).insert(key, value);
    }
    pub fn remove(&mut self, key: &FidanString) {
        Arc::make_mut(&mut self.inner).remove(key);
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&FidanString, &FidanValue)> {
        self.inner.iter()
    }
}

impl Default for FidanDict {
    fn default() -> Self {
        Self::new()
    }
}
