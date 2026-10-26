use crate::{FidanHashKey, FidanValue, HashKeyError};
use std::collections::HashMap;
use std::rc::Rc;

/// Copy-on-Write dictionary.
#[derive(Debug, Clone)]
pub struct FidanDict {
    inner: Rc<HashMap<FidanHashKey, (FidanValue, FidanValue)>>,
    // Runtime-only provenance. Ordinary source keys must never brand a dict as an object.
    aot_class: Option<crate::FidanString>,
}

impl FidanDict {
    pub fn new() -> Self {
        FidanDict {
            inner: Rc::new(HashMap::new()),
            aot_class: None,
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        FidanDict {
            inner: Rc::new(HashMap::with_capacity(capacity)),
            aot_class: None,
        }
    }

    pub(crate) fn mark_aot_object(&mut self, class: crate::FidanString) {
        self.aot_class = Some(class);
    }

    pub(crate) fn aot_class_name(&self) -> Option<&str> {
        self.aot_class.as_ref().map(crate::FidanString::as_str)
    }

    pub(crate) fn capture_values(
        &self,
        mut capture: impl FnMut(&FidanValue) -> FidanValue,
    ) -> Self {
        Self {
            inner: Rc::new(
                self.inner
                    .iter()
                    .map(|(hashed, (key, value))| (hashed.clone(), (capture(key), capture(value))))
                    .collect(),
            ),
            aot_class: self.aot_class.clone(),
        }
    }

    pub(crate) fn remap_object_keys(&mut self, identities: &HashMap<usize, usize>) {
        self.inner = Rc::new(
            self.inner
                .iter()
                .map(|(key, value)| (key.remap_objects(identities), value.clone()))
                .collect(),
        );
    }

    pub fn get(&self, key: &FidanValue) -> Result<Option<&FidanValue>, HashKeyError> {
        let key = FidanHashKey::from_value(key)?;
        Ok(self.inner.get(&key).map(|(_, value)| value))
    }

    pub fn get_hashed(&self, key: &FidanHashKey) -> Option<&FidanValue> {
        self.inner.get(key).map(|(_, value)| value)
    }

    pub fn insert(
        &mut self,
        key: FidanValue,
        value: FidanValue,
    ) -> Result<Option<FidanValue>, HashKeyError> {
        let hashed = FidanHashKey::from_value(&key)?;
        Ok(Rc::make_mut(&mut self.inner)
            .insert(hashed, (key, value))
            .map(|(_, previous)| previous))
    }

    pub fn remove(&mut self, key: &FidanValue) -> Result<Option<FidanValue>, HashKeyError> {
        let key = FidanHashKey::from_value(key)?;
        Ok(Rc::make_mut(&mut self.inner)
            .remove(&key)
            .map(|(_, value)| value))
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&FidanValue, &FidanValue)> {
        self.inner.values().map(|(key, value)| (key, value))
    }

    pub fn entries_sorted_refs(&self) -> Vec<(&FidanValue, &FidanValue)> {
        let mut entries: Vec<_> = self
            .inner
            .iter()
            .map(|(hashed, (key, value))| (hashed, key, value))
            .collect();
        entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
        entries
            .into_iter()
            .map(|(_, key, value)| (key, value))
            .collect()
    }

    pub fn entries_sorted(&self) -> Vec<(FidanValue, FidanValue)> {
        self.entries_sorted_refs()
            .into_iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

impl Default for FidanDict {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FidanDict;
    use crate::{FidanString, FidanValue};

    #[test]
    fn entries_sorted_refs_preserve_entries_sorted_order() {
        let mut dict = FidanDict::new();
        let _ = dict.insert(
            FidanValue::String(FidanString::new("b")),
            FidanValue::Integer(2),
        );
        let _ = dict.insert(
            FidanValue::String(FidanString::new("a")),
            FidanValue::Integer(1),
        );

        let owned = dict.entries_sorted();
        let borrowed: Vec<(FidanValue, FidanValue)> = dict
            .entries_sorted_refs()
            .into_iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        assert_eq!(
            owned
                .iter()
                .map(|(key, value)| (crate::display(key), crate::display(value)))
                .collect::<Vec<_>>(),
            borrowed
                .iter()
                .map(|(key, value)| (crate::display(key), crate::display(value)))
                .collect::<Vec<_>>()
        );
    }
}
