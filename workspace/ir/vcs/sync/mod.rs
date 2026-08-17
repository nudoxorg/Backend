//! iroh-based distribution of libpijul change files to a trusted remote.
//!
//! Implements `heart::sync::{ContentIo, ApplyHook}` for the IR VCS plane.

pub(crate) mod mem_io;
pub mod repo_glue;
pub mod transport;
pub mod types;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_repo_glue;

/// Maximum allowed change blob size (64 MiB).
pub const MAX_CHANGE_BYTES: usize = 64 * 1024 * 1024;

pub use mem_io::{MemChangeIo, change_id_for_bytes};
pub use repo_glue::{
    Error, FsChangeIo, RepoApplyHook, TrustGateError, merge_event, sync_merged,
    trusted_provide_endpoint,
};
pub use transport::{RemoteConfig, SyncService, Syncer};
pub use types::{
    ChangeId, ChannelRef, IrohHash, MergeEvent, PackageName, SyncAck, SyncResponse,
    TipAnnouncement,
};
pub use types::{SyncError, VerifyError};
