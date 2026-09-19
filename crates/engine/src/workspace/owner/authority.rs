//! Owner-minted capabilities used to resume durable remote attempts.

use super::{WorkspaceModel, WorkspaceOwner};

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Mints the only restart capability accepted by the dispatch journal.
    ///
    /// The journal contributes the latest durable revocation and notification
    /// observations; the owner contributes its checked selected root, live
    /// lease epoch, and full-width fence. Keeping minting here prevents a
    /// caller from turning an arbitrary epoch-shaped value into authority.
    pub(crate) fn restart_authority(
        &self,
        revocation_version: u64,
        notification_cursor: u64,
    ) -> crate::dispatch::OwnerRestartAuthority {
        crate::dispatch::OwnerRestartAuthority::mint(
            self.head.root().to_bytes(),
            self.lease.epoch(),
            self.lease.fence(),
            revocation_version,
            notification_cursor,
        )
    }
}
