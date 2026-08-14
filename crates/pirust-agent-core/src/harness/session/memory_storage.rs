//! In-memory session storage — port of
//! `packages/agent/src/harness/session/memory-storage.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §8 (`SessionStorage` impl). `[LEAF]`
//! depending on `harness::types` (the trait) + `session::uuid` (entry ids).
//!
//! [`InMemorySessionStorage`] (memory-storage.ts:42) holds the tree in a `Mutex`
//! (the trait takes `&self`; Pi mutates instance fields directly). Entry ids come
//! from an INJECTABLE [`Uuidv7Source`] so tests are deterministic — the retry loop
//! mirrors `generateEntryId` (`uuidv7().slice(-8)` with ≤100 collision retries,
//! then a full-uuid fallback; memory-storage.ts:28-36).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::uuid::{SystemSource, Uuidv7Generator, Uuidv7Source};
use super::{
    build_labels_by_id, entry_id, entry_type_tag, leaf_id_after_entry, system_now_iso,
    update_label_cache, Clock, SystemClock,
};
use crate::harness::session::uuid::create_session_id;
use crate::harness::types::{
    SessionError, SessionErrorCode, SessionMetadata, SessionStorage, SessionTreeEntry,
};

/// Interior tree state guarded by the storage `Mutex`.
struct State<S: Uuidv7Source> {
    entries: Vec<SessionTreeEntry>,
    by_id: HashMap<String, SessionTreeEntry>,
    labels_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
    generator: Uuidv7Generator<S>,
}

impl<S: Uuidv7Source> State<S> {
    /// `generateEntryId` (memory-storage.ts:28-36): a short id from the uuid's
    /// random tail, retried on collision, falling back to a full uuid.
    fn next_entry_id(&mut self) -> String {
        for _ in 0..100 {
            let full = self.generator.generate();
            let short = full[full.len() - 8..].to_string();
            if !self.by_id.contains_key(&short) {
                return short;
            }
        }
        self.generator.generate()
    }
}

/// In-memory [`SessionStorage`] (memory-storage.ts:42).
pub struct InMemorySessionStorage<S: Uuidv7Source + Send + Sync = SystemSource> {
    metadata: SessionMetadata,
    clock: Box<dyn Clock>,
    state: Mutex<State<S>>,
}

impl InMemorySessionStorage<SystemSource> {
    /// Empty store with a generated metadata id, system uuid source + clock.
    pub fn new() -> Self {
        Self::with_metadata(SessionMetadata {
            id: create_session_id(),
            created_at: system_now_iso(),
        })
    }

    /// Empty store with the given metadata, system uuid source + clock.
    pub fn with_metadata(metadata: SessionMetadata) -> Self {
        Self::from_options(SystemSource, Box::new(SystemClock), Vec::new(), metadata)
            .expect("empty entries cannot have a dangling leaf")
    }
}

impl Default for InMemorySessionStorage<SystemSource> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Uuidv7Source + Send + Sync + 'static> InMemorySessionStorage<S> {
    /// Full constructor with an injected uuid `source` + `clock` (memory-storage.ts:51-61).
    ///
    /// Rehydrates the id/label caches and the leaf from `entries`, erroring if the
    /// derived leaf is dangling (memory-storage.ts:57-59).
    pub fn from_options(
        source: S,
        clock: Box<dyn Clock>,
        entries: Vec<SessionTreeEntry>,
        metadata: SessionMetadata,
    ) -> Result<Self, SessionError> {
        let by_id: HashMap<String, SessionTreeEntry> = entries
            .iter()
            .map(|e| (entry_id(e).to_string(), e.clone()))
            .collect();
        let labels_by_id = build_labels_by_id(&entries);
        let mut leaf_id: Option<String> = None;
        for entry in &entries {
            leaf_id = leaf_id_after_entry(entry);
        }
        if let Some(ref lid) = leaf_id {
            if !by_id.contains_key(lid) {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {lid} not found"),
                ));
            }
        }
        Ok(Self {
            metadata,
            clock,
            state: Mutex::new(State {
                entries,
                by_id,
                labels_by_id,
                leaf_id,
                generator: Uuidv7Generator::with_source(source),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<S>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl<S: Uuidv7Source + Send + Sync + 'static> SessionStorage for InMemorySessionStorage<S> {
    type Metadata = SessionMetadata;

    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(self.metadata.clone())
    }

    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let state = self.lock();
        if let Some(ref lid) = state.leaf_id {
            if !state.by_id.contains_key(lid) {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {lid} not found"),
                ));
            }
        }
        Ok(state.leaf_id.clone())
    }

    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError> {
        let mut state = self.lock();
        if let Some(ref lid) = leaf_id {
            if !state.by_id.contains_key(lid) {
                return Err(SessionError::new(
                    SessionErrorCode::NotFound,
                    format!("Entry {lid} not found"),
                ));
            }
        }
        let entry = SessionTreeEntry::Leaf {
            id: state.next_entry_id(),
            parent_id: state.leaf_id.clone(),
            timestamp: self.clock.now_iso(),
            target_id: leaf_id.clone(),
        };
        let entry_key = entry_id(&entry).to_string();
        state.entries.push(entry.clone());
        state.by_id.insert(entry_key, entry);
        state.leaf_id = leaf_id;
        Ok(())
    }

    async fn create_entry_id(&self) -> Result<String, SessionError> {
        Ok(self.lock().next_entry_id())
    }

    async fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError> {
        let mut state = self.lock();
        let key = entry_id(&entry).to_string();
        state.entries.push(entry.clone());
        state.by_id.insert(key, entry.clone());
        update_label_cache(&mut state.labels_by_id, &entry);
        state.leaf_id = leaf_id_after_entry(&entry);
        Ok(())
    }

    async fn get_entry(&self, id: &str) -> Result<Option<SessionTreeEntry>, SessionError> {
        Ok(self.lock().by_id.get(id).cloned())
    }

    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionTreeEntry>, SessionError> {
        Ok(self
            .lock()
            .entries
            .iter()
            .filter(|e| entry_type_tag(e) == entry_type)
            .cloned()
            .collect())
    }

    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        Ok(self.lock().labels_by_id.get(id).cloned())
    }

    async fn get_path_to_root(
        &self,
        leaf_id: Option<String>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError> {
        let Some(leaf_id) = leaf_id else {
            return Ok(Vec::new());
        };
        let state = self.lock();
        let mut path: Vec<SessionTreeEntry> = Vec::new();
        let mut current = state.by_id.get(&leaf_id).cloned().ok_or_else(|| {
            SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {leaf_id} not found"),
            )
        })?;
        loop {
            path.insert(0, current.clone());
            let Some(parent_id) = super::entry_parent_id(&current).map(str::to_string) else {
                break;
            };
            let parent = state.by_id.get(&parent_id).cloned().ok_or_else(|| {
                SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {parent_id} not found"),
                )
            })?;
            current = parent;
        }
        Ok(path)
    }

    async fn get_entries(&self) -> Result<Vec<SessionTreeEntry>, SessionError> {
        Ok(self.lock().entries.clone())
    }
}
