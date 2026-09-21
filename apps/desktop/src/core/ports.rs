//! Narrow dependency-inversion contracts used by views and runtime adapters.
//!
//! GPUI views receive these read-only traits.  They never receive an engine
//! client, transport DTO, or persistence handle.  DTO mapping lives in
//! `runtime::mapping`, which keeps the widget layer replaceable and testable.

use crate::core::ids::VersionedRoot;
use crate::model::snapshot::AppSnapshot;
use crate::navigation::{ActionId, Intent};
use std::sync::Arc;

/// A read-only snapshot projection suitable for any widget.
pub trait SnapshotReadModel {
    /// Returns the immutable snapshot currently visible to the UI thread.
    fn snapshot(&self) -> Arc<AppSnapshot>;

    /// Returns the root key the projection is based on.
    fn root_key(&self) -> VersionedRoot {
        self.snapshot().key()
    }
}

/// The only command seam a view needs in order to cause a state transition.
pub trait IntentDispatcher {
    /// Queues a typed intent without performing engine work on the UI thread.
    fn dispatch(&mut self, intent: Intent);
}

/// Read-only action metadata exposed to accessibility tooling and palettes.
pub trait ActionCatalog {
    /// Returns stable actions in deterministic order.
    fn actions(&self) -> &[ActionId];
}
