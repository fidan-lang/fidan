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

    /// Collect all members (fields + methods) of a type by walking the
    /// inheritance chain across all stored documents.  Child entries take
    /// precedence over inherited ones (first-wins).
    ///
    /// No `DashMap Ref` may be held when calling this.
    pub fn collect_type_members(&self, start_type: &str) -> Vec<(String, SymbolEntry)> {
        let mut seen = std::collections::HashSet::<String>::new();
        let mut result: Vec<(String, SymbolEntry)> = Vec::new();
        let mut cur_type = start_type.to_string();

        for _ in 0..8 {
            let prefix = format!("{}.", cur_type);
            let mut next_type: Option<String> = None;
            for kv in self.inner.iter() {
                let tbl = &kv.value().symbol_table;
                for (name, entry) in tbl.all() {
                    if name.starts_with(&prefix) {
                        let member = name[prefix.len()..].to_string();
                        if seen.insert(member.clone()) {
                            result.push((member, entry.clone()));
                        }
                    }
                }
                if next_type.is_none() {
                    if let Some(e) = tbl.get(&cur_type) {
                        next_type = e.ty_name.clone();
                    }
                }
            }
            match next_type {
                Some(p) if !p.is_empty() && p != cur_type => cur_type = p,
                _ => break,
            }
        }
        result
    }

    /// Collect all unqualified (top-level) symbol entries from the document at
    /// `uri`.  Used for `module.` prefix completion when the dot-receiver is an
    /// import alias.
    pub fn get_doc_top_level(&self, uri: &Url) -> Vec<(String, SymbolEntry)> {
        match self.inner.get(uri) {
            Some(kv) => kv
                .value()
                .symbol_table
                .all()
                .filter(|(name, entry)| !name.contains('.') && !entry.is_param)
                .map(|(n, e)| (n.clone(), e.clone()))
                .collect(),
            None => vec![],
        }
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
