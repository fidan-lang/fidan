//! Thread-safe in-memory store for all open documents.

use crate::document::Document;
use crate::symbols::SymbolEntry;
use dashmap::DashMap;
use dashmap::mapref::one::{MappedRef, MappedRefMut};
use tower_lsp::lsp_types::Url;

/// Concurrent map from `Url` → `Document`.
///
/// All operations are thread-safe — `DashMap` shards the map internally so
/// multiple threads can read/write without a global lock.
pub struct DocumentStore {
    inner: DashMap<Url, StoredDocument>,
}

#[derive(Clone)]
pub(crate) struct StoredDocument {
    uri: Url,
    document: Document,
}

fn document_identity(uri: &Url) -> Url {
    let Ok(path) = uri.to_file_path() else {
        return uri.clone();
    };
    // Canonical paths unify editor percent-encoding/drive casing with import URLs.
    // ponytail: this performs filesystem IO per lookup; cache with watcher
    // invalidation if profiling shows this boundary needs it.
    let canonical = path
        .canonicalize()
        .or_else(|_| {
            let parent = path
                .parent()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            let name = path
                .file_name()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            Ok::<_, std::io::Error>(parent.canonicalize()?.join(name))
        })
        .unwrap_or(path);
    Url::from_file_path(canonical).unwrap_or_else(|_| uri.clone())
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    /// Insert or replace the document stored under `uri`.
    pub fn insert(&self, uri: Url, doc: Document) {
        match self.inner.entry(document_identity(&uri)) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let current = &entry.get().document;
                if current.version >= 0 && doc.version < current.version {
                    return;
                }
                entry.insert(StoredDocument { uri, document: doc });
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(StoredDocument { uri, document: doc });
            }
        }
    }

    /// Snapshot only editor-owned text. Background documents must reload from disk.
    /// No DashMap guard escapes into import graph resolution.
    pub fn open_sources(&self) -> std::collections::HashMap<std::path::PathBuf, String> {
        self.inner
            .iter()
            .filter(|entry| entry.value().document.version >= 0)
            .filter_map(|entry| {
                let path = entry.key().to_file_path().ok()?;
                Some((
                    path.canonicalize().unwrap_or(path),
                    entry.value().document.text.clone(),
                ))
            })
            .collect()
    }

    pub fn dependent_documents(&self, changed: &Url) -> Vec<(Url, i32, String)> {
        let changed = document_identity(changed);
        let documents = self
            .inner
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();
        // Normalize each edge once, outside the fixed-point walk and map guards.
        let documents = documents
            .into_iter()
            .map(|(identity, stored)| {
                let document = &stored.document;
                let dependencies = document
                    .imports
                    .values()
                    .chain(document.direct_imports.values().map(|(uri, _)| uri))
                    .chain(document.wildcard_imports.iter())
                    .map(document_identity)
                    .collect::<Vec<_>>();
                (identity, stored, dependencies)
            })
            .collect::<Vec<_>>();
        let mut affected = std::collections::HashSet::from([changed.clone()]);
        loop {
            let before = affected.len();
            for (identity, _, dependencies) in &documents {
                if dependencies.iter().any(|uri| affected.contains(uri)) {
                    affected.insert(identity.clone());
                }
            }
            if before == affected.len() {
                break;
            }
        }
        documents
            .into_iter()
            .filter(|(identity, stored, _)| {
                *identity != changed && stored.document.version >= 0 && affected.contains(identity)
            })
            .map(|(_, stored, _)| (stored.uri, stored.document.version, stored.document.text))
            .collect()
    }

    /// Borrow the document stored under `uri`, if present.
    pub fn get(&self, uri: &Url) -> Option<MappedRef<'_, Url, StoredDocument, Document>> {
        self.inner
            .get(&document_identity(uri))
            .map(|entry| entry.map(|stored| &stored.document))
    }

    /// Mutably borrow the document stored under `uri`, if present.
    pub fn get_mut(&self, uri: &Url) -> Option<MappedRefMut<'_, Url, StoredDocument, Document>> {
        self.inner
            .get_mut(&document_identity(uri))
            .map(|entry| entry.map(|stored| &mut stored.document))
    }

    /// Remove and discard the document stored under `uri`.
    pub fn remove(&self, uri: &Url) {
        self.inner.remove(&document_identity(uri));
    }

    /// Search every open document for a symbol entry with the given key.
    ///
    /// Returns a clone of the first matching entry together with the URI of
    /// the document it was found in.  The `Ref` from `get()` must be dropped
    /// **before** calling this method to avoid re-entrant shard locking.
    pub fn find_in_any_doc(&self, key: &str) -> Option<(Url, SymbolEntry)> {
        for kv in self.inner.iter() {
            if let Some(e) = kv.value().document.symbol_table.get(key) {
                return Some((kv.value().uri.clone(), e.clone()));
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
                let tbl = &kv.value().document.symbol_table;
                for (name, entry) in tbl.all() {
                    if name.starts_with(&prefix) {
                        let member = name[prefix.len()..].to_string();
                        if seen.insert(member.clone()) {
                            result.push((member, entry.clone()));
                        }
                    }
                }
                if next_type.is_none()
                    && let Some(e) = tbl.get(&cur_type)
                {
                    next_type = e.parent_type_name.clone();
                }
            }
            match next_type {
                Some(p) if !p.is_empty() && p != cur_type => cur_type = p,
                _ => break,
            }
        }
        result
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
