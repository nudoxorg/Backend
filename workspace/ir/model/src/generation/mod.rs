//! **`GenerationRoot`** — the plan's keystone type (`docs/IR-STORAGE-PLAN.md`
//! §1, §5, §6, phase P2). Full design rationale and the pinned specification
//! live in `tests/generation_root.rs`'s module doc; this comment covers the
//! implementation choices that doc leaves to the code.
//!
//! # What this replaces
//!
//! Today a package's IR is stored as one opaque blob — `IrSnapshot { package,
//! declarations: Vec<(IntroId, Entry, Option<IntroId>)> }`
//! (`workspace/nudox-engine/src/store/remote.rs:47-70`), serialized whole and
//! addressed by a single hash. `GenerationRoot` replaces that with a sorted
//! list of `(IntroId, ContentBlake3)` pairs plus the per-generation location
//! of each entry — simultaneously the storage index, the diff basis, the
//! fault-in manifest, and the reuse key.
//!
//! # Moved is not changed
//!
//! [`entry_storage_hash`](crate::content::entry_storage_hash)
//! (`content/mod.rs:471`) is deliberately **position-independent**: it
//! excludes `sym.source`/`sym.span`, and (per the doc at `content/mod.rs:403-405`)
//! already excludes the `Node`'s parent/children edges. Task #17 measured that
//! including position inflated apparent churn 58× on a real package pair
//! (75.6% "modified" vs 1.3%), entirely from declarations shifting position
//! without changing. Location and the parent edge are not thrown away by that
//! exclusion — they are **generation-scoped**, and this module is where they
//! live: on [`RootEntry::source`]/[`RootEntry::span`]/[`RootEntry::parent`].
//!
//! Consequently, [`GenerationRoot::diff`] classifies an entry whose `content`
//! (storage hash) is unchanged but whose `source`/`span`/`parent` differs as
//! an [`EntryChange`] in [`RootDiff::moved`], never [`RootDiff::changed`]. A
//! moved entry needs no refetch and no re-store — only its row in the root is
//! rewritten. Reporting it as `changed` would hand the fault-in path the
//! 75.6% figure and undo the entire measured win task #17 established.
//! `entry_storage_hash` itself is not touched by this module.
//!
//! # Canonicity is correctness
//!
//! The root hash is a package's identity, so [`GenerationRoot::encode`] must
//! be **bijective**: the same logical root must always produce the same
//! bytes (regardless of the order entries were supplied to
//! [`GenerationRoot::build`]), and no two distinct byte strings may decode to
//! the same logical root. The second half of that contract is enforced on
//! [`GenerationRoot::decode`], not merely produced by `encode` — a decoder
//! that silently accepted and re-sorted a non-canonical encoding would let
//! two byte strings share one hash preimage, which is exactly the failure
//! mode this type exists to rule out. See the "Canonicity enforced on the way
//! *in*" tests in `tests/generation_root.rs`.
//!
//! [`GenerationRoot::encode_unchecked`] exists **only** so tests can produce
//! a deliberately non-canonical byte string (e.g. entries in reverse order,
//! or a duplicate `IntroId`) to prove `decode` rejects it. It must never be
//! used to persist a root: unlike [`GenerationRoot::encode`], it serializes
//! `entries` in whatever order the field currently holds, without sorting a
//! defensive copy first. `encode` is the only encoder whose output `decode`
//! is guaranteed to accept.
//!
//! # Wire format
//!
//! Hand-rolled, in the same style as `content/mod.rs`'s hash preimages and
//! `codec.rs`'s envelope (length-prefixed strings, fixed-width little-endian
//! integers, an explicit version tag) rather than `serde_json` — field order
//! and float/escape handling in JSON are not canonical, and this format needs
//! to be. Layout:
//!
//! ```text
//! u16le(version)
//! encode_str(ecosystem) || encode_str(name)      -- PackageLineageId
//! u32le(entry_count)
//! entry_count × {
//!     32 bytes                                    -- intro (IntroId)
//!     32 bytes                                    -- content (ContentBlake3)
//!     0x00 | 0x01 ++ 32 bytes                      -- parent (Option<IntroId>)
//!     encode_str(source.to_string_lossy())         -- source
//!     u64le(span.start) || u64le(span.end)         -- span
//! }
//! ```
//!
//! [`GenerationRoot::hash`] is `ContentBlake3::from_domain(GENERATION_ROOT_DOMAIN,
//! &self.encode())` — a domain-separated digest over exactly the canonical
//! bytes `decode` requires, using [`GENERATION_ROOT_DOMAIN`], a fresh
//! constant distinct from [`crate::content::ENTRY_CONTENT_DOMAIN`] and
//! [`crate::content::ENTRY_STORAGE_DOMAIN`] (never a reuse — see those
//! domains' own doc comments for why sibling hash planes must never share a
//! domain string).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;

use crate::change::encode::{encode_str, write_u16le, write_u32le, write_u64le};
use crate::change::{ContentBlake3, EcosystemId, IntroId, PackageLineageId, PackageName};

/// Domain tag for [`GenerationRoot::hash`] preimages.
///
/// A sibling of [`crate::content::ENTRY_CONTENT_DOMAIN`] and
/// [`crate::content::ENTRY_STORAGE_DOMAIN`] — a distinct plane with its own
/// wire format and its own evolution, never reusing either of theirs. `v1`
/// because this is the first version of the root's wire format.
pub const GENERATION_ROOT_DOMAIN: &str = "nudox.generation.root.v1";

/// Wire-format version written at the start of every [`GenerationRoot::encode`]
/// (and [`GenerationRoot::encode_unchecked`]) output. [`GenerationRoot::decode`]
/// rejects any other value via
/// [`GenerationRootDecodeError::UnsupportedVersion`].
const GENERATION_ROOT_WIRE_VERSION: u16 = 1;

// ---------------------------------------------------------------------------
// RootEntry
// ---------------------------------------------------------------------------

/// One row of a [`GenerationRoot`]: a declaration's stable identity, its
/// position-independent content hash, and the location/tree-edge data that
/// only makes sense *for this generation*.
///
/// # Why `content` is a storage hash, not a content hash
///
/// `content` must be produced by [`crate::content::entry_storage_hash`],
/// never [`crate::content::entry_content_hash`]. The latter folds in
/// `sym.source`/`sym.span`, which would make `content` change whenever a
/// declaration merely moved — defeating the entire point of carrying
/// `source`/`span`/`parent` separately on this row. See the module
/// documentation's "Moved is not changed" section.
///
/// # Why `parent` lives here, not on the payload
///
/// `entry_storage_hash` already excludes the `Node`'s parent/children edges
/// (`content/mod.rs:403-405`) — tree structure is relational, not content.
/// This settles `docs/IR-STORAGE-PLAN.md`'s open question 3 ("where do
/// parent edges live?") in the root's favour: reparenting a declaration is a
/// move, not a content change, so its edge belongs beside its location,
/// which is exactly what this struct is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootEntry {
    /// Stable identity of the declaration this row describes. The primary
    /// key of a [`GenerationRoot`]: [`GenerationRoot::entries`] must contain
    /// at most one row per distinct `intro`, enforced by
    /// [`GenerationRoot::decode`].
    pub intro: IntroId,
    /// Position-independent storage identity of the declaration's payload —
    /// [`crate::content::entry_storage_hash`] of the current `Entry`. Two
    /// rows (in the same root or in different roots) with equal `content`
    /// share exactly the same stored payload; this is the field
    /// [`GenerationRoot::missing`] fault-checks and dedups on.
    pub content: ContentBlake3,
    /// This generation's parent edge, or `None` at the package root. Not
    /// folded into `content` — see the struct documentation.
    pub parent: Option<IntroId>,
    /// This generation's source file for the declaration. Not folded into
    /// `content` — a pure move changes this field (and/or `span`) and
    /// nothing else.
    pub source: PathBuf,
    /// This generation's byte span within `source`. Not folded into
    /// `content`, for the same reason as `source`.
    pub span: Range<usize>,
}

// ---------------------------------------------------------------------------
// GenerationRoot
// ---------------------------------------------------------------------------

/// A package's IR as a sorted list of `(IntroId, ContentBlake3)` rows plus
/// per-generation location — the storage index, diff basis, fault-in
/// manifest, and reuse key for one generation of one package. See the module
/// documentation for the full design rationale.
///
/// `entries` is `pub` so tests can build deliberately-invalid fixtures (see
/// [`GenerationRoot::encode_unchecked`]); production callers should prefer
/// [`GenerationRoot::build`] over constructing this struct directly, since
/// `build` is what establishes the canonical order every other method
/// assumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRoot {
    /// The package lineage this root belongs to.
    pub package: PackageLineageId,
    /// Canonically-ordered rows: strictly ascending by `intro`, no
    /// duplicates, after construction via [`GenerationRoot::build`] or
    /// [`GenerationRoot::decode`]. Methods that rely on this order
    /// (`entry`, `diff`, `missing`) document that dependency; `encode`
    /// defensively re-sorts a copy rather than trusting the field, since
    /// tests are free to mutate `entries` directly (e.g. to build invalid
    /// fixtures for `decode`).
    pub entries: Vec<RootEntry>,
}

impl GenerationRoot {
    /// Build a root from an unordered list of rows, canonicalising their
    /// order (ascending by `intro`) so that two producers who walked the
    /// same package in different orders converge on one encoding and one
    /// hash. See `build_canonicalises_entry_order` in
    /// `tests/generation_root.rs` — the test this method exists to satisfy.
    pub fn build(package: PackageLineageId, mut entries: Vec<RootEntry>) -> Self {
        entries.sort_by_key(|e| e.intro);
        Self { package, entries }
    }

    /// Domain-separated digest over [`GenerationRoot::encode`]'s canonical
    /// bytes. Two roots with the same logical content — regardless of the
    /// order their rows were supplied in — produce the same hash; this is
    /// what makes "has this package changed" a single comparison.
    pub fn hash(&self) -> ContentBlake3 {
        ContentBlake3::from_domain(GENERATION_ROOT_DOMAIN, &self.encode())
    }

    /// Canonical wire bytes: `entries` sorted ascending by `intro`
    /// regardless of the field's current order, so `encode` always produces
    /// bytes [`GenerationRoot::decode`] accepts. See the module
    /// documentation's "Wire format" section for the exact layout.
    pub fn encode(&self) -> Vec<u8> {
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|e| e.intro);
        Self::encode_fields(&self.package, &sorted)
    }

    /// The same field layout as [`GenerationRoot::encode`], **without**
    /// canonicalising `entries` first — serializes them in whatever order
    /// the field currently holds.
    ///
    /// This exists solely so tests can produce a deliberately non-canonical
    /// encoding (unsorted rows, or two rows sharing one `IntroId`) and
    /// assert that [`GenerationRoot::decode`] rejects it. Using this to
    /// persist a root would defeat the entire canonicity contract: `decode`
    /// is only guaranteed to accept output from `encode`. Production code
    /// must never call this.
    pub fn encode_unchecked(&self) -> Vec<u8> {
        Self::encode_fields(&self.package, &self.entries)
    }

    fn encode_fields(package: &PackageLineageId, entries: &[RootEntry]) -> Vec<u8> {
        let mut out = Vec::new();
        write_u16le(&mut out, GENERATION_ROOT_WIRE_VERSION);
        package.encode(&mut out);
        write_u32le(
            &mut out,
            u32::try_from(entries.len()).expect("entry count exceeds u32::MAX"),
        );
        for e in entries {
            out.extend_from_slice(e.intro.as_bytes());
            out.extend_from_slice(e.content.as_bytes());
            match &e.parent {
                None => out.push(0x00),
                Some(p) => {
                    out.push(0x01);
                    out.extend_from_slice(p.as_bytes());
                }
            }
            encode_str(&mut out, &e.source.to_string_lossy());
            write_u64le(&mut out, e.span.start as u64);
            write_u64le(&mut out, e.span.end as u64);
        }
        out
    }

    /// Decode wire bytes produced by [`GenerationRoot::encode`], rejecting
    /// anything that is not itself the unique canonical encoding of some
    /// logical root: truncated input, an unsupported version tag, trailing
    /// bytes past the last declared entry, entries not in strictly ascending
    /// `intro` order, or two entries sharing one `intro`. See the module
    /// documentation's "Canonicity is correctness" section for why this
    /// validation happens here rather than being left to callers.
    pub fn decode(bytes: &[u8]) -> Result<Self, GenerationRootDecodeError> {
        let mut pos = 0usize;

        let version = read_u16le(bytes, &mut pos)?;
        if version != GENERATION_ROOT_WIRE_VERSION {
            return Err(GenerationRootDecodeError::UnsupportedVersion {
                expected: GENERATION_ROOT_WIRE_VERSION,
                got: version,
            });
        }

        let ecosystem = read_str(bytes, &mut pos)?;
        let name = read_str(bytes, &mut pos)?;
        let package = PackageLineageId::new(EcosystemId::new(ecosystem), PackageName::new(name));

        let count = read_u32le(bytes, &mut pos)? as usize;
        let mut entries = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let intro = IntroId::from_raw(read_bytes32(bytes, &mut pos)?);
            let content = ContentBlake3::from_raw(read_bytes32(bytes, &mut pos)?);
            let parent_tag = read_u8(bytes, &mut pos)?;
            let parent = match parent_tag {
                0x00 => None,
                0x01 => Some(IntroId::from_raw(read_bytes32(bytes, &mut pos)?)),
                other => return Err(GenerationRootDecodeError::InvalidTag(other)),
            };
            let source = PathBuf::from(read_str(bytes, &mut pos)?);
            let start = read_u64le(bytes, &mut pos)? as usize;
            let end = read_u64le(bytes, &mut pos)? as usize;
            entries.push(RootEntry {
                intro,
                content,
                parent,
                source,
                span: start..end,
            });
        }

        // Truncation must be an error, not a partial-but-plausible root: a
        // stream that has extra bytes past the declared entry count is just
        // as much a framing violation as one that runs out early, since it
        // means these bytes are not *the* canonical encoding of `entries`.
        if pos != bytes.len() {
            return Err(GenerationRootDecodeError::TrailingBytes);
        }

        // Canonicity, enforced on the way in (module doc, "Canonicity is
        // correctness"): entries must already be strictly ascending by
        // `intro`. A non-strict adjacent pair is either an out-of-order
        // encoding or a duplicate `IntroId`; either way, this byte string is
        // not the unique canonical encoding of a logical root and must be
        // rejected rather than silently accepted and re-sorted.
        for pair in entries.windows(2) {
            if pair[0].intro >= pair[1].intro {
                return Err(GenerationRootDecodeError::NotCanonical);
            }
        }

        Ok(Self { package, entries })
    }

    /// Look up a row by its stable identity. `O(log n)` via binary search,
    /// relying on `entries` being canonically sorted — true for any root
    /// produced by [`GenerationRoot::build`] or [`GenerationRoot::decode`].
    pub fn entry(&self, intro: IntroId) -> Option<&RootEntry> {
        self.entries
            .binary_search_by(|e| e.intro.cmp(&intro))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Linear merge of two canonically-sorted roots into
    /// added/removed/changed/moved — `O(n + m)`, walking both entry lists
    /// once each like a merge-sort merge step, never a nested loop. Relies
    /// on both `old.entries` and `new.entries` being in canonical order,
    /// which holds for any root produced by `build` or `decode`.
    ///
    /// The classification that matters (module doc, "Moved is not changed"):
    /// a row present in both roots under the same `intro` is `changed` only
    /// if `content` (the storage hash) differs; if `content` is equal but
    /// `source`/`span`/`parent` differ, it is `moved`, never `changed`. A row
    /// identical on every field is reported nowhere — the plan's entire
    /// economy rests on the unchanged remainder costing nothing.
    pub fn diff(old: &GenerationRoot, new: &GenerationRoot) -> RootDiff {
        debug_assert!(
            is_canonically_sorted(&old.entries),
            "GenerationRoot::diff requires a canonically-sorted `old` root; \
             pass one produced by `build` or `decode`, not a hand-mutated one"
        );
        debug_assert!(
            is_canonically_sorted(&new.entries),
            "GenerationRoot::diff requires a canonically-sorted `new` root; \
             pass one produced by `build` or `decode`, not a hand-mutated one"
        );

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        let mut moved = Vec::new();

        let mut i = 0usize;
        let mut j = 0usize;
        while i < old.entries.len() && j < new.entries.len() {
            let o = &old.entries[i];
            let n = &new.entries[j];
            match o.intro.cmp(&n.intro) {
                Ordering::Less => {
                    removed.push(EntryChange::new(o));
                    i += 1;
                }
                Ordering::Greater => {
                    added.push(EntryChange::new(n));
                    j += 1;
                }
                Ordering::Equal => {
                    if o.content != n.content {
                        changed.push(EntryChange::new(n));
                    } else if o.parent != n.parent || o.source != n.source || o.span != n.span {
                        moved.push(EntryChange::new(n));
                    }
                    // Else: every field equal — unchanged, reported nowhere.
                    i += 1;
                    j += 1;
                }
            }
        }
        removed.extend(old.entries[i..].iter().map(EntryChange::new));
        added.extend(new.entries[j..].iter().map(EntryChange::new));

        RootDiff {
            added,
            removed,
            changed,
            moved,
        }
    }

    /// The content hashes this root references that `held` reports the
    /// caller does not already have, deduplicated, in canonical (`intro`)
    /// order. This is the fault-in manifest (`docs/IR-STORAGE-PLAN.md` P6):
    /// given a local store's membership test, which payloads must actually
    /// be fetched?
    ///
    /// Deduplication matters because storage identity is content-addressed:
    /// two declarations with identical bodies share one
    /// `entry_storage_hash`, so a manifest that requested it once per
    /// `IntroId` would pay for the duplication content-addressing exists to
    /// eliminate.
    pub fn missing(&self, held: impl Fn(&ContentBlake3) -> bool) -> Vec<ContentBlake3> {
        let mut seen = HashSet::with_capacity(self.entries.len());
        let mut out = Vec::new();
        for e in &self.entries {
            if held(&e.content) {
                continue;
            }
            if seen.insert(e.content) {
                out.push(e.content);
            }
        }
        out
    }
}

/// `true` if `entries` is strictly ascending by `intro` with no duplicates —
/// the invariant [`GenerationRoot::build`] establishes and
/// [`GenerationRoot::decode`] validates. Used only as a `debug_assert!`
/// precondition inside [`GenerationRoot::diff`]; not a public API, since
/// callers should rely on construction to guarantee this rather than
/// checking it themselves.
fn is_canonically_sorted(entries: &[RootEntry]) -> bool {
    entries.windows(2).all(|pair| pair[0].intro < pair[1].intro)
}

// ---------------------------------------------------------------------------
// Diff output
// ---------------------------------------------------------------------------

/// One classified difference between two generations of a [`GenerationRoot`],
/// as produced by [`GenerationRoot::diff`]. Carries the changed row's
/// snapshot from whichever side is informative for that classification: the
/// `new` root's row for `added`/`changed`/`moved` (what a caller should fetch
/// or record now), the `old` root's row for `removed` (the last known
/// snapshot of what is going away).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryChange {
    intro: IntroId,
    entry: RootEntry,
}

impl EntryChange {
    fn new(entry: &RootEntry) -> Self {
        Self {
            intro: entry.intro,
            entry: entry.clone(),
        }
    }

    /// Stable identity of the entry this change describes.
    ///
    /// Takes `&self` and returns an owned `IntroId` (not `&IntroId`) so that
    /// `EntryChange::intro` can be passed directly as a function reference to
    /// `Iterator::map` — `changes.iter().map(EntryChange::intro)` — which is
    /// how `tests/generation_root.rs` uses it.
    pub fn intro(&self) -> IntroId {
        self.intro
    }

    /// The row snapshot this change carries. See the struct documentation
    /// for which root (`old` vs `new`) it is drawn from.
    pub fn entry(&self) -> &RootEntry {
        &self.entry
    }
}

/// The result of [`GenerationRoot::diff`]: two generations' rows classified
/// into added/removed/changed/moved. See the module documentation's "Moved
/// is not changed" section for why `changed` and `moved` are kept separate
/// rather than folded into one "modified" bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RootDiff {
    added: Vec<EntryChange>,
    removed: Vec<EntryChange>,
    changed: Vec<EntryChange>,
    moved: Vec<EntryChange>,
}

impl RootDiff {
    /// Entries present in `new` but not `old`.
    pub fn added(&self) -> &[EntryChange] {
        &self.added
    }

    /// Entries present in `old` but not `new`.
    pub fn removed(&self) -> &[EntryChange] {
        &self.removed
    }

    /// Entries present in both roots whose storage-hash `content` differs —
    /// a real payload edit, requiring refetch/re-store.
    pub fn changed(&self) -> &[EntryChange] {
        &self.changed
    }

    /// Entries present in both roots with equal `content` but a different
    /// `source`, `span`, or `parent` — a pure relocation or reparent,
    /// requiring only a root rewrite. See the module documentation's "Moved
    /// is not changed" section.
    pub fn moved(&self) -> &[EntryChange] {
        &self.moved
    }

    /// `true` if no entry was added, removed, changed, or moved — the two
    /// generations are identical.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.moved.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Decode errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong decoding [`GenerationRoot::encode`] output.
/// Every variant corresponds to a way the input bytes fail to be *the*
/// unique canonical encoding of some logical root — see the module
/// documentation's "Canonicity is correctness" section.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationRootDecodeError {
    /// The input ended before a length-prefixed field, a fixed-width
    /// integer, or a declared entry could be fully read. The storage
    /// analogue of the wire decoder rule that a stream stopping early is
    /// truncation, never a small success.
    #[error("generation root bytes truncated while decoding")]
    Truncated,
    /// The wire-format version tag did not match
    /// [`GENERATION_ROOT_WIRE_VERSION`].
    #[error("unsupported generation root wire version: expected {expected}, got {got}")]
    UnsupportedVersion {
        /// The version this build of `nudox-ir` supports.
        expected: u16,
        /// The version actually present in the input.
        got: u16,
    },
    /// A string field's bytes were not valid UTF-8.
    #[error("generation root bytes contain invalid UTF-8 in a string field")]
    InvalidUtf8,
    /// The byte introducing `RootEntry::parent` was neither `0x00` (absent)
    /// nor `0x01` (present).
    #[error("generation root bytes contain an unrecognised parent-presence tag: {0:#x}")]
    InvalidTag(u8),
    /// Decoded entries are not in strictly ascending `intro` order, or two
    /// entries share one `intro`. Both failures are detected by the same
    /// adjacency check: a non-strict step is either an out-of-order pair or
    /// a duplicate. Accepting either would let two distinct byte strings
    /// decode to one logical root, breaking the root hash as an identity.
    #[error(
        "generation root entries are not in canonical (strictly ascending IntroId) order, \
         or contain a duplicate IntroId"
    )]
    NotCanonical,
    /// Bytes remained after the last declared entry was read. The declared
    /// entry count did not account for all the input, so these bytes are not
    /// the canonical encoding of any root.
    #[error("generation root bytes contain unconsumed trailing data")]
    TrailingBytes,
}

// ---------------------------------------------------------------------------
// Byte-cursor decode helpers
// ---------------------------------------------------------------------------
//
// A minimal hand-rolled cursor, mirroring `content/mod.rs`'s encode-side
// helpers on the way in: every read either advances `pos` and returns a
// value, or leaves `pos` unspecified and returns `Err(Truncated)`. There is
// no framework here because the wire format is small and fixed; introducing
// one would be more code to audit for the same guarantee these functions
// already provide directly.

fn take<'a>(
    bytes: &'a [u8],
    pos: &mut usize,
    n: usize,
) -> Result<&'a [u8], GenerationRootDecodeError> {
    let end = pos
        .checked_add(n)
        .ok_or(GenerationRootDecodeError::Truncated)?;
    let slice = bytes
        .get(*pos..end)
        .ok_or(GenerationRootDecodeError::Truncated)?;
    *pos = end;
    Ok(slice)
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8, GenerationRootDecodeError> {
    Ok(take(bytes, pos, 1)?[0])
}

fn read_u16le(bytes: &[u8], pos: &mut usize) -> Result<u16, GenerationRootDecodeError> {
    let s = take(bytes, pos, 2)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32le(bytes: &[u8], pos: &mut usize) -> Result<u32, GenerationRootDecodeError> {
    let s = take(bytes, pos, 4)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64le(bytes: &[u8], pos: &mut usize) -> Result<u64, GenerationRootDecodeError> {
    let s = take(bytes, pos, 8)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

fn read_bytes32(bytes: &[u8], pos: &mut usize) -> Result<[u8; 32], GenerationRootDecodeError> {
    let s = take(bytes, pos, 32)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(s);
    Ok(out)
}

fn read_str(bytes: &[u8], pos: &mut usize) -> Result<String, GenerationRootDecodeError> {
    let len = read_u32le(bytes, pos)? as usize;
    let s = take(bytes, pos, len)?;
    String::from_utf8(s.to_vec()).map_err(|_| GenerationRootDecodeError::InvalidUtf8)
}
