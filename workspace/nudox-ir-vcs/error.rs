//! Error type for `nudox-ir-vcs`.

use thiserror::Error;

use ir_stream::StreamError;

/// Unified error type for all VCS operations.
#[derive(Debug, Error)]
pub enum VcsError {
    /// Catch-all for libpijul errors (apply, record, output, txn).
    ///
    /// libpijul's error types are not easily `std::error::Error + Send + Sync`,
    /// so we convert via `anyhow` before storing here.
    #[error("libpijul error: {0}")]
    Pijul(#[from] anyhow::Error),

    /// Archive sealing failure.
    #[error("seal error: {0}")]
    Seal(#[from] nudox_ir_archive::SealError),

    /// A `symbols/*` file contained unexpected or empty content.
    #[error("corrupt symbol file at path '{path}': {reason}")]
    CorruptSymbolFile { path: String, reason: String },

    /// A reference name (branch, tag, or version label) was empty or contained a
    /// channel-unsafe / reserved sequence.
    #[error("invalid {kind} name '{name}': {reason}")]
    InvalidRefName { kind: String, name: String, reason: String },

    /// A reference (branch/tag/version) that must not yet exist already does.
    #[error("reference already exists: {reference}")]
    RefAlreadyExists { reference: String },

    /// A reference (branch/tag/version) named in a serve/resolve call does not
    /// exist.
    #[error("reference not found: {reference}")]
    RefNotFound { reference: String },

    /// A reference cannot be served/materialized directly (e.g. a bare change
    /// hash, which is a point in a branch's history, not a channel tip — capture
    /// it as a tag or branch to serve it).
    #[error("reference is not directly servable: {reference}")]
    RefNotServable { reference: String },

    /// A branch-lifecycle operation was refused because it would remove or
    /// clobber the repository's current working branch.
    #[error("operation refused on the current working branch: {branch}")]
    CurrentBranchProtected { branch: String },

    /// A [`crate::session::StagedEntry`] belongs to a package other than the
    /// one this repository tracks.  The entry is rejected before any working-copy
    /// mutation; previously processed entries in the same batch may have already
    /// been written (partial-batch semantics — see
    /// [`crate::session::RecordingSession::stage`]).
    #[error("foreign package in staged entry: expected {expected}, got {got}")]
    ForeignPackage { expected: String, got: String },

    /// An `ir-stream` protocol error: framing violation, count mismatch,
    /// version mismatch, truncation, or decode failure. The recording session
    /// is abandoned before this error is returned.
    #[error("stream protocol error: {0}")]
    Stream(#[from] StreamError),

    /// A change hash that must be present in the changestore (written by the
    /// iroh-sync import path via `nudox-sync`'s `FsChangeIo`) is absent.
    ///
    /// This error is returned by [`crate::repo::IrRepository::apply_external_changes`]
    /// when a caller tries to apply a change whose file has not yet been written
    /// to the filesystem changestore. The caller (nudox-sync's `RepoApplyHook`)
    /// must ensure all change files are written via `FsChangeIo::write_change`
    /// before calling `apply_external_changes`.
    #[error("change {hash} missing from changestore")]
    ChangeMissingFromStore { hash: String },
}
