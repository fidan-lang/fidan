//! Thread-safe in-memory store for all open documents.

use crate::document::Document;
use crate::symbols::SymbolEntry;
use dashmap::DashMap;
use dashmap::mapref::one::Ref;
use dashmap::mapref::one::RefMut;
use tower_lsp::lsp_types::Url;

/// Concurrent map from `Url` → `Document`.
///
/// All operations are thread-safe — `DashMap` shards the map internally so
/// multiple threads can read/write without a global lock.
pub struct DocumentStore {
    inner: DashMap<Url, Document>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Insert or replace the document stored under `uri`.
    pub fn insert(&self, uri: Url, doc: Document) {
        self.inner.insert(uri, doc);
    }

    /// Borrow the document stored under `uri`, if present.
    pub fn get(&self, uri: &Url) -> Option<Ref<'_, Url, Document>> {
        self.inner.get(uri)
    }

    /// Mutably borrow the document stored under `uri`, if present.
    pub fn get_mut(&self, uri: &Url) -> Option<RefMut<'_, Url, Document>> {
        self.inner.get_mut(uri)
    }

    /// Remove and discard the document stored under `uri`.
    pub fn remove(&self, uri: &Url) {
        self.inner.remove(uri);
    }

    /// Search every open document for a symbol entry with the given key.
    ///
    /// Returns a clone of the first matching entry together with the URI of
    /// the document it was found in.  The `Ref` from `get()` must be dropped
    /// **before** calling this method to avoid re-entrant shard locking.
    pub fn find_in_any_doc(&self, key: &str) -> Option<(Url, SymbolEntry)> {
        for kv in self.inner.iter() {
            if let Some(e) = kv.value().symbol_table.get(key) {
                return Some((kv.key().clone(), e.clone()));
            }
        }
        None
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
