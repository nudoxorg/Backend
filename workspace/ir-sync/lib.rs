//! ir-sync — iroh-based distribution of libpijul change files to a trusted remote.
//!
//! # VCS-native distribution model
//!
//! The compile/serve plane is fully VCS-native: a package's IR history lives in
//! libpijul channels (see `nudox-ir-vcs`). Pijul change files are already
//! content-addressed — the change hash is recomputable from the change file and
//! libpijul validates it on load. There is **no** parallel CAS for artifacts.
//!
//! **Distribution rule**: when a commit (pijul change) is merged onto a package's
//! durable channel, it is synced to the trusted remote over iroh. The remote
//! imports the change files, verifies their pijul hashes, applies them to its own
//! channel, and confirms the tip. Trust comes from content-addressing + hash
//! verification, not from a trusted transport.
//!
//! # Merge-trigger model
//!
//! Sync is **triggered by merge**, not by a background sweep. The caller (repo
//! merge hook) constructs a [`MergeEvent`] when a commit lands and calls
//! [`Syncer::on_merge`]. Nothing else drives pushes.
//!
//! # Integration points
//!
//! Two seams connect this crate to the repo implementation in `nudox-ir-vcs`:
//!
//! 1. **[`ChangeIo`]** — implement this over `FsChanges` (libpijul's filesystem
//!    changestore at `<repo-root>/changes/`). `FsChanges::filename(hash)` gives the
//!    path; `read`/`write` are plain file I/O.  The `verify` impl MUST call
//!    `libpijul::change::Change::deserialize` and compare the recomputed hash to the
//!    announced `ChangeId` — only that gives you full pijul change validation.
//!
//! 2. **[`ApplyHook`]** — implement this over `IrRepository::apply_change` (or
//!    equivalent) in `nudox-ir-vcs`. After the receiver has written all change files
//!    from an announcement it calls `apply_hook.apply(channel, changes)` to apply
//!    them to the local pristine and advance the channel tip.

pub mod types;
pub mod transport;
pub mod io;

#[cfg(test)]
mod tests;

pub use types::{
    ChangeId, ChannelRef, PackageName, TipAnnouncement, SyncAck, VerifyError, SyncError,
    MergeEvent,
};
pub use io::{ChangeIo, ApplyHook, MemChangeIo};
pub use transport::{Syncer, SyncService, RemoteConfig};
