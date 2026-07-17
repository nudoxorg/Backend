//! [`GenerationStamp`] derivation (Hash ①) from a [`BlobManifestV3`].
//!
//! # The two hashes
//!
//! The design (K12, Issue 5) distinguishes two distinct hash planes:
//!
//! - **Hash ①** — the *logical generation identity*: a [`GenerationStamp`]
//!   derived from a versioned canonical preimage of the manifest's *content*.
//!   This is what the outbox stores, what callers compare for equality, and
//!   what [`generation_stamp_v3`] computes.
//! - **Hash ②** — the *CAS address* of the manifest blob itself: a [`CasKey`]
//!   that identifies the exact serialized bytes of the manifest. Useful as a
//!   storage address, but **never** a generation identity.
//!
//! These types are deliberately distinct newtypes; the outbox (`crate::outbox`)
//! stores a `GenerationStamp`, not a `CasKey`, enforcing Hash ①/② discipline at
//! the type level.
//!
//! # Version stability
//!
//! `generation_stamp_v3` seals the preimage format at **version 3**. Historical
//! generations sealed with v3 are forever stable: their stamps will never change
//! because neither the domain tag (`"nudox.gen.v3"`) nor the preimage encoding
//! algorithm can change. Future breaking changes require a new function
//! (`generation_stamp_v4`, etc.) with a distinct domain tag.
//!
//! # Canonical preimage layout (v3)
//!
//! ```text
//! u32le version = 3
//! for file in files sorted ascending by path:
//!     u32le(path_utf8_len) || path_utf8 || file_blake3[32]
//! file_blake3 = content_hash.as_bytes()  (CasKey is #[repr(transparent)] over ContentBlake3)
//! ir_package_ref[32]
//! if change_set_ref is Some:
//!     u32le(postcard_bytes_len) || postcard(ChangeSetRef)
//! else:
//!     u32le(0)
//! references_ref[32]   (zero-filled if None)
//! occurrences_ref[32]  (zero-filled if None)
//! u32le(toolchain_postcard_len) || postcard(Toolchain)
//! GenerationStamp = GenerationStamp::from_domain("nudox.gen.v3", &preimage)
//! ```
//!
//! Postcard is used for `ChangeSetRef` and `Toolchain` because both are
//! enum-containing structs whose stable encoding is owned by `nudox-change`/
//! `heart` respectively. Any future change to those types' postcard encoding
//! would require a v4 stamp function with a new domain tag.

use nudox_change::{
    GenerationStamp,
    encode::{encode_str, write_u32le},
};
use thiserror::Error;

use crate::manifest::BlobManifestV3;

/// Domain constant for the v3 generation stamp.
///
/// Frozen. A different constant is required for every future incompatible
/// preimage layout change.
const DOMAIN_V3: &str = "nudox.gen.v3";

/// Errors that can occur during [`generation_stamp_v3`].
///
/// Currently the only failure mode is a postcard serialization error on the
/// `ChangeSetRef` or `Toolchain` fields. Postcard is infallible for
/// fixed-layout in-memory types in practice, but we model the error for
/// correctness.
#[derive(Debug, Error)]
pub enum GenerationStampError {
    /// Postcard serialization of [`ChangeSetRef`] failed.
    #[error("postcard serialization of ChangeSetRef failed: {0}")]
    ChangeSetRefSerialize(postcard::Error),
    /// Postcard serialization of [`heart::Toolchain`] failed.
    #[error("postcard serialization of Toolchain failed: {0}")]
    ToolchainSerialize(postcard::Error),
}

/// Derive the v3 [`GenerationStamp`] (Hash ①) from a [`BlobManifestV3`].
///
/// The stamp is the BLAKE3 digest of a versioned canonical preimage (see
/// module-level docs and Appendix C of the design doc). Key properties:
///
/// - **Deterministic**: the same manifest always yields the same stamp.
/// - **File-order-independent**: files are sorted ascending by `path` before
///   encoding, so the stamp is independent of insertion order.
/// - **Version-tagged**: the preimage starts with `u32le(3)` and the BLAKE3
///   domain is `"nudox.gen.v3"`, making v3 stamps non-colliding with future
///   versions even if their other inputs coincide.
/// - **Not Hash ②**: this is never the CAS address of the manifest blob
///   itself. Do not pass a `CasKey` where a `GenerationStamp` is required.
///
/// # Errors
///
/// Returns [`GenerationStampError`] if postcard serialization of
/// `ChangeSetRef` or `Toolchain` fails (rare; these types are stable).
pub fn generation_stamp_v3(
    manifest: &BlobManifestV3,
) -> Result<GenerationStamp, GenerationStampError> {
    let mut preimage: Vec<u8> = Vec::new();

    // Version discriminant.
    write_u32le(&mut preimage, 3u32);

    // Source files in ascending path order.
    // We sort a Vec of references rather than cloning to avoid an unnecessary
    // allocation of the file entries themselves.
    let mut sorted_files: Vec<&crate::manifest::FileEntry> = manifest.files.iter().collect();
    sorted_files.sort_by(|a, b| a.path.cmp(&b.path));

    for file in sorted_files {
        encode_str(&mut preimage, &file.path);
        preimage.extend_from_slice(file.content_hash.as_bytes());
    }

    // IR archive reference (32 raw bytes, always present).
    preimage.extend_from_slice(manifest.ir_package_ref.as_bytes());

    // Optional change-set reference: length-framed postcard or u32le(0).
    match &manifest.change_set_ref {
        Some(csr) => {
            let csr_bytes = postcard::to_allocvec(csr)
                .map_err(GenerationStampError::ChangeSetRefSerialize)?;
            write_u32le(&mut preimage, u32::try_from(csr_bytes.len())
                .expect("ChangeSetRef postcard bytes exceed u32::MAX"));
            preimage.extend_from_slice(&csr_bytes);
        }
        None => {
            write_u32le(&mut preimage, 0u32);
        }
    }

    // References blob pointer (32 bytes, zero-filled when absent).
    match &manifest.references_ref {
        Some(key) => preimage.extend_from_slice(key.as_bytes()),
        None => preimage.extend_from_slice(&[0u8; 32]),
    }

    // Occurrences blob pointer (32 bytes, zero-filled when absent).
    match &manifest.occurrences_ref {
        Some(key) => preimage.extend_from_slice(key.as_bytes()),
        None => preimage.extend_from_slice(&[0u8; 32]),
    }

    // Toolchain: length-framed postcard.
    let tc_bytes = postcard::to_allocvec(&manifest.toolchain)
        .map_err(GenerationStampError::ToolchainSerialize)?;
    write_u32le(&mut preimage, u32::try_from(tc_bytes.len())
        .expect("Toolchain postcard bytes exceed u32::MAX"));
    preimage.extend_from_slice(&tc_bytes);

    Ok(GenerationStamp::from_domain(DOMAIN_V3, &preimage))
}

#[cfg(test)]
mod tests {
    use nonempty::nonempty;
    use nudox_change::{CasKey, ChangeSetFingerprint, ChannelName};

    use super::*;
    use crate::manifest::{BlobManifestV3, ChangeSetRef, FileEntry};

    fn cas(byte: u8) -> CasKey {
        CasKey::from_raw([byte; 32])
    }

    fn package_id() -> heart::PackageId {
        use heart::identity::Id;
        let ns = uuid::Uuid::nil();
        Id::from_name(&ns, b"test-pkg")
    }

    fn toolchain() -> heart::Toolchain {
        heart::Toolchain::Rust {
            compiler: semver::Version::new(1, 80, 0),
            edition: heart::ecosystem::Edition::E2021,
        }
    }

    fn base_manifest() -> BlobManifestV3 {
        BlobManifestV3 {
            package: package_id(),
            files: nonempty![
                FileEntry { path: "src/lib.rs".to_string(), content_hash: cas(0x01) },
                FileEntry { path: "src/main.rs".to_string(), content_hash: cas(0x02) },
            ],
            ir_package_ref: cas(0x10),
            change_set_ref: None,
            references_ref: None,
            occurrences_ref: None,
            toolchain: toolchain(),
        }
    }

    /// The stamp must be deterministic: same manifest → same stamp every time.
    #[test]
    fn stamp_is_deterministic() {
        let m = base_manifest();
        let s1 = generation_stamp_v3(&m).unwrap();
        let s2 = generation_stamp_v3(&m).unwrap();
        assert_eq!(s1, s2, "same manifest must produce the same stamp");
    }

    /// Reordering `files` must NOT change the stamp (we sort by path).
    #[test]
    fn stamp_is_file_order_independent() {
        let mut m_reordered = base_manifest();
        // Swap the two files so they appear in reverse order.
        m_reordered.files = nonempty![
            FileEntry { path: "src/main.rs".to_string(), content_hash: cas(0x02) },
            FileEntry { path: "src/lib.rs".to_string(), content_hash: cas(0x01) },
        ];

        let s_original = generation_stamp_v3(&base_manifest()).unwrap();
        let s_reordered = generation_stamp_v3(&m_reordered).unwrap();
        assert_eq!(
            s_original, s_reordered,
            "reordering files must not change the stamp (sorting by path is applied)"
        );
    }

    /// Changing a file's content hash MUST change the stamp.
    #[test]
    fn stamp_changes_when_file_hash_changes() {
        let mut m_changed = base_manifest();
        m_changed.files = nonempty![
            FileEntry { path: "src/lib.rs".to_string(), content_hash: cas(0xFF) }, // different
            FileEntry { path: "src/main.rs".to_string(), content_hash: cas(0x02) },
        ];

        let s_original = generation_stamp_v3(&base_manifest()).unwrap();
        let s_changed = generation_stamp_v3(&m_changed).unwrap();
        assert_ne!(
            s_original, s_changed,
            "changing a file hash must change the stamp"
        );
    }

    /// Changing a file's path MUST change the stamp.
    #[test]
    fn stamp_changes_when_file_path_changes() {
        let mut m_changed = base_manifest();
        m_changed.files = nonempty![
            FileEntry { path: "src/renamed.rs".to_string(), content_hash: cas(0x01) },
            FileEntry { path: "src/main.rs".to_string(), content_hash: cas(0x02) },
        ];

        let s_original = generation_stamp_v3(&base_manifest()).unwrap();
        let s_changed = generation_stamp_v3(&m_changed).unwrap();
        assert_ne!(s_original, s_changed, "changing a file path must change the stamp");
    }

    /// Adding an optional field (e.g. `references_ref`) MUST change the stamp.
    #[test]
    fn stamp_changes_when_optional_field_added() {
        let s_without = generation_stamp_v3(&base_manifest()).unwrap();

        let mut m_with = base_manifest();
        m_with.references_ref = Some(cas(0xAA));
        let s_with = generation_stamp_v3(&m_with).unwrap();

        assert_ne!(s_without, s_with, "adding references_ref must change the stamp");
    }

    /// Changing the toolchain MUST change the stamp.
    #[test]
    fn stamp_changes_when_toolchain_changes() {
        let s_original = generation_stamp_v3(&base_manifest()).unwrap();

        let mut m_changed = base_manifest();
        m_changed.toolchain = heart::Toolchain::Rust {
            compiler: semver::Version::new(1, 81, 0), // different patch
            edition: heart::ecosystem::Edition::E2021,
        };
        let s_changed = generation_stamp_v3(&m_changed).unwrap();

        assert_ne!(s_original, s_changed, "changing the toolchain must change the stamp");
    }

    /// `generation_stamp_v3` returns `GenerationStamp`, not `CasKey`.
    /// This test is type-level: it confirms the return type is correct.
    #[test]
    fn return_type_is_generation_stamp_not_cas_key() {
        let stamp: GenerationStamp = generation_stamp_v3(&base_manifest()).unwrap();
        // Just exercise the Debug impl to ensure it's the right newtype.
        let debug = format!("{:?}", stamp);
        assert!(debug.starts_with("gen:"), "expected gen: prefix, got: {debug}");
    }

    /// **Golden pin** (design Issue 16 / "Outbox — day one"): the v3 stamp of a
    /// fixed manifest is pinned to an exact digest. Any change to the preimage
    /// layout, the domain tag, `encode_str`, or the postcard encoding of
    /// `ChangeSetRef`/`Toolchain` breaks this test — which is the point:
    /// historical generations must keep their stamps forever, so an intentional
    /// layout change requires a *new* `generation_stamp_v4`, never an edit here.
    #[test]
    fn stamp_golden_pin_v3() {
        let mut m = base_manifest();
        m.change_set_ref = Some(ChangeSetRef {
            channel: ChannelName::new("main"),
            tip: ChangeSetFingerprint::empty(),
            tip_change: None,
            change_log_cas: Some(cas(0x42)),
        });
        m.references_ref = Some(cas(0xAA));

        let stamp = generation_stamp_v3(&m).unwrap();
        assert_eq!(
            stamp.to_hex(),
            "7fb8950f498cd3b643838e8914adc68605c5d84f219abbe1cba5e0776c64013a",
            "v3 stamp preimage drifted — this is a wire-stability break"
        );
    }

    /// A `ChangeSetRef` round-trips through postcard (validates our encoding).
    #[test]
    fn stamp_with_change_set_ref_is_deterministic() {
        let mut m = base_manifest();
        m.change_set_ref = Some(ChangeSetRef {
            channel: ChannelName::new("main"),
            tip: ChangeSetFingerprint::empty(),
            tip_change: None,
            change_log_cas: Some(cas(0x42)),
        });

        let s1 = generation_stamp_v3(&m).unwrap();
        let s2 = generation_stamp_v3(&m).unwrap();
        assert_eq!(s1, s2);

        // Also confirm it differs from the stamp without a change_set_ref.
        let s_base = generation_stamp_v3(&base_manifest()).unwrap();
        assert_ne!(s1, s_base, "change_set_ref presence must affect the stamp");
    }
}
