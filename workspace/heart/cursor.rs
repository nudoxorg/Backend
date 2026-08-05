use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot policy — sealed, so only the two provided implementors exist.
// ─────────────────────────────────────────────────────────────────────────────

mod private {
    pub trait Sealed {}
}

/// The policy discriminant carried **in the encoded payload**, so a token's
/// brand survives the wire. `encode` stamps `P::TAG`; `decode` checks the tag it
/// reads back against the `P` the caller asks for and rejects a mismatch with
/// [`CursorError::PolicyMismatch`]. Without this, the [`SnapshotPolicy`] brand
/// (a zero-sized `PhantomData`) would be erased on encode and a token minted
/// `Advisory` could be silently decoded as `Enforced`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyTag {
    /// Anchored by an [`Enforced`] cursor.
    Enforced,
    /// Anchored by an [`Advisory`] cursor.
    Advisory,
}

/// Whether a [`Cursor`]'s snapshot is **enforced** at construction time or only
/// carried as an advisory hint.
///
/// Only [`Enforced`] and [`Advisory`] implement this trait; the sealed module
/// prevents third-party impls. Each implementor names its wire [`PolicyTag`] via
/// [`SnapshotPolicy::TAG`], which is what makes the brand survive encoding.
pub trait SnapshotPolicy: private::Sealed + std::fmt::Debug + Clone + PartialEq + Eq {
    /// The discriminant this policy writes into (and is checked against on) the
    /// encoded token, so the brand cannot be silently reinterpreted on decode.
    const TAG: PolicyTag;
}

/// The snapshot is **checked against the live index** — both at construction
/// *and* again at decode. Holding a `Cursor<K, Enforced>` is proof that the
/// cursor's snapshot matched the live snapshot at the moment it was obtained.
/// Text search uses this policy.
///
/// Constructed only via [`Cursor::mint_enforced`] (after the mint site verifies
/// the live snapshot matches) or via [`Cursor::<K, Enforced>::decode`], which
/// *requires the caller to pass the live snapshot* and re-verifies freshness —
/// so an `Enforced` cursor can never be conjured from an untrusted token without
/// re-proving freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Enforced {}

/// The snapshot is **carried but not enforced** — an eventually-consistent
/// backend (e.g. Qdrant ANN) has no cheap content hash, so completeness is
/// best-effort. The policy is explicit in every call site's type signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Advisory {}

impl private::Sealed for Enforced {}
impl private::Sealed for Advisory {}
impl SnapshotPolicy for Enforced {
    const TAG: PolicyTag = PolicyTag::Enforced;
}
impl SnapshotPolicy for Advisory {
    const TAG: PolicyTag = PolicyTag::Advisory;
}

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

    /// Zero-sized policy brand — carries the freshness guarantee in the type.
    /// The brand itself is not serialized (it is zero-sized and generic); the
    /// wire instead carries an explicit [`PolicyTag`] on the [`Wire`] envelope so
    /// the brand survives encoding and is re-checked on decode.
    #[serde(skip)]
    _policy: PhantomData<fn() -> P>,
}

/// The on-the-wire envelope for a [`Cursor`]. Distinct from `Cursor` itself so
/// the [`PolicyTag`] discriminant is *serialized* (the `Cursor`'s policy lives
/// only in the erased `PhantomData` brand). `encode` builds one of these from a
/// `Cursor<K, P>` stamping `P::TAG`; `decode` reconstructs it and verifies the
/// tag against the target `P` before handing back a branded `Cursor`.
#[derive(Debug, Serialize, Deserialize)]
struct Wire<K> {
    after: K,
    snapshot: ContentHash,
    /// The policy brand, carried explicitly so it cannot be erased on the wire.
    policy: PolicyTag,
}

impl<K, P: SnapshotPolicy> Cursor<K, P>
where
    K: Serialize + for<'de> Deserialize<'de>,
{
    /// Encode to an opaque base64url token for the wire.
    ///
    /// The token carries `P::TAG`, so a cursor minted under one policy cannot be
    /// silently decoded under another — [`Cursor::decode_tagged`] rejects a tag
    /// mismatch with [`CursorError::PolicyMismatch`].
    pub fn encode(&self) -> String {
        let wire = Wire {
            after: &self.after,
            snapshot: self.snapshot,
            policy: P::TAG,
        };
        let bytes = postcard::to_allocvec(&wire)
            .expect("cursor keys are plain data and serialize infallibly");
        data_encoding::BASE64URL_NOPAD.encode(&bytes)
    }

    /// Decode an opaque token into a cursor of policy `P`, verifying that the
    /// token's serialized [`PolicyTag`] matches `P::TAG`.
    ///
    /// This is the shared, tag-checking core. It does **not** perform a freshness
    /// check — that is why it is crate-private and why [`Enforced`] does not
    /// expose it directly. The public entry points are:
    /// - [`Cursor::<K, Advisory>::decode`] — tag check only (best-effort snapshot).
    /// - [`Cursor::<K, Enforced>::decode`] — tag check **and** a mandatory live
    ///   snapshot re-verification, so an `Enforced` cursor cannot be obtained
    ///   from a token without re-proving freshness.
    fn decode_tagged(token: &str) -> Result<Self, CursorError> {
        let bytes = data_encoding::BASE64URL_NOPAD.decode(token.as_bytes())?;
        let wire: Wire<K> = postcard::from_bytes(&bytes)?;
        if wire.policy != P::TAG {
            return Err(CursorError::PolicyMismatch {
                expected: P::TAG,
                found: wire.policy,
            });
        }
        Ok(Self {
            after: wire.after,
            snapshot: wire.snapshot,
            _policy: PhantomData,
        })
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
        Self {
            after,
            snapshot,
            _policy: PhantomData,
        }
    }

    /// Decode an opaque token into an [`Advisory`] cursor.
    ///
    /// Verifies the token was minted `Advisory` (rejecting an [`Enforced`] token
    /// with [`CursorError::PolicyMismatch`]). No freshness check is performed —
    /// advisory snapshots are best-effort by definition.
    pub fn decode(token: &str) -> Result<Self, CursorError> {
        Self::decode_tagged(token)
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
        Self {
            after,
            snapshot,
            _policy: PhantomData,
        }
    }

    /// Decode an opaque token into an [`Enforced`] cursor, re-proving freshness.
    ///
    /// Unlike the [`Advisory`] path, this **requires the caller to pass the live
    /// snapshot** and re-verifies `decoded.snapshot == live`, returning
    /// [`CursorError::StaleSnapshot`] if the index has moved. This is what makes
    /// the `Enforced` brand honest across the wire: the brand is erased on
    /// encode, so a decoded `Enforced` cursor would otherwise be a freshness
    /// claim nobody re-checked. By funnelling every `Enforced` decode through a
    /// mandatory live-snapshot comparison, holding a decoded `Cursor<K, Enforced>`
    /// is again proof that freshness held at the moment it was obtained.
    ///
    /// Also verifies the token was minted `Enforced` (an [`Advisory`] token is
    /// rejected with [`CursorError::PolicyMismatch`] *before* the freshness
    /// check), so an advisory token cannot be laundered into an enforced cursor.
    pub fn decode(token: &str, live: ContentHash) -> Result<Self, CursorError> {
        let cursor = Self::decode_tagged(token)?;
        if cursor.snapshot != live {
            return Err(CursorError::StaleSnapshot);
        }
        Ok(cursor)
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
    /// The token was minted under one [`SnapshotPolicy`] but decoded as another
    /// (e.g. an [`Advisory`] token decoded as [`Enforced`]). The brand is carried
    /// explicitly on the wire precisely so this mismatch is caught rather than
    /// silently reinterpreted.
    #[error("cursor policy mismatch: token is {found:?}, expected {expected:?}")]
    PolicyMismatch {
        expected: PolicyTag,
        found: PolicyTag,
    },
    /// The cursor's snapshot does not match the current index snapshot; the
    /// caller should discard this cursor and restart from the first page.
    /// Surfaced by [`Cursor::<K, Enforced>::decode`]'s mandatory live-snapshot
    /// re-verification.
    #[error("cursor snapshot is stale; restart pagination from the first page")]
    StaleSnapshot,
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    type Key = (u64, u32);

    fn snap(byte: u8) -> ContentHash {
        ContentHash::of_bytes(&[byte])
    }

    /// A matching snapshot round-trips: encode `Enforced`, decode `Enforced`
    /// against the same live snapshot, and recover the original cursor.
    #[test]
    fn enforced_matching_snapshot_round_trips() {
        let live = snap(1);
        let cursor = Cursor::<Key, Enforced>::mint_enforced((7, 3), live);
        let token = cursor.encode();

        let decoded =
            Cursor::<Key, Enforced>::decode(&token, live).expect("matching snapshot must decode");
        assert_eq!(decoded.after, (7, 3));
        assert_eq!(decoded.snapshot, live);
    }

    /// An `Advisory` token must NOT decode as `Enforced`: the policy tag on the
    /// wire catches the mismatch before any freshness check runs.
    #[test]
    fn advisory_token_fails_to_decode_as_enforced() {
        let live = snap(1);
        let advisory = Cursor::<Key, Advisory>::new((7, 3), live);
        let token = advisory.encode();

        // Even with a *matching* live snapshot, the tag mismatch is fatal.
        let err = Cursor::<Key, Enforced>::decode(&token, live)
            .expect_err("advisory token must not decode as enforced");
        assert!(
            matches!(
                err,
                CursorError::PolicyMismatch {
                    expected: PolicyTag::Enforced,
                    found: PolicyTag::Advisory
                }
            ),
            "expected PolicyMismatch, got {err:?}"
        );
    }

    /// Symmetrically, an `Enforced` token must not decode as `Advisory`.
    #[test]
    fn enforced_token_fails_to_decode_as_advisory() {
        let live = snap(1);
        let enforced = Cursor::<Key, Enforced>::mint_enforced((7, 3), live);
        let token = enforced.encode();

        let err = Cursor::<Key, Advisory>::decode(&token)
            .expect_err("enforced token must not decode as advisory");
        assert!(
            matches!(
                err,
                CursorError::PolicyMismatch {
                    expected: PolicyTag::Advisory,
                    found: PolicyTag::Enforced
                }
            ),
            "expected PolicyMismatch, got {err:?}"
        );
    }

    /// `Enforced::decode` with a mismatched live snapshot yields `StaleSnapshot`,
    /// so the freshness re-check is unavoidable at decode.
    #[test]
    fn enforced_mismatched_snapshot_is_stale() {
        let minted_at = snap(1);
        let cursor = Cursor::<Key, Enforced>::mint_enforced((7, 3), minted_at);
        let token = cursor.encode();

        // The live snapshot moved since the token was minted.
        let live = snap(2);
        let err = Cursor::<Key, Enforced>::decode(&token, live)
            .expect_err("moved snapshot must be rejected");
        assert!(
            matches!(err, CursorError::StaleSnapshot),
            "expected StaleSnapshot, got {err:?}"
        );
    }

    /// An `Advisory` cursor round-trips (tag matches, no freshness check).
    #[test]
    fn advisory_round_trips() {
        let cursor = Cursor::<Key, Advisory>::new((9, 4), snap(5));
        let token = cursor.encode();
        let decoded = Cursor::<Key, Advisory>::decode(&token).expect("advisory token must decode");
        assert_eq!(decoded.after, (9, 4));
    }
}
