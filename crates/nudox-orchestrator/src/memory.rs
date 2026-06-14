use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nudox_core::{BlobRef, FutureParseQueue, GlobalSymbolId, GlobalSymbolQuery, GlobalSymbolStore, LibRef, OccurrenceId, Result};

/// In-memory implementation of [`GlobalSymbolStore`] for use in tests.
#[derive(Clone)]
pub struct InMemoryGlobalSymbolStore {
    /// `(lib_name, lib_version, symbol_name)` → [`GlobalSymbolId`]
    symbols: Arc<Mutex<HashMap<(String, String, String), GlobalSymbolId>>>,
    /// [`GlobalSymbolId`] → all associated [`OccurrenceId`]s
    associations: Arc<Mutex<HashMap<GlobalSymbolId, Vec<OccurrenceId>>>>,
}

impl InMemoryGlobalSymbolStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            symbols: Arc::new(Mutex::new(HashMap::new())),
            associations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Pre-populate a known symbol mapping (for tests and demo bootstrap).
    pub fn insert(&self, lib: &LibRef, symbol_name: &str, id: GlobalSymbolId) {
        let mut map = self.symbols.lock().expect("mutex poisoned");
        map.insert(
            (lib.name.clone(), lib.version.clone(), symbol_name.to_string()),
            id,
        );
    }

    /// Return all occurrences associated with a global symbol (for test assertions).
    pub fn occurrences_for(&self, global_id: GlobalSymbolId) -> Vec<OccurrenceId> {
        self.associations
            .lock()
            .expect("mutex poisoned")
            .get(&global_id)
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for InMemoryGlobalSymbolStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GlobalSymbolStore for InMemoryGlobalSymbolStore {
    async fn lookup(&self, lib: &LibRef, symbol_name: &str) -> Result<Option<GlobalSymbolId>> {
        let map = self.symbols.lock().expect("mutex poisoned");
        let key = (lib.name.clone(), lib.version.clone(), symbol_name.to_string());
        Ok(map.get(&key).copied())
    }

    async fn associate(&self, global_id: GlobalSymbolId, occurrence: OccurrenceId) -> Result<()> {
        self.associations
            .lock()
            .expect("mutex poisoned")
            .entry(global_id)
            .or_default()
            .push(occurrence);
        Ok(())
    }
}

#[async_trait]
impl GlobalSymbolQuery for InMemoryGlobalSymbolStore {
    async fn get_occurrences(&self, global_id: GlobalSymbolId) -> Result<Vec<OccurrenceId>> {
        Ok(self.occurrences_for(global_id))
    }
}

/// In-memory implementation of [`FutureParseQueue`] for use in tests.
#[derive(Clone)]
pub struct InMemoryFutureParseQueue {
    // key: (lib_name, lib_version) → vec of BlobRefs
    inner: Arc<Mutex<HashMap<(String, String), Vec<BlobRef>>>>,
}

impl InMemoryFutureParseQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return all queued blob refs for a lib (without draining, for assertions).
    pub fn peek_for_lib(&self, lib: &LibRef) -> Vec<BlobRef> {
        let map = self.inner.lock().expect("mutex poisoned");
        let key = (lib.name.clone(), lib.version.clone());
        map.get(&key).cloned().unwrap_or_default()
    }
}

impl Default for InMemoryFutureParseQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FutureParseQueue for InMemoryFutureParseQueue {
    async fn enqueue(&self, lib: LibRef, blob_ref: BlobRef) -> Result<()> {
        let mut map = self.inner.lock().expect("mutex poisoned");
        let key = (lib.name.clone(), lib.version.clone());
        map.entry(key).or_default().push(blob_ref);
        Ok(())
    }

    async fn drain_for_lib(&self, lib: &LibRef) -> Result<Vec<BlobRef>> {
        let mut map = self.inner.lock().expect("mutex poisoned");
        let key = (lib.name.clone(), lib.version.clone());
        Ok(map.remove(&key).unwrap_or_default())
    }
}
