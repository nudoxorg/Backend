//! Pure workspace model capability boundary.

use super::head::WorkspaceSnapshot;
use super::transition::{PersistedTransition, PreparedTransition, TransactionId};
use backend_store::FileStore;

/// Workspace model capability boundary.
pub trait WorkspaceModel: Send + Sync + 'static {
    /// Durable intent type.
    type Intent: Clone + Send + Sync + crate::queue::QueueSized + 'static;
    /// Planner/recovery error.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Returns a stable canonical request identity.
    fn request_id(&self, intent: &Self::Intent) -> [u8; 32];

    /// Plans a complete checked transition against one selected snapshot.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error>;

    /// Re-admits a persisted transition against fresh typed state and closure
    /// evidence.  Implementations must decode untrusted bytes and use the
    /// backend-version `admit_*` APIs before returning a capability.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn admit_persisted(
        &self,
        persisted: &PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error>;

    /// Re-admits a persisted transition with access to the durable relation
    /// CAS. Models with path-copy state can override this to avoid hydrating
    /// untouched descendants during restart; the default preserves the
    /// original bounded model contract.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn admit_persisted_with_store(
        &self,
        persisted: &PersistedTransition,
        _store: &FileStore,
    ) -> Result<PreparedTransition, Self::Error> {
        self.admit_persisted(persisted)
    }
}
