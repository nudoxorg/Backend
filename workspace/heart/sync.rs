//! The generic content-addressed sync seam.
//!
//! Several planes distribute **content-addressed items** to a trusted peer over
//! a verifying transport: the IR VCS ships libpijul change files, the object
//! pack plane ships NDPK members/packs. The shape is always the same — *announce
//! a tip, fetch the missing content-addressed items, verify each item's bytes
//! against its id, apply the ordered set, ack the new tip* — so the seam lives
//! here once, generic over the item id and the durable target.
//!
//! This module holds ONLY the transport-agnostic contract: the [`ContentIo`] and
//! [`ApplyHook`] traits plus the [`VerifyError`]/[`SyncError`] vocabulary. It
//! deliberately names no transport (`iroh`) and no engine (`libpijul`) type — an
//! implementor (`ir::sync`, `object_pack`) instantiates the associated types and
//! owns its own provide/fetch wiring. The trust anchor is content-addressing:
//! an item is genuine iff its bytes verify against its id, never because a
//! particular peer served them.

use std::io;

use thiserror::Error;

/// A content-addressed item store: read/write/probe/verify items keyed by their
/// content-address `Id`.
///
/// # Verify-before-write
///
/// [`verify`](ContentIo::verify) MUST succeed before [`write`](ContentIo::write)
/// is called for the same bytes — content-addressing is the only trust anchor,
/// so an implementation that writes unverified bytes undermines the whole model.
/// The sync driver enforces the ordering; implementations must still refuse to
/// write bytes they can detect are inconsistent with the id.
pub trait ContentIo: Send + Sync {
	/// The content-address id — the item's trust anchor (e.g. a pijul change
	/// hash, an NDPK member hash). It is recomputable from the item bytes, so a
	/// correct [`verify`](ContentIo::verify) needs nothing but `(id, bytes)`.
	type Id: Clone + core::fmt::Display;

	/// Read the raw bytes of the item for `id`.
	fn read(&self, id: &Self::Id) -> io::Result<Vec<u8>>;

	/// Write raw item bytes. The caller guarantees [`verify`](ContentIo::verify)
	/// already succeeded for `(id, bytes)`.
	fn write(&self, id: &Self::Id, bytes: &[u8]) -> io::Result<()>;

	/// Whether the item is already present (idempotent re-sync skips it).
	fn has(&self, id: &Self::Id) -> io::Result<bool>;

	/// Verify `bytes` are consistent with `id` — the full content-address check
	/// for this item kind (e.g. libpijul change deserialize + hash, or BLAKE3 /
	/// bao). MUST be sufficient on its own: a passing verify is the sole license
	/// to write.
	fn verify(&self, id: &Self::Id, bytes: &[u8]) -> Result<(), VerifyError>;

	/// Hard per-item byte cap applied *before* writing untrusted bytes. Declared
	/// sizes from an announcement are never trusted — only the bytes received.
	fn max_item_bytes(&self) -> usize {
		64 * 1024 * 1024
	}
}

/// Applies a fetched, dependency-ordered set of items to a durable target and
/// advances it to a new tip — the receiver-side commit half.
///
/// For the IR VCS this is a libpijul channel apply; for the object pack plane it
/// is an install into the pack store. Called only after every item in the set
/// has been fetched, verified, and written through [`ContentIo`].
pub trait ApplyHook: Send + Sync {
	/// The item id type (matches the paired [`ContentIo::Id`]).
	type Id;
	/// What the items are applied to (a channel ref, a pack target, …).
	type Target;
	/// The resulting tip after applying the ordered set.
	type Tip;

	/// Apply `ids` (in dependency order) to `target`, returning the new tip.
	fn apply(&self, target: &Self::Target, ids: &[Self::Id]) -> Result<Self::Tip, SyncError>;
}

/// An item's bytes did not verify against its content-address id.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VerifyError {
	/// A content-address hex id had the wrong length.
	#[error("expected {expected}-char id, got {got} chars")]
	InvalidLength { expected: usize, got: usize },

	/// A hex id contained an out-of-alphabet character.
	#[error("invalid character in content-address id: {0:?}")]
	InvalidChar(char),

	/// The bytes did not hash to the announced id (the core content-address
	/// failure). Ids are rendered to strings so this stays item-kind-agnostic.
	#[error("hash mismatch: expected {expected}, computed {got}")]
	HashMismatch { expected: String, got: String },

	/// The item bytes exceeded the store's per-item cap.
	#[error("item {id} is too large: {size} bytes (max {max})")]
	TooLarge { id: String, size: usize, max: usize },
}

/// A sync operation failed.
#[derive(Debug, Error)]
pub enum SyncError {
	/// An item failed verification. NOTHING is written or applied past this.
	#[error("verification failed: {0}")]
	VerificationFailed(#[from] VerifyError),

	/// The transport (QUIC / iroh / …) failed. Kept as a string so no transport
	/// type leaks into the generic seam.
	#[error("transport error: {0}")]
	Transport(String),

	/// A single item exceeded the configured size cap before it could be written.
	#[error("item {id} exceeds size cap: {size} > {max} bytes")]
	ItemTooLarge { id: String, size: usize, max: usize },

	/// Could not connect to the remote peer.
	#[error("connection to remote failed")]
	ConnectionFailed,

	/// The operation timed out.
	#[error("sync timed out")]
	Timeout,

	/// The remote peer explicitly refused the sync.
	#[error("remote refused: {0}")]
	RemoteRefused(String),

	/// A local [`ContentIo`] I/O error.
	#[error("io error: {0}")]
	Io(#[from] io::Error),

	/// An announcement / message could not be (de)serialized.
	#[error("codec error: {0}")]
	Codec(String),

	/// The sender announced an item it does not actually hold.
	#[error("sender is missing announced item {0}")]
	MissingItem(String),

	/// An implementation-specific rejection carrying the essential detail.
	#[error("sync error: {0}")]
	Other(String),
}
