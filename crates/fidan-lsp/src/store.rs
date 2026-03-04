//! Thread-safe in-memory store for all open documents.

use crate::document::Document;
use dashmap::DashMap;
use dashmap::mapref::one::Ref;
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

    /// Remove and discard the document stored under `uri`.
    pub fn remove(&self, uri: &Url) {
        self.inner.remove(uri);
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
