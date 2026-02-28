use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use rustc_hash::FxHashMap;

/// An interned identifier. Equality and hashing are O(1) (`u32` compare).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(pub u32);

/// Global symbol interner. All identifiers produced by the lexer go through here.
///
/// `SymbolInterner` is `Send + Sync` so it can live in the `Session` and be
/// shared across threads without wrapping in an extra `Arc`.
#[derive(Debug, Default)]
pub struct SymbolInterner {
    next_id:    AtomicU32,
    str_to_id:  RwLock<FxHashMap<Arc<str>, Symbol>>,
    id_to_str:  RwLock<Vec<Arc<str>>>,
}

impl SymbolInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern a string slice, returning its stable `Symbol`.
    pub fn intern(&self, s: &str) -> Symbol {
        // Fast path: already interned
        {
            let map = self.str_to_id.read().unwrap();
            if let Some(&sym) = map.get(s) {
                return sym;
            }
        }

        // Slow path: insert
        let mut map = self.str_to_id.write().unwrap();
        // Double-check after acquiring write lock
        if let Some(&sym) = map.get(s) {
            return sym;
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let sym = Symbol(id);
        let arc: Arc<str> = Arc::from(s);
        map.insert(Arc::clone(&arc), sym);
        self.id_to_str.write().unwrap().push(arc);

        sym
    }

    /// Resolve a `Symbol` back to its string representation.
    pub fn resolve(&self, sym: Symbol) -> Arc<str> {
        let strings = self.id_to_str.read().unwrap();
        Arc::clone(&strings[sym.0 as usize])
    }
}
