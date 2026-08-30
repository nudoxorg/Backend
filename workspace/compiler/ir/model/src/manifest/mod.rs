//! Generation identity, blob manifest, and outbox for a sealed
//! [`PristineIntroTable`].
//!
//! # Module overview
//!
//! This module provides three cooperating pieces:
//!
//! * **[`GenerationStamp`]** — the 32-byte BLAKE3 content identity of one
//!   sealed generation, covering both the intro *set* (which symbols are
//!   present) and each entry's *content hash* (what those symbols contain). Two
//!   generations that share the exact same set of declarations but have any
//!   body difference produce DIFFERENT stamps.
//!
//! * **[`BlobManifest`]** — the value that captures the source-file list, the
//!   IR archive CAS pointer, the VCS channel pin, and the toolchain descriptor
//!   for one sealed generation. It is the *input* to [`generation_stamp`]; the
//!   stamp is derived from it, not stored inside it.
//!
//! * **[`Outbox`] / [`InMemoryOutbox`]** — a staging seam between the sealing
//!   pass and downstream consumers (registry ingestion, search indexing,
//!   artifact publication). The outbox stores [`GenerationStamp`] values (Hash
//!   ①), never CAS addresses (Hash ②) — the distinct newtypes enforce this at
//!   compile time.
//!
//! # The two hash planes (K12 discipline)
//!
//! - **Hash ①** — [`GenerationStamp`]: the *logical generation identity*,
//!   derived from a canonical preimage of the manifest's content. This is what
//!   the outbox stores and what callers compare for equality.
//! - **Hash ②** — [`CasKey`]: the CAS address of the serialized manifest blob.
//!   Useful as a storage address, but NEVER a generation identity.
//!
//! These are distinct newtypes; passing one where the other is required is a
//! compile error.
//!
//! # heart-independence note
//!
//! nudox-ir must NOT depend on the `heart` crate (see the parallel note in
//! `body.rs` for `Language`). The old `workspace/ir` manifest used
//! `heart::Toolchain` and `heart::PackageId`. This module defines local,
//! minimal replacements ([`Toolchain`] and [`PackageUuid`]) and notes that
//! consumers convert at their own boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::{
    apply::PristineIntroTable,
    change::{IntroId, PackageLineageId, encode::write_u32le, hash::ContentBlake3},
};

// ============================================================================
// Local VCS-layer identity types
//
// CasKey, ChangeId, ChangeSetFingerprint, and ChannelName are intentionally
// deferred from `crate::change` (see its module doc). They are defined here
// so BlobManifest can carry them without pulling in the full VCS stack.
// ============================================================================

/// Content address of exact archive or blob bytes in the CAS (Hash ② domain).
///
/// This is a distinct newtype from [`GenerationStamp`] — the two must never be
/// confused (K12 discipline). Passing a `CasKey` where a `GenerationStamp` is
/// required is a compile error.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct CasKey(ContentBlake3);

impl CasKey {
    /// Wrap raw bytes (already computed).
    #[inline]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(ContentBlake3::from_raw(bytes))
    }

    /// `blake3(domain || preimage)` wrapped in this newtype.
    #[inline]
    pub fn from_domain(domain: &str, preimage: &[u8]) -> Self {
        Self(ContentBlake3::from_domain(domain, preimage))
    }

    /// The raw 32 bytes of the digest.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex representation (64 characters).
    #[inline]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Debug for CasKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cas:{}…", &self.to_hex()[..12])
    }
}

impl core::fmt::Display for CasKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Forever-stable identity of one change object; channel membership key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ChangeId(ContentBlake3);

impl ChangeId {
    /// Wrap raw bytes (already computed).
    #[inline]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(ContentBlake3::from_raw(bytes))
    }

    /// The raw 32 bytes of the digest.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex representation (64 characters).
    #[inline]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Debug for ChangeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "chg:{}…", &self.to_hex()[..12])
    }
}

/// A human-readable name for a pijul channel (e.g. `"main"`).
///
/// Channel names are UTF-8 strings; no length limit is enforced here, but
/// producers should keep them short and path-safe.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelName(String);

impl ChannelName {
    /// Wrap a string as a channel name.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The channel name as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for ChannelName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl core::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

// ============================================================================
// heart-independent types
// ============================================================================

/// Stable UUID identifying a package in the registry across all its
/// generations.
///
/// # heart-independence
///
/// `workspace/ir` uses `heart::PackageId` (a UUID newtype) in `BlobManifestV3`.
/// nudox-ir must NOT depend on `heart`. This is a minimal local replacement:
/// a 16-byte UUID stored as raw bytes. Consumers that cross the nudox-ir
/// boundary convert to/from `heart::PackageId` at their own seam, the same way
/// [`crate::body::Language`] is handled.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageUuid([u8; 16]);

impl PackageUuid {
    /// Wrap raw UUID bytes.
    #[inline]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The raw 16 bytes.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for PackageUuid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Render as hyphenated UUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx).
        let b = &self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_be_bytes([b[4], b[5]]),
            u16::from_be_bytes([b[6], b[7]]),
            u16::from_be_bytes([b[8], b[9]]),
            {
                let mut tail = [0u8; 8];
                tail[2..].copy_from_slice(&b[10..16]);
                u64::from_be_bytes(tail)
            }
        )
    }
}

/// Minimal toolchain descriptor: the producer's language runtime and its
/// version string.
///
/// # heart-independence
///
/// `workspace/ir` uses `heart::Toolchain` (a rich enum per language). nudox-ir
/// must NOT depend on `heart`. This is a minimal, stable replacement: a
/// free-form language tag plus a semver-ish version string. Consumers that
/// need `heart::Toolchain` convert at their boundary; the round-trip is
/// bijective for all cases `heart::Toolchain` currently supports.
///
/// # Stamp sensitivity
///
/// Both `language` and `version` contribute to the [`GenerationStamp`]
/// preimage. Changing either field produces a different stamp for an otherwise
/// identical generation.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Toolchain {
    /// The source language the producer ran against (e.g. `"rust"`,
    /// `"typescript"`).
    pub language: String,
    /// The compiler/runtime version string (e.g. `"1.80.0"`, `"5.5.4"`).
    pub version: String,
}

impl Toolchain {
    /// Construct a toolchain descriptor.
    pub fn new(language: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            version: version.into(),
        }
    }
}

impl core::fmt::Debug for Toolchain {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}/{}", self.language, self.version)
    }
}

// ============================================================================
// GenerationStamp
// ============================================================================

/// Domain tag for generation stamps — version 2.
///
/// Bumped from v1 because the preimage layout now folds in per-entry
/// content hashes in addition to the intro-id set. Stamps computed with the
/// old v1 domain are NOT comparable with v2 stamps even when the intro sets
/// are identical; the domain prefix makes them non-colliding by construction.
pub const GENERATION_DOMAIN: &str = "nudox.gen.v2";

/// Content-addressed identity of a sealed generation's intro set **and**
/// per-entry content.
///
/// # What this stamps
///
/// A `GenerationStamp` covers:
///
/// * The **set of [`IntroId`]s** present in the [`PristineIntroTable`] at seal
///   time — the *shape* of the generation.
/// * The **content hash** of every entry in that set, obtained by calling
///   `crate::content::entry_content_hash(entry)`. Two generations with the same
///   set of declarations but different bodies produce DIFFERENT stamps. This is
///   the headline fix over the v1 stub, which could not detect edits.
///
/// # Determinism and order independence
///
/// The stamp is **deterministic** and **order-independent**: two tables with
/// the same entries (by `IntroId` and content) always produce the same stamp,
/// regardless of insertion order. The implementation sorts contributions by
/// `IntroId` bytes before folding, so HashMap iteration order never reaches
/// the hash preimage.
///
/// # K12 — Hash ①/② discipline
///
/// A `GenerationStamp` is Hash ① — the *logical* generation identity. It is
/// NOT a CAS address ([`CasKey`], Hash ②) of any serialized blob. The
/// distinct newtype makes it a compile error to pass one where the other is
/// required.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(transparent)]
pub struct GenerationStamp(ContentBlake3);

impl GenerationStamp {
    /// Wrap a raw `ContentBlake3` (already computed).
    #[inline]
    pub const fn from_raw(inner: ContentBlake3) -> Self {
        Self(inner)
    }

    /// `blake3(domain || preimage)` wrapped in this newtype.
    ///
    /// Exposed for test helpers that construct fixture stamps without going
    /// through [`generation_stamp`]. Production callers should always use
    /// [`generation_stamp`].
    #[inline]
    pub fn from_domain(domain: &str, preimage: &[u8]) -> Self {
        Self(ContentBlake3::from_domain(domain, preimage))
    }

    /// Raw 32 bytes of the digest.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Lowercase hex representation (64 characters).
    #[inline]
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}

impl core::fmt::Display for GenerationStamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl core::fmt::Debug for GenerationStamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "gen:{}…", &self.to_hex()[..12])
    }
}

// ============================================================================
// generation_stamp — the headline fix
// ============================================================================

/// Compute the [`GenerationStamp`] for a sealed [`PristineIntroTable`].
///
/// # Algorithm (v2 — content-sensitive)
///
/// 1. Collect `(IntroId, content_hash)` pairs from the table, where
///    `content_hash = crate::content::entry_content_hash(entry)`.
/// 2. Sort the pairs by `IntroId` bytes (stable total order). **Determinism
///    requires explicit sorting here because the table is backed by a `HashMap`
///    whose iteration order is unspecified and not reproducible.**
/// 3. Build the preimage:
///    - `u32le(count)` — the number of entries.
///    - For each pair in sorted order: `intro_bytes[32] || content_hash[32]`.
/// 4. Return `GenerationStamp(ContentBlake3::from_domain(GENERATION_DOMAIN,
///    &preimage))`.
///
/// # Why v2?
///
/// The v1 implementation (domain `"nudox.gen.v1"`) folded only the set of
/// `IntroId`s into the preimage. Two generations with the same set of
/// declarations but completely different bodies produced the **same** stamp:
/// the stamp could not detect an edit. The v2 preimage folds in
/// `entry_content_hash` for every entry, making the stamp sensitive to body
/// changes.
///
/// # Order independence
///
/// The sort in step 2 guarantees that the same set of entries — regardless of
/// the order they were inserted into the `HashMap` — always produces the same
/// preimage and therefore the same stamp.
pub fn generation_stamp(table: &PristineIntroTable) -> GenerationStamp {
    // 1. Collect (IntroId, content_hash) pairs. entry_content_hash returns
    //    ContentBlake3 directly.
    let mut pairs: Vec<(IntroId, ContentBlake3)> = table
        .iter()
        .map(|(id, entry)| (id, crate::content::entry_content_hash(entry)))
        .collect();

    // 2. Sort by IntroId bytes. HashMap iteration is unspecified; sorting is
    //    mandatory for determinism. IntroId is Ord via ContentBlake3 ([u8;32]).
    pairs.sort_by_key(|(id, _)| *id.as_bytes());

    // 3. Build the preimage.
    let count = u32::try_from(pairs.len()).expect("intro count exceeds u32::MAX");
    let mut preimage = Vec::with_capacity(4 + pairs.len() * 64);
    write_u32le(&mut preimage, count);
    for (id, content_hash) in &pairs {
        preimage.extend_from_slice(id.as_bytes());
        preimage.extend_from_slice(content_hash.as_bytes());
    }

    // 4. Domain-hash and wrap.
    GenerationStamp(ContentBlake3::from_domain(GENERATION_DOMAIN, &preimage))
}

// ============================================================================
// BlobManifest
// ============================================================================

/// One source file inside a sealed generation.
///
/// `path` is the package-relative UTF-8 path (POSIX separators, no leading
/// `/`). `content_hash` is the [`CasKey`] of the raw file bytes.
///
/// File size is deliberately absent (matching the v3 design note in
/// `workspace/ir`): it is derivable from the blob and its inclusion would
/// couple the stamp to a field carrying no additional identity information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Package-relative UTF-8 path (POSIX separators, no leading `/`).
    pub path: String,
    /// CAS address of the file's raw byte content.
    pub content_hash: CasKey,
}

/// A pinned reference into a package's channel DAG at seal time.
///
/// `channel` names the channel (e.g. `"main"`). `tip_change` is the
/// [`ChangeId`] of the most-recently applied change (may be `None` for a
/// fresh channel with no changes). `change_log_cas` is an optional CAS
/// address for the serialized change-log blob.
///
/// The workspace/ir `ChangeSetFingerprint` field (`tip`) is intentionally
/// absent here: it is a derived value (hash of sorted change-id bytes) and
/// carrying it inside the manifest would couple the stamp to a redundant
/// field. Consumers that need the fingerprint compute it from `tip_change`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetRef {
    /// The channel being tracked (e.g. `"main"`).
    pub channel: ChannelName,
    /// The most-recently applied [`ChangeId`], if the channel is non-empty.
    pub tip_change: Option<ChangeId>,
    /// Optional CAS address of the serialized change-log blob.
    pub change_log_cas: Option<CasKey>,
}

/// Sealed generation manifest.
///
/// Records the source-file list, the IR archive CAS pointer, the optional
/// VCS channel pin, the reference/occurrence blob pointers, and the toolchain
/// descriptor for one sealed generation.
///
/// This is the input to [`generation_stamp`], which derives the logical
/// [`GenerationStamp`] (Hash ①) from a versioned canonical preimage of this
/// struct. The manifest itself may also be serialized and stored in the CAS
/// (Hash ②), but those two keys must never be confused.
///
/// # Field ordering and the stamp
///
/// The stamp preimage encodes fields in the fixed order defined by
/// [`generation_stamp`]'s algorithm. Adding fields requires a new
/// `generation_stamp_v3` function with a new domain tag — the existing stamp
/// must never change.
///
/// # heart-independence
///
/// `workspace/ir`'s `BlobManifestV3` uses `heart::PackageId` and
/// `heart::Toolchain`. This manifest uses [`PackageUuid`] and [`Toolchain`]
/// instead. Consumers convert at the `heart` boundary; the fields are
/// semantically identical.
///
/// # Non-empty file list
///
/// The source-file list is `Box<[FileEntry]>` (a `List<FileEntry>` alias).
/// Unlike `workspace/ir` which uses the `nonempty` crate, nudox-ir uses the
/// `List<T>` convention. Callers are responsible for ensuring the slice is
/// non-empty at construction time; the type does not enforce this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    /// Registry-stable UUID for this package. Stable across all generations.
    pub package: PackageUuid,
    /// Source files in this generation. Must not be empty.
    pub files: crate::List<FileEntry>,
    /// CAS address of the IR archive blob for this generation.
    pub ir_package_ref: CasKey,
    /// Optional pin into the channel DAG at seal time.
    pub change_set_ref: Option<ChangeSetRef>,
    /// Optional CAS address of the cross-package reference blob.
    pub references_ref: Option<CasKey>,
    /// Optional CAS address of the occurrence-index blob.
    pub occurrences_ref: Option<CasKey>,
    /// The toolchain the producer ran under when sealing this generation.
    pub toolchain: Toolchain,
}

// ============================================================================
// BlobManifest stamp derivation
// ============================================================================

/// Compute the [`GenerationStamp`] from a [`BlobManifest`]'s content fields.
///
/// This is a **separate** function from [`generation_stamp`] (which operates
/// on a [`PristineIntroTable`]). In the full pipeline both are computed;
/// their results are stored in the outbox alongside the manifest.
///
/// # Canonical preimage layout
///
/// ```text
/// u32le(version = 2)
/// for file in files sorted ascending by path:
///     u32le(path_utf8_len) || path_utf8 || file_content_hash[32]
/// ir_package_ref[32]
/// if change_set_ref is Some:
///     u8(1)
///     u32le(channel_utf8_len) || channel_utf8
///     if tip_change is Some: u8(1) || tip_change[32]  else: u8(0)
///     if change_log_cas is Some: u8(1) || cas[32]  else: u8(0)
/// else:
///     u8(0)
/// references_ref[32]   (zero-filled if None)
/// occurrences_ref[32]  (zero-filled if None)
/// u32le(language_utf8_len) || language_utf8
/// u32le(version_utf8_len) || version_utf8
/// GenerationStamp = ContentBlake3::from_domain("nudox.gen.manifest.v2", &preimage)
/// ```
///
/// All optional fields use a u8 presence flag (0 = absent, 1 = present) so
/// their absence is unambiguously encoded and distinct from the zero-filled
/// sentinel used for fixed-width optional blob pointers.
///
/// # Why no postcard?
///
/// `workspace/ir` used postcard for `ChangeSetRef` and `Toolchain` in the v3
/// stamp. postcard is not a dependency of nudox-ir, so the fields are encoded
/// manually using the same `encode_str`/`write_u32le` primitives used
/// everywhere else in this crate.
pub fn manifest_stamp(manifest: &BlobManifest) -> GenerationStamp {
    use crate::change::encode::encode_str;

    let mut preimage: Vec<u8> = Vec::new();

    // Version discriminant.
    write_u32le(&mut preimage, 2u32);

    // Source files in ascending path order. Sorting a Vec of references avoids
    // cloning the FileEntry values.
    let mut sorted_files: Vec<&FileEntry> = manifest.files.iter().collect();
    sorted_files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in sorted_files {
        encode_str(&mut preimage, &file.path);
        preimage.extend_from_slice(file.content_hash.as_bytes());
    }

    // IR archive reference (32 raw bytes, always present).
    preimage.extend_from_slice(manifest.ir_package_ref.as_bytes());

    // Optional change-set reference: presence flag then fields.
    match &manifest.change_set_ref {
        Some(csr) => {
            preimage.push(1u8);
            encode_str(&mut preimage, csr.channel.as_str());
            match &csr.tip_change {
                Some(c) => {
                    preimage.push(1u8);
                    preimage.extend_from_slice(c.as_bytes());
                }
                None => preimage.push(0u8),
            }
            match &csr.change_log_cas {
                Some(k) => {
                    preimage.push(1u8);
                    preimage.extend_from_slice(k.as_bytes());
                }
                None => preimage.push(0u8),
            }
        }
        None => preimage.push(0u8),
    }

    // References blob pointer (32 bytes, zero-filled when absent).
    match &manifest.references_ref {
        Some(k) => preimage.extend_from_slice(k.as_bytes()),
        None => preimage.extend_from_slice(&[0u8; 32]),
    }

    // Occurrences blob pointer (32 bytes, zero-filled when absent).
    match &manifest.occurrences_ref {
        Some(k) => preimage.extend_from_slice(k.as_bytes()),
        None => preimage.extend_from_slice(&[0u8; 32]),
    }

    // Toolchain: language then version.
    encode_str(&mut preimage, &manifest.toolchain.language);
    encode_str(&mut preimage, &manifest.toolchain.version);

    GenerationStamp(ContentBlake3::from_domain(
        "nudox.gen.manifest.v2",
        &preimage,
    ))
}

// ============================================================================
// Outbox
// ============================================================================

/// A pending generation that has been sealed but not yet consumed by the
/// downstream pipeline (registry ingestion, search, artifact publication).
///
/// `generation` stores the logical identity (Hash ①). Never store a [`CasKey`]
/// (Hash ②) here — the type system prevents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    /// Package lineage (ecosystem + name) that was sealed.
    pub package: PackageLineageId,
    /// Logical identity of the sealed generation (Hash ①).
    ///
    /// This is a [`GenerationStamp`], not a [`CasKey`]. The distinct newtypes
    /// enforce the K12 Hash ①/② discipline at compile time.
    pub generation: GenerationStamp,
    /// Channel the generation was sealed into.
    pub channel: ChannelName,
    /// The most-recently applied change at seal time, if the channel is
    /// non-empty. Consumers use this to gate downstream jobs on tip alignment.
    pub tip_change: Option<ChangeId>,
}

/// Errors returned by [`Outbox`] operations.
#[derive(Debug)]
pub enum Error {
    /// The outbox's internal lock was poisoned (a previous holder panicked).
    LockPoisoned,
    /// A downstream storage error (for persistent outbox implementations).
    Storage(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::LockPoisoned => f.write_str("outbox lock poisoned"),
            Error::Storage(msg) => write!(f, "outbox storage error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Backwards-compatible alias for cross-crate consumers.
pub use self::Error as OutboxError;

/// A staging area for sealed generation stamps awaiting downstream processing.
///
/// # Object safety
///
/// Both methods take `&self` so the trait is object-safe and can be used as
/// `dyn Outbox`. Implementations must be `Send + Sync`.
///
/// # Ordering guarantees
///
/// `pending()` returns entries in the order they were `append()`ed.
/// Consumers must not assume global ordering across process restarts; a
/// persistent implementation should use a monotonic sequence number.
pub trait Outbox: Send + Sync {
    /// Stage a new [`OutboxEntry`].
    ///
    /// Implementations should be idempotent with respect to the
    /// `generation` stamp when possible (de-duplicate on
    /// `OutboxEntry::generation`), though the in-memory implementation does
    /// not enforce this.
    fn append(&self, entry: OutboxEntry) -> Result<(), Error>;

    /// Return all pending entries in insertion order.
    ///
    /// This does **not** drain the outbox. Callers that want drain-on-ack
    /// semantics should use [`InMemoryOutbox::drain`] or implement a
    /// separate acknowledgement call.
    fn pending(&self) -> Result<Vec<OutboxEntry>, Error>;
}

/// An in-process, in-memory outbox implementation.
///
/// Intended for tests and single-process deployments where durability across
/// restarts is not required. All entries are held in a `Mutex<Vec<_>>`; reads
/// and writes are O(n) in the number of pending entries.
///
/// # Thread safety
///
/// Both `append` and `pending` acquire the mutex for the duration of the call.
#[derive(Debug, Default)]
pub struct InMemoryOutbox {
    entries: Mutex<Vec<OutboxEntry>>,
}

impl InMemoryOutbox {
    /// Create a new, empty in-memory outbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remove all pending entries and return them in insertion order.
    ///
    /// This is the drain operation — it atomically empties the outbox and
    /// returns what was in it. Use this for consume-once semantics.
    pub fn drain(&self) -> Result<Vec<OutboxEntry>, Error> {
        let mut guard = self.entries.lock().map_err(|_| Error::LockPoisoned)?;
        Ok(std::mem::take(&mut *guard))
    }

    /// Return the number of pending entries without cloning them.
    pub fn len(&self) -> Result<usize, Error> {
        let guard = self.entries.lock().map_err(|_| Error::LockPoisoned)?;
        Ok(guard.len())
    }

    /// Return `true` if there are no pending entries.
    pub fn is_empty(&self) -> Result<bool, Error> {
        Ok(self.len()? == 0)
    }
}

impl Outbox for InMemoryOutbox {
    fn append(&self, entry: OutboxEntry) -> Result<(), Error> {
        let mut guard = self.entries.lock().map_err(|_| Error::LockPoisoned)?;
        guard.push(entry);
        Ok(())
    }

    fn pending(&self) -> Result<Vec<OutboxEntry>, Error> {
        let guard = self.entries.lock().map_err(|_| Error::LockPoisoned)?;
        Ok(guard.clone())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
