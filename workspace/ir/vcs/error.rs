//! Error type for `nudox-ir-vcs`.

use std::io;

use thiserror::Error;

use crate::protocol::StreamError;

/// Wrap a libpijul/sanakirja error into a [`VcsError::Pijul`].
///
/// libpijul's error types vary per operation (the sanakirja backend uses
/// [`SanakirjaError`], [`TxnErr<SanakirjaError>`], [`TreeErr<SanakirjaError>`],
/// and other wrapper types that all implement `std::error::Error + Send +
/// Sync`). Because these associated types resolve to different concrete types
/// and cannot be unified with a common `From` impl, we preserve them via
/// `Box<dyn Error>`. This is a documented, unavoidable erasure boundary —
/// libpijul's type system makes enumerating every concrete error type
/// impractical and version-fragile.
pub fn pijul_err(source: impl std::error::Error + Send + Sync + 'static) -> VcsError {
    VcsError::Pijul(Box::new(source))
}

/// Unified error type for all VCS operations.
#[derive(Debug, Error)]
pub enum VcsError {
    /// A libpijul/sanakirja error (pristine, txn, record, apply, output, …).
    ///
    /// The source is boxed because libpijul resolves its error types through
    /// associated types that differ per operation — they share no common
    /// concrete supertype beyond `std::error::Error`.  See [`pijul_err`].
    #[error("libpijul error")]
    Pijul(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A host I/O error (filesystem create, read, write).
    #[error("i/o error")]
    Io(#[from] io::Error),

    /// Archive sealing failure.
    #[error("seal error")]
    Seal(#[from] crate::archive::SealError),

    /// A `symbols/*` file contained unexpected or empty content.
    #[error("corrupt symbol file")]
    CorruptSymbolFile { path: String, reason: String },

    /// A reference name (branch, tag, or version label) was empty or contained a
    /// channel-unsafe / reserved sequence.
    #[error("invalid ref name")]
    InvalidRefName {
        kind: String,
        name: String,
        reason: String,
    },

    /// A hexadecimal hash/id string contained an invalid character.
    #[error("invalid hex digit in hash or id")]
    InvalidHexDigit,

    /// A reference (branch/tag/version) that must not yet exist already does.
    #[error("reference already exists")]
    RefAlreadyExists { reference: String },

    /// A reference (branch/tag/version) named in a serve/resolve call does not
    /// exist.
    #[error("reference not found")]
    RefNotFound { reference: String },

    /// A reference cannot be served/materialized directly (e.g. a bare change
    /// hash, which is a point in a branch's history, not a channel tip — capture
    /// it as a tag or branch to serve it).
    #[error("reference is not directly servable")]
    RefNotServable { reference: String },

    /// A branch-lifecycle operation was refused because it would remove or
    /// clobber the repository's current working branch.
    #[error("operation refused on current working branch")]
    CurrentBranchProtected { branch: String },

    /// A [`crate::session::StagedEntry`] belongs to a package other than the
    /// one this repository tracks.  The entry is rejected before any working-copy
    /// mutation; previously processed entries in the same batch may have already
    /// been written (partial-batch semantics — see
    /// [`crate::session::RecordingSession::stage`]).
    #[error("foreign package in staged entry")]
    ForeignPackage { expected: String, got: String },

    /// An `ir-stream` protocol error: framing violation, count mismatch,
    /// version mismatch, truncation, or decode failure. The recording session
    /// is abandoned before this error is returned.
    #[error("stream protocol error")]
    Stream(#[from] StreamError),

    /// A change hash that must be present in the changestore (written by the
    /// iroh-sync import path via `nudox-sync`'s `FsChangeIo`) is absent.
    ///
    /// This error is returned by [`crate::repo::IrRepository::apply_external_changes`]
    /// when a caller tries to apply a change whose file has not yet been written
    /// to the filesystem changestore. The caller (nudox-sync's `RepoApplyHook`)
    /// must ensure all change files are written via `FsChangeIo::write_change`
    /// before calling `apply_external_changes`.
    #[error("change missing from changestore")]
    ChangeMissingFromStore { hash: String },

    /// §12.3: `output_repository_no_pending` returned merge conflicts; all
    /// conflict paths are listed.  The caller must resolve conflicts before
    /// parsing any F1 blob.
    #[error("repository has unresolved conflicts")]
    ConflictedState { paths: Vec<String> },

    /// F1 canonical format parse/serialize error.
    #[error("F1 format error")]
    F1(#[from] crate::f1::F1Error),
}
