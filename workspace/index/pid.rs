//! Persistent-identifier stack for a package.
//!
//! PID practice (Handle, DOI / ISO 26324, ARK, the RDA PID kernel, Zenodo’s
//! concept/version DOIs, Software Heritage SWHID / ISO 18670, Package URL)
//! separates five claims that must not share one string:
//!
//! * **Concept** — the work, across versions (Zenodo concept DOI). Opaque. This
//!   is the catalog [`PackageId`](heart::PackageId).
//! * **Version** — one release coordinate. Opaque, and not the concept id.
//! * **Content** — the bytes (SWHID, gitoid, a registry checksum). Intrinsic:
//!   the identifier *is* the digest.
//! * **Location** — where the bytes are fetched today. Replaceable. Not part of
//!   identity. A withdrawn object still resolves (tombstone), it is not
//!   deleted.
//! * **Rendering** — purl, `swh:`, `doi:`. Derived for export. Never a storage
//!   key. A purl names a version coordinate, not bytes.
//!
//! Assigned identifiers stay opaque: a local name that is a URL, a purl, or an
//! `swh:`/`doi:`/`ark:` string is rejected. Those are locators or renderings.

use heart::{PackageId, identity::package_id_from_parts};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

/// Why a string cannot be an assigned (extrinsic) persistent identifier.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PidError {
    /// Empty names are not identifiers.
    #[error("persistent identifier is empty")]
    Empty,
    /// The string is a locator or a rendering of another scheme.
    #[error("persistent identifier must be opaque, not a locator or rendering")]
    NotOpaque,
}

/// Bytes a content PID names. The algorithm is part of the identity: the same
/// hex under SHA-256 and BLAKE3 is not the same object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentDigest {
    /// Catalog CAS digest.
    Blake3([u8; 32]),
    /// Registry checksum (crates.io `cksum`, PyPI `digests.sha256`).
    Sha256([u8; 32]),
    /// Git blob/commit digest. The only digest a SWHID rendering can name.
    GitSha1([u8; 20]),
}

/// The work, across every version. Opaque [`PackageId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConceptPid(PackageId);

/// One release coordinate of a [`ConceptPid`]. A different opaque id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VersionPid(PackageId);

/// The bytes. Intrinsic: equality is the digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentPid(ContentDigest);

/// What a PID resolves to. Locations and the withdrawn flag are kernel
/// metadata. They are not part of [`PidKernel::pid`] equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidKernel {
    /// The identifier being resolved.
    pub pid: BoundPid,
    /// Current fetch URLs. Order is not significant for identity.
    pub locations: Vec<SmolStr>,
    /// Yanked, unpublished, or retracted. The PID still resolves.
    pub withdrawn: bool,
    /// Bytes bound to a concept or version PID, when known.
    pub checksum: Option<ContentDigest>,
}

/// Which granularity a kernel resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundPid {
    /// Versionless work.
    Concept(ConceptPid),
    /// One release.
    Version(VersionPid),
    /// Exact bytes.
    Content(ContentPid),
}

/// A successful resolve. Withdrawn objects return [`Resolve::Tombstone`] with
/// the same PID they had when live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolve {
    /// The object is still published. Locations may be empty.
    Live(PidKernel),
    /// The object was withdrawn. The PID is unchanged.
    Tombstone(PidKernel),
}

impl ConceptPid {
    /// Mint the concept PID for an ecosystem stem.
    ///
    /// Same framing as the catalog package id: ecosystem, then canonical stem.
    #[must_use]
    pub fn mint(ecosystem: &str, stem: &str) -> Self {
        Self(package_id_from_parts([
            ecosystem.as_bytes(),
            stem.as_bytes(),
        ]))
    }

    /// The opaque local name (UUID). Not a purl.
    #[must_use]
    pub fn local_name(self) -> String {
        self.0.as_uuid().to_string()
    }
}

impl VersionPid {
    /// Mint the version PID. The version string is part of the seed, so two
    /// releases of one concept do not share this id.
    #[must_use]
    pub fn mint(ecosystem: &str, stem: &str, version: &str) -> Self {
        Self(package_id_from_parts([
            b"version".as_slice(),
            ecosystem.as_bytes(),
            stem.as_bytes(),
            version.as_bytes(),
        ]))
    }

    /// The opaque local name (UUID). Not a purl.
    #[must_use]
    pub fn local_name(self) -> String {
        self.0.as_uuid().to_string()
    }
}

impl ContentPid {
    /// The content PID for these exact bytes.
    #[must_use]
    pub const fn from_digest(digest: ContentDigest) -> Self {
        Self(digest)
    }

    /// The digest this PID names.
    #[must_use]
    pub const fn digest(self) -> ContentDigest {
        self.0
    }
}

impl PidKernel {
    /// Resolve this kernel. A withdrawn object is a tombstone, not a miss.
    #[must_use]
    pub fn resolve(&self) -> Resolve {
        if self.withdrawn {
            Resolve::Tombstone(self.clone())
        } else {
            Resolve::Live(self.clone())
        }
    }

    /// A new location does not change the PID.
    #[must_use]
    pub fn with_location(&self, location: impl Into<SmolStr>) -> Self {
        let mut next = self.clone();
        next.locations.push(location.into());
        next
    }
}

/// Reject a string that is being stored as an assigned PID.
///
/// Intrinsic content PIDs are digests, not this function. Renderings (`pkg:`,
/// `swh:`, `doi:`, `ark:`) and locators (`http`, `://`) are not local names.
pub fn require_opaque(local_name: &str) -> Result<(), PidError> {
    if local_name.is_empty() {
        return Err(PidError::Empty);
    }
    let lower = local_name.to_ascii_lowercase();
    let rendering = lower.starts_with("pkg:")
        || lower.starts_with("swh:")
        || lower.starts_with("doi:")
        || lower.starts_with("ark:");
    let locator =
        lower.starts_with("http://") || lower.starts_with("https://") || lower.contains("://");
    if rendering || locator {
        Err(PidError::NotOpaque)
    } else {
        Ok(())
    }
}

/// A full git SHA-1 as a content PID. Anything shorter or non-hex is not one.
#[must_use]
pub fn git_sha1(rev: &str) -> Option<ContentDigest> {
    if rev.len() != 40 || !rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0u8; 20];
    for (index, chunk) in rev.as_bytes().chunks(2).enumerate() {
        let hex = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(ContentDigest::GitSha1(bytes))
}

/// Package URL rendering of a version coordinate.
///
/// A rendering for SBOM export. Not a [`VersionPid`] and not a content id.
#[must_use]
pub fn render_purl(ecosystem: &str, namespace: &str, name: &str, version: &str) -> String {
    if namespace.is_empty() {
        format!("pkg:{ecosystem}/{name}@{version}")
    } else {
        format!("pkg:{ecosystem}/{namespace}/{name}@{version}")
    }
}

/// Software Heritage rendering of a git SHA-1 content PID.
///
/// `None` for BLAKE3 and SHA-256: those are not SWHIDs. SWHID class `cnt` is
/// the file object (ISO/IEC 18670).
#[must_use]
pub fn render_swhid(pid: ContentPid) -> Option<String> {
    match pid.digest() {
        ContentDigest::GitSha1(bytes) => Some(format!("swh:1:cnt:{}", hex_encode(&bytes))),
        ContentDigest::Blake3(_) | ContentDigest::Sha256(_) => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concept_is_stable_and_version_is_a_different_id() {
        let concept = ConceptPid::mint("rust", "serde");
        assert_eq!(concept, ConceptPid::mint("rust", "serde"));
        let one = VersionPid::mint("rust", "serde", "1.0.0");
        let two = VersionPid::mint("rust", "serde", "1.0.1");
        assert_ne!(one, two);
        assert_ne!(concept.local_name(), one.local_name());
        assert!(require_opaque(&concept.local_name()).is_ok());
        assert!(require_opaque(&one.local_name()).is_ok());
    }

    #[test]
    fn same_version_coordinate_can_name_two_different_byte_sets() {
        let version = VersionPid::mint("rust", "serde", "1.0.0");
        let again = VersionPid::mint("rust", "serde", "1.0.0");
        assert_eq!(version, again);
        let left = ContentPid::from_digest(ContentDigest::Sha256([1; 32]));
        let right = ContentPid::from_digest(ContentDigest::Sha256([2; 32]));
        assert_ne!(left, right);
        let blake = ContentPid::from_digest(ContentDigest::Blake3([1; 32]));
        assert_ne!(left, blake);
    }

    #[test]
    fn a_new_location_does_not_change_the_pid() {
        let kernel = PidKernel {
            pid: BoundPid::Version(VersionPid::mint("npm", "lodash", "4.17.21")),
            locations: vec![SmolStr::new("https://registry.npmjs.org/lodash")],
            withdrawn: false,
            checksum: Some(ContentDigest::Sha256([9; 32])),
        };
        let moved = kernel.with_location("https://mirror.example/lodash");
        assert_eq!(kernel.pid, moved.pid);
        assert_ne!(kernel.locations, moved.locations);
    }

    #[test]
    fn withdrawn_still_resolves_to_the_same_pid() {
        let kernel = PidKernel {
            pid: BoundPid::Concept(ConceptPid::mint("rust", "yanked-crate")),
            locations: vec![],
            withdrawn: true,
            checksum: None,
        };
        match kernel.resolve() {
            Resolve::Tombstone(found) => assert_eq!(found.pid, kernel.pid),
            Resolve::Live(_) => panic!("withdrawn pid must tombstone"),
        }
    }

    #[test]
    fn renderings_are_rejected_as_assigned_ids() {
        for bad in [
            "pkg:cargo/serde@1.0.0",
            "swh:1:rev:abcdef",
            "doi:10.5281/zenodo.1",
            "ark:/12345/fk1",
            "https://crates.io/crates/serde",
            "",
        ] {
            assert_eq!(
                require_opaque(bad),
                if bad.is_empty() {
                    Err(PidError::Empty)
                } else {
                    Err(PidError::NotOpaque)
                }
            );
        }
        let purl = render_purl("cargo", "", "serde", "1.0.0");
        assert_eq!(purl, "pkg:cargo/serde@1.0.0");
        assert_eq!(require_opaque(&purl), Err(PidError::NotOpaque));
        let version = VersionPid::mint("cargo", "serde", "1.0.0");
        assert_ne!(version.local_name(), purl);
    }

    #[test]
    fn swhid_renders_only_git_sha1_content() {
        let git = ContentPid::from_digest(ContentDigest::GitSha1([0xab; 20]));
        let rendered = render_swhid(git).expect("git sha1 is a swhid");
        assert!(rendered.starts_with("swh:1:cnt:"));
        assert_eq!(require_opaque(&rendered), Err(PidError::NotOpaque));
        let blake = ContentPid::from_digest(ContentDigest::Blake3([0xab; 32]));
        assert!(render_swhid(blake).is_none());
    }
}
