use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot policy — sealed, so only the two provided implementors exist.
// ─────────────────────────────────────────────────────────────────────────────

mod private {
    pub trait Sealed {}
}

/// Whether a [`Cursor`]'s snapshot is **enforced** at construction time or only
/// carried as an advisory hint.
///
/// Only [`Enforced`] and [`Advisory`] implement this trait; the sealed module
/// prevents third-party impls.
pub trait SnapshotPolicy: private::Sealed + std::fmt::Debug + Clone + PartialEq + Eq {}

/// The snapshot is **checked at construction** — holding a `Cursor<K, Enforced>`
/// is proof that the cursor was minted against the live snapshot. Text search
/// uses this policy.
///
/// Constructible only via [`Cursor::mint_enforced`] (crate-internal), which the
/// text query path calls after verifying the live snapshot matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Enforced {}

/// The snapshot is **carried but not enforced** — an eventually-consistent
/// backend (e.g. Qdrant ANN) has no cheap content hash, so completeness is
/// best-effort. The policy is explicit in every call site's type signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Advisory {}

impl private::Sealed for Enforced {}
impl private::Sealed for Advisory {}
impl SnapshotPolicy for Enforced {}
impl SnapshotPolicy for Advisory {}

// ─────────────────────────────────────────────────────────────────────────────
// Cursor
// ─────────────────────────────────────────────────────────────────────────────

/// A pagination position over a stream of `K`-keyed results.
///
/// `K` is the keyset key (e.g. `(Score, SymbolId)`), which must
/// round-trip through `postcard` for the opaque token encoding.
///
/// `P` is the [`SnapshotPolicy`]:
/// - [`Enforced`]: freshness was checked at construction; holding one is proof.
///   Text search mints these.
/// - [`Advisory`]: snapshot is carried as a hint only; ANN indices cannot
///   enforce it cheaply. Semantic search uses this policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor<K, P: SnapshotPolicy = Advisory> {
    /// The last key returned; the next page resumes strictly after it.
    pub after: K,

    /// The index snapshot (content hash) this cursor is anchored to. For
    /// [`Enforced`] cursors this was verified against the live snapshot at
    /// construction time. For [`Advisory`] cursors it is a best-effort hint.
    pub snapshot: ContentHash,

    /// Zero-sized policy brand — carries the freshness guarantee in the type
    /// without any runtime cost.
    #[serde(skip)]
    _policy: PhantomData<fn() -> P>,
}

impl<K, P: SnapshotPolicy> Cursor<K, P>
where
    K: Serialize + for<'de> Deserialize<'de>,
{
    /// Encode to an opaque base64url token for the wire.
    pub fn encode(&self) -> String {
        let bytes = postcard::to_allocvec(self)
            .expect("cursor keys are plain data and serialize infallibly");
        data_encoding::BASE64URL_NOPAD.encode(&bytes)
    }

    /// Decode an opaque token back into a cursor.
    pub fn decode(token: &str) -> Result<Self, CursorError> {
        let bytes = data_encoding::BASE64URL_NOPAD.decode(token.as_bytes())?;
        Ok(postcard::from_bytes::<Self>(&bytes)?)
    }
}

impl<K> Cursor<K, Advisory>
where
    K: Serialize + for<'de> Deserialize<'de>,
{
    /// Create an advisory cursor anchored at `after` within `snapshot`.
    ///
    /// The snapshot is carried as a hint; no freshness check is performed at
    /// construction. Use for eventually-consistent backends (e.g. Qdrant ANN).
    pub fn new(after: K, snapshot: ContentHash) -> Self {
        Self { after, snapshot, _policy: PhantomData }
    }
}

impl<K> Cursor<K, Enforced>
where
    K: Serialize + for<'de> Deserialize<'de>,
{
    /// Mint an enforced cursor.
    ///
    /// **Precondition (enforced by convention, not the compiler):** the caller
    /// must have just verified that `snapshot` equals the live index snapshot.
    /// Holding the returned `Cursor<K, Enforced>` is then a type-level claim
    /// that freshness was checked at construction — the burden of proof lives
    /// in the type, not in a comment buried inside the paginator.
    ///
    /// Only backends that can cheaply compute a content hash of the live index
    /// (e.g. tantivy) should mint enforced cursors.
    pub fn mint_enforced(after: K, snapshot: ContentHash) -> Self {
        Self { after, snapshot, _policy: PhantomData }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Why an opaque cursor token could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    /// The token was not valid base64url.
    #[error("malformed cursor encoding")]
    Encoding(#[from] data_encoding::DecodeError),
    /// The decoded bytes did not match the expected keyset shape.
    #[error("malformed cursor payload")]
    Payload(#[from] postcard::Error),
    /// The cursor's snapshot does not match the current index snapshot; the
    /// caller should discard this cursor and restart from the first page.
    #[error("cursor snapshot is stale; restart pagination from the first page")]
    StaleSnapshot,
}
