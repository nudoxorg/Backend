//! [`BlobManifestV3`] — the sealed source-file manifest for one package generation.
//!
//! A manifest records:
//! - `package` — the [`heart::PackageId`] UUID that identifies this package in the
//!   registry (stable across all its generations).
//! - `files` — a non-empty list of [`FileEntry`] values, each carrying the file's
//!   path and its CAS address (`content_hash`). File size is intentionally absent
//!   from this struct (v3 design note: size is derivable from the blob and its
//!   inclusion would make stamp recalculation sensitive to a redundant field).
//! - `ir_package_ref` — a [`CasKey`] pointing at the IR archive blob.
//! - `change_set_ref` — optional pointer into the channel DAG: the channel name,
//!   its tip [`ChangeSetFingerprint`], the tip [`ChangeId`], and an optional CAS
//!   address for the serialized change log.
//! - `references_ref` / `occurrences_ref` — optional CAS pointers to the
//!   cross-package reference blob and the occurrence index blob respectively.
//! - `toolchain` — the [`heart::Toolchain`] the producer ran under.
//!
//! [`generation_stamp_v3`](crate::generation::generation_stamp_v3) computes the
//! logical [`GenerationStamp`] (Hash ①) from a versioned canonical preimage of
//! this struct. **Never** pass the manifest's own CAS address as a generation
//! stamp — that is Hash ② and breaks the K12 discipline.

use nonempty::NonEmpty;
use serde::{Deserialize, Serialize};

use nudox_change::{CasKey, ChangeId, ChangeSetFingerprint, ChannelName};

/// One source file inside a sealed generation.
///
/// `path` is the package-relative UTF-8 path (POSIX separators, no leading
/// `/`). `content_hash` is the CAS address of the raw file bytes.
///
/// File size is deliberately absent (v3): it is derivable from the blob and
/// its inclusion would couple the stamp to a field that carries no additional
/// identity information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Package-relative UTF-8 file path (POSIX separators, no leading `/`).
    pub path: String,
    /// CAS address of the file's raw byte content.
    pub content_hash: CasKey,
}

/// A pinned reference into a package's channel DAG at seal time.
///
/// `channel` is the channel name (e.g. `"main"`). `tip` is the
/// [`ChangeSetFingerprint`] of the applied change set at seal time.
/// `tip_change` is the [`ChangeId`] of the most-recently applied change
/// (may be `None` for a fresh channel with no changes). `change_log_cas` is
/// an optional CAS address for the serialized change-log blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetRef {
    /// The channel being tracked (e.g. `"main"`).
    pub channel: ChannelName,
    /// Equality-only fingerprint of the applied change set at seal time.
    pub tip: ChangeSetFingerprint,
    /// The most-recently applied [`ChangeId`], if the channel is non-empty.
    pub tip_change: Option<ChangeId>,
    /// Optional CAS address of the serialized change-log blob.
    pub change_log_cas: Option<CasKey>,
}

/// Sealed generation manifest (v3).
///
/// This type is the input to
/// [`generation_stamp_v3`](crate::generation::generation_stamp_v3), which
/// derives the logical [`GenerationStamp`] (Hash ①) from a versioned
/// canonical preimage of this struct. The manifest itself may also be stored
/// in the CAS (Hash ②), but those two keys must never be confused.
///
/// # Field ordering and the stamp
///
/// The stamp preimage encodes fields in a fixed order defined by Appendix C
/// of the design doc (Rev 3.2). Adding fields requires a version bump
/// (`generation_stamp_v4`) — existing v3 stamps are forever stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifestV3 {
    /// Registry-stable UUID for this package. Stable across all generations.
    pub package: heart::PackageId,
    /// Non-empty list of source files in this generation.
    pub files: NonEmpty<FileEntry>,
    /// CAS address of the IR archive blob for this generation.
    pub ir_package_ref: CasKey,
    /// Optional pin into the channel DAG at seal time.
    pub change_set_ref: Option<ChangeSetRef>,
    /// Optional CAS address of the cross-package reference blob.
    pub references_ref: Option<CasKey>,
    /// Optional CAS address of the occurrence-index blob.
    pub occurrences_ref: Option<CasKey>,
    /// The toolchain the producer ran under when sealing this generation.
    pub toolchain: heart::Toolchain,
}

#[cfg(test)]
mod tests {
    use nonempty::nonempty;
    use nudox_change::{CasKey, ChangeSetFingerprint, ChannelName};

    use super::*;

    fn dummy_cas(byte: u8) -> CasKey {
        CasKey::from_raw([byte; 32])
    }

    fn dummy_package_id() -> heart::PackageId {
        use heart::identity::Id;
        let ns = uuid::Uuid::nil();
        Id::from_name(&ns, b"test-package")
    }

    fn dummy_toolchain() -> heart::Toolchain {
        heart::Toolchain::Rust {
            compiler: semver::Version::new(1, 80, 0),
            edition: heart::ecosystem::Edition::E2021,
        }
    }

    /// Round-trip a `FileEntry` through postcard (serde).
    #[test]
    fn file_entry_postcard_roundtrip() {
        let entry = FileEntry {
            path: "src/lib.rs".to_string(),
            content_hash: dummy_cas(0xab),
        };
        let bytes = postcard::to_allocvec(&entry).unwrap();
        let back: FileEntry = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(entry, back);
    }

    /// Round-trip a full `BlobManifestV3` through postcard.
    #[test]
    fn blob_manifest_v3_postcard_roundtrip() {
        let manifest = BlobManifestV3 {
            package: dummy_package_id(),
            files: nonempty![
                FileEntry { path: "src/lib.rs".to_string(), content_hash: dummy_cas(1) },
                FileEntry { path: "src/main.rs".to_string(), content_hash: dummy_cas(2) },
            ],
            ir_package_ref: dummy_cas(0x10),
            change_set_ref: Some(ChangeSetRef {
                channel: ChannelName::new("main"),
                tip: ChangeSetFingerprint::empty(),
                tip_change: None,
                change_log_cas: None,
            }),
            references_ref: Some(dummy_cas(0x20)),
            occurrences_ref: None,
            toolchain: dummy_toolchain(),
        };
        let bytes = postcard::to_allocvec(&manifest).unwrap();
        let back: BlobManifestV3 = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(manifest, back);
    }
}
