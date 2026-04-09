use crate::value::{FidanValue, display};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanonicalFloat {
    Bits(u64),
}

impl CanonicalFloat {
    fn from_f64(value: f64) -> Self {
        let normalized = if value.is_nan() {
            f64::NAN
        } else if value == 0.0 {
            0.0
        } else {
            value
        };
        Self::Bits(normalized.to_bits())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum FidanHashKeyRepr {
    Integer(i64),
    Float(CanonicalFloat),
    Boolean(bool),
    Handle(usize),
    Nothing,
    String(crate::FidanString),
    List(Vec<FidanHashKeyRepr>),
    Dict(Vec<(FidanHashKeyRepr, FidanHashKeyRepr)>),
    HashSet(Vec<FidanHashKeyRepr>),
    Tuple(Vec<FidanHashKeyRepr>),
    EnumType(Arc<str>),
    EnumVariant {
        tag: Arc<str>,
        payload: Vec<FidanHashKeyRepr>,
    },
    ClassType(Arc<str>),
    Namespace(Arc<str>),
    StdlibFn {
        module: Arc<str>,
        name: Arc<str>,
    },
    Function(u32),
    Closure {
        fn_id: u32,
        captured: Vec<FidanHashKeyRepr>,
    },
    Object(usize),
    Shared(usize),
    WeakShared(usize),
    Pending(usize),
    PendingTask(u64),
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
}

impl FidanHashKeyRepr {
    fn from_value(value: &FidanValue) -> Result<Self, HashKeyError> {
        match value {
            FidanValue::Integer(v) => Ok(Self::Integer(*v)),
            FidanValue::Float(v) => Ok(Self::Float(CanonicalFloat::from_f64(*v))),
            FidanValue::Boolean(v) => Ok(Self::Boolean(*v)),
            FidanValue::Handle(v) => Ok(Self::Handle(*v)),
            FidanValue::Nothing => Ok(Self::Nothing),
            FidanValue::String(v) => Ok(Self::String(v.clone())),
            FidanValue::List(values) => values
                .borrow()
                .iter()
                .map(Self::from_value)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::List),
            FidanValue::Dict(entries) => {
                let mut items = entries
                    .borrow()
                    .entries_sorted_refs()
                    .into_iter()
                    .map(|(key, value)| Ok((Self::from_value(key)?, Self::from_value(value)?)))
                    .collect::<Result<Vec<_>, HashKeyError>>()?;
                items.sort();
                Ok(Self::Dict(items))
            }
            FidanValue::HashSet(values) => {
                let mut items = values
                    .borrow()
                    .iter()
                    .map(Self::from_value)
                    .collect::<Result<Vec<_>, _>>()?;
                items.sort();
                Ok(Self::HashSet(items))
            }
            FidanValue::Tuple(values) => values
                .iter()
                .map(Self::from_value)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Tuple),
            FidanValue::EnumType(tag) => Ok(Self::EnumType(Arc::clone(tag))),
            FidanValue::EnumVariant { tag, payload } => Ok(Self::EnumVariant {
                tag: Arc::clone(tag),
                payload: payload
                    .iter()
                    .map(Self::from_value)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            FidanValue::ClassType(name) => Ok(Self::ClassType(Arc::clone(name))),
            FidanValue::Namespace(module) => Ok(Self::Namespace(Arc::clone(module))),
            FidanValue::StdlibFn(module, name) => Ok(Self::StdlibFn {
                module: Arc::clone(module),
                name: Arc::clone(name),
            }),
            FidanValue::Function(id) => Ok(Self::Function(id.0)),
            FidanValue::Closure { fn_id, captured } => Ok(Self::Closure {
                fn_id: fn_id.0,
                captured: captured
                    .iter()
                    .map(Self::from_value)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            FidanValue::Object(object) => Ok(Self::Object(object.identity())),
            FidanValue::Shared(shared) => Ok(Self::Shared(shared.identity())),
            FidanValue::WeakShared(shared) => Ok(Self::WeakShared(shared.identity())),
            FidanValue::Pending(pending) => Ok(Self::Pending(pending.identity())),
            FidanValue::PendingTask(id) => Ok(Self::PendingTask(*id)),
            FidanValue::Range {
                start,
                end,
                inclusive,
            } => Ok(Self::Range {
                start: *start,
                end: *end,
                inclusive: *inclusive,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FidanHashKey {
    repr: FidanHashKeyRepr,
}

impl FidanHashKey {
    pub fn from_value(value: &FidanValue) -> Result<Self, HashKeyError> {
        Ok(Self {
            repr: FidanHashKeyRepr::from_value(value)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct HashKeyError {
    message: Arc<str>,
}

impl HashKeyError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: Arc::from(message.into()),
        }
    }

    pub fn message(&self) -> &str {
        self.message.as_ref()
    }
}

impl std::fmt::Display for HashKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for HashKeyError {}

#[derive(Debug, Clone)]
pub struct FidanHashSet {
    inner: Rc<HashMap<FidanHashKey, FidanValue>>,
}

impl FidanHashSet {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(HashMap::new()),
        }
    }

    pub fn from_values(values: impl IntoIterator<Item = FidanValue>) -> Result<Self, HashKeyError> {
        let mut set = Self::new();
        for value in values {
            set.insert(value)?;
        }
        Ok(set)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&mut self, value: FidanValue) -> Result<bool, HashKeyError> {
        let key = FidanHashKey::from_value(&value)?;
        Ok(Rc::make_mut(&mut self.inner).insert(key, value).is_none())
    }

    pub fn contains(&self, value: &FidanValue) -> Result<bool, HashKeyError> {
        let key = FidanHashKey::from_value(value)?;
        Ok(self.inner.contains_key(&key))
    }

    pub fn remove(&mut self, value: &FidanValue) -> Result<bool, HashKeyError> {
        let key = FidanHashKey::from_value(value)?;
        Ok(Rc::make_mut(&mut self.inner).remove(&key).is_some())
    }

    pub fn union(&self, other: &FidanHashSet) -> FidanHashSet {
        let mut result = self.clone();
        let inner = Rc::make_mut(&mut result.inner);
        inner.extend(
            other
                .inner
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        result
    }

    pub fn intersection(&self, other: &FidanHashSet) -> FidanHashSet {
        let mut result = HashMap::new();
        for (key, value) in self.inner.iter() {
            if other.inner.contains_key(key) {
                result.insert(key.clone(), value.clone());
            }
        }
        FidanHashSet {
            inner: Rc::new(result),
        }
    }

    pub fn difference(&self, other: &FidanHashSet) -> FidanHashSet {
        let mut result = HashMap::new();
        for (key, value) in self.inner.iter() {
            if !other.inner.contains_key(key) {
                result.insert(key.clone(), value.clone());
            }
        }
        FidanHashSet {
            inner: Rc::new(result),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &FidanValue> {
        self.inner.values()
    }

    pub fn values_sorted_refs(&self) -> Vec<&FidanValue> {
        let mut entries: Vec<_> = self.inner.iter().collect();
        entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
        entries.sort_by_cached_key(|(_, value)| display(value));
        entries.into_iter().map(|(_, value)| value).collect()
    }

    pub fn values_sorted(&self) -> Vec<FidanValue> {
        self.values_sorted_refs().into_iter().cloned().collect()
    }
}

impl Default for FidanHashSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{FidanHashKey, FidanHashSet};
    use crate::{FidanClass, FidanDict, FidanObject, FidanString, FidanValue, OwnedRef};
    use rustc_hash::FxHashMap;
    use std::sync::Arc;

    #[test]
    fn canonicalizes_nested_values() {
        let mut dict = FidanDict::new();
        let _ = dict.insert(
            FidanValue::String(FidanString::new("name")),
            FidanValue::String(FidanString::new("Ada")),
        );
        let _ = dict.insert(
            FidanValue::String(FidanString::new("age")),
            FidanValue::Integer(42),
        );

        let mut list = crate::FidanList::new();
        list.append(FidanValue::Integer(1));
        list.append(FidanValue::Integer(2));

        let value = FidanValue::Tuple(vec![
            FidanValue::List(OwnedRef::new(list)),
            FidanValue::Dict(OwnedRef::new(dict)),
        ]);

        let key = FidanHashKey::from_value(&value).expect("hashable value");
        let same = FidanHashKey::from_value(&value).expect("stable hash key");
        assert_eq!(key, same);
    }

    #[test]
    fn deduplicates_values() {
        let mut set = FidanHashSet::new();
        assert!(set.insert(FidanValue::Integer(1)).expect("inserted"));
        assert!(!set.insert(FidanValue::Integer(1)).expect("deduped"));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn dict_accepts_non_string_keys() {
        let mut dict = FidanDict::new();
        let key = FidanValue::Tuple(vec![FidanValue::Boolean(true), FidanValue::Integer(7)]);
        let _ = dict.insert(key.clone(), FidanValue::String(FidanString::new("ok")));

        assert!(matches!(
            dict.get(&key),
            Ok(Some(FidanValue::String(value))) if value.as_str() == "ok"
        ));
    }

    #[test]
    fn object_values_hash_by_identity() {
        let class = Arc::new(FidanClass {
            name: fidan_lexer::Symbol(0),
            name_str: Arc::from("Box"),
            parent: None,
            fields: vec![],
            field_index: FxHashMap::default(),
            methods: FxHashMap::default(),
            has_drop_action: false,
        });
        let first = FidanValue::Object(OwnedRef::new(FidanObject::new(Arc::clone(&class))));
        let second = FidanValue::Object(OwnedRef::new(FidanObject::new(class)));

        let mut set = FidanHashSet::new();
        assert!(set.insert(first.clone()).expect("first insert"));
        assert!(!set.insert(first.clone()).expect("duplicate identity"));
        assert!(set.insert(second.clone()).expect("second object insert"));
        assert_eq!(set.len(), 2);
        assert!(set.contains(&first).expect("contains first"));
        assert!(set.contains(&second).expect("contains second"));
    }

    #[test]
    fn values_sorted_refs_preserve_values_sorted_order() {
        let mut set = FidanHashSet::new();
        assert!(set.insert(FidanValue::Integer(7)).expect("insert int"));
        assert!(
            set.insert(FidanValue::String(FidanString::new("alpha")))
                .expect("insert string")
        );
        assert!(set.insert(FidanValue::Integer(3)).expect("insert int"));

        let owned = set.values_sorted();
        let borrowed: Vec<FidanValue> = set.values_sorted_refs().into_iter().cloned().collect();
        assert_eq!(
            owned.iter().map(crate::display).collect::<Vec<_>>(),
            borrowed.iter().map(crate::display).collect::<Vec<_>>()
        );
    }
}
