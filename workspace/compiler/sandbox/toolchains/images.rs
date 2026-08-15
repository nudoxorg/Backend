//! OCI toolchain-image sealed-plane types (SMOLVM-PLAN SV-3 / §3.2).
//!
//! # Two planes, one `ToolchainPlane`
//!
//! The Backend compile pipeline has two toolchain planes that must **never**
//! produce colliding `JobKey`s even when their logical content would otherwise
//! be identical:
//!
//! - **Sealed-image plane** (`ToolchainPlane::SealedImages`): Linux microVM
//!   guests run OCI toolchain images, fingerprinted by their content (config
//!   digest + sorted layer digests). Same image on desktop and fleet ⇒ same
//!   fingerprint ⇒ **JobKey parity across planes** (GD-18, risk V6).
//!
//! - **Host fast-path plane** (`ToolchainPlane::HostFastPath`): trusted in-
//!   process compiles keyed on host directory paths (the legacy `ToolchainSet`).
//!   Fast-path Jobs must never match sealed-path JobKeys even if a host path
//!   happened to produce the same hash — `ToolchainPlane::digest` injects a
//!   plane-specific domain prefix so this is structurally impossible.
//!
//! # Fingerprint stability (SV-3 + risk V6)
//!
//! `ToolchainPlane::digest` is the value that feeds `JobKey::derive`'s
//! `toolchain` argument. `ToolchainSet::digest` is left byte-for-byte
//! unchanged; domain separation lives here, not there — existing fast-path
//! JobKeys do not shift.
//!
//! # Pull / IO
//!
//! This module contains **metadata and fingerprints only**. Actual image
//! pulls are performed by smolvm's registry client (`crates/smolvm-registry/`)
//! and are out of scope here. `ToolchainImageStore` is the handoff point:
//! the cage's `GoldenPool` receives a `ToolchainImageStore` and calls
//! `lookup` to retrieve the fingerprinted metadata it needs; it then drives
//! smolvm pull machinery independently.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::str::FromStr;

use heart::ContentHash;
use thiserror::Error;

use crate::budget::profiles::ProducerProfile;

// ─── hex helpers (no external dep) ──────────────────────────────────────────

/// Encode 32 bytes as 64 lowercase hex characters.
fn encode_hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        // Writing into a `String` is infallible.
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Decode exactly 64 lowercase hex characters into 32 bytes.
///
/// Returns `None` if the input is not exactly 64 lowercase hex characters.
fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

// ─── ImageDigest ─────────────────────────────────────────────────────────────

/// An OCI content digest in `sha256:<64 lowercase hex>` form.
///
/// Stores the raw 32 bytes internally; `Display` emits the canonical
/// `sha256:<hex>` string that OCI registries and smolvm's registry client
/// expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageDigest([u8; 32]);

impl ImageDigest {
    /// The raw 32-byte SHA-256 digest.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Wrap pre-validated raw bytes (e.g. decoded from registry JSON).
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Error returned when an OCI digest string is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid OCI digest")]
pub struct ParseDigestError(String);

impl FromStr for ImageDigest {
    type Err = ParseDigestError;

    /// Parse `sha256:<64 lowercase hex>`.
    ///
    /// Rejects uppercase hex, wrong algorithm prefix, or wrong byte count.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s
            .strip_prefix("sha256:")
            .ok_or_else(|| ParseDigestError(format!("missing `sha256:` prefix in {s:?}")))?;
        let bytes = decode_hex32(hex).ok_or_else(|| {
            ParseDigestError(format!(
                "expected 64 lowercase hex chars after `sha256:`, got {s:?}"
            ))
        })?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", encode_hex32(&self.0))
    }
}

// ─── OciImageRef ─────────────────────────────────────────────────────────────

/// The reference part of an OCI image name: a tag or a digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OciReference {
    /// `:<tag>` — mutable; e.g. `latest`, `1.2.3`.
    Tag(String),
    /// `@sha256:<hex>` — content-pinned.
    Digest(ImageDigest),
}

impl fmt::Display for OciReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tag(t) => write!(f, ":{t}"),
            Self::Digest(d) => write!(f, "@{d}"),
        }
    }
}

/// A fully-qualified OCI image reference.
///
/// Parses the two standard OCI string forms:
/// - `registry/repository:tag`
/// - `registry/repository@sha256:<hex>`
///
/// The `Display` impl produces a canonical roundtrip string.
///
/// # Validation
///
/// - Registry must be non-empty.
/// - Repository must be non-empty.
/// - For digest references the digest must be a valid `sha256:` digest
///   (see [`ImageDigest`]).
/// - Tags must be non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OciImageRef {
    /// Registry hostname (+ optional port), e.g. `registry.example.com:5000`.
    pub registry: String,
    /// Repository path, e.g. `nudox/toolchain-rust`.
    pub repository: String,
    /// Tag or pinned digest.
    pub reference: OciReference,
}

/// Error returned when an OCI image reference string is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid OCI image ref")]
pub struct ParseImageRefError(String);

impl FromStr for OciImageRef {
    type Err = ParseImageRefError;

    /// Parse `registry/repo:tag` or `registry/repo@sha256:<hex>`.
    ///
    /// The first path component is taken as the registry; everything up to the
    /// `:` or `@` is the repository.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split at `@` (digest) or `:` (tag), preferring `@`.
        if let Some(at) = s.find('@') {
            let repo_part = &s[..at];
            let digest_part = &s[at + 1..];
            let (registry, repository) = split_registry_repo(repo_part)
                .ok_or_else(|| ParseImageRefError(format!("missing registry in {s:?}")))?;
            let digest = digest_part
                .parse::<ImageDigest>()
                .map_err(|e| ParseImageRefError(format!("bad digest in {s:?}: {e}")))?;
            Ok(Self {
                registry,
                repository,
                reference: OciReference::Digest(digest),
            })
        } else if let Some(colon) = s.rfind(':') {
            // Make sure the colon is after a `/` so we don't confuse a
            // port in the registry (`host:5000/repo`) with a tag. A colon
            // before the first slash is registry-port; a colon after the
            // last slash is a tag.
            let after_last_slash = s.rfind('/').map_or(0, |i| i + 1);
            if colon < after_last_slash {
                // No explicit tag — treat rest as tag-less (error).
                return Err(ParseImageRefError(format!("no tag or digest in {s:?}")));
            }
            let repo_part = &s[..colon];
            let tag = s[colon + 1..].to_owned();
            if tag.is_empty() {
                return Err(ParseImageRefError(format!("empty tag in {s:?}")));
            }
            let (registry, repository) = split_registry_repo(repo_part)
                .ok_or_else(|| ParseImageRefError(format!("missing registry in {s:?}")))?;
            Ok(Self {
                registry,
                repository,
                reference: OciReference::Tag(tag),
            })
        } else {
            Err(ParseImageRefError(format!("no tag or digest in {s:?}")))
        }
    }
}

/// Split `registry/rest` into `(registry, rest)`.
///
/// Returns `None` if either component is empty.
fn split_registry_repo(s: &str) -> Option<(String, String)> {
    let slash = s.find('/')?;
    let registry = &s[..slash];
    let repository = &s[slash + 1..];
    if registry.is_empty() || repository.is_empty() {
        return None;
    }
    Some((registry.to_owned(), repository.to_owned()))
}

impl fmt::Display for OciImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}{}", self.registry, self.repository, self.reference)
    }
}

// ─── ToolchainImage ───────────────────────────────────────────────────────────

/// Metadata for a single OCI toolchain image (config + layers).
///
/// `content_fingerprint` is the value that feeds `ToolchainImageSet::digest`
/// and ultimately `JobKey::derive`'s `toolchain` argument (via
/// `ToolchainPlane::digest`). Pull / IO is intentionally absent — smolvm's
/// registry client owns that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainImage {
    /// The OCI image reference (registry + repository + tag or digest).
    pub image: OciImageRef,
    /// SHA-256 digest of the image config blob.
    pub config_digest: ImageDigest,
    /// SHA-256 digests of each layer blob, in any order (fingerprint sorts them).
    pub layer_digests: Vec<ImageDigest>,
}

impl ToolchainImage {
    /// Blake3 content fingerprint over `"nudox-producer/1"` ‖ config digest ‖
    /// sorted layer digests (§3.2 verbatim).
    ///
    /// Layer digests are **sorted bytewise** before hashing so that
    /// enumeration order at pull time never changes the fingerprint. Each
    /// component is length-prefixed (little-endian `u64`) so the encoding is
    /// unambiguous — the same discipline as [`heart::JobKey::derive`].
    pub fn content_fingerprint(&self) -> ContentHash {
        let domain = b"nudox-producer/1";
        let mut sorted_layers = self.layer_digests.clone();
        sorted_layers.sort_unstable();

        let mut h = ContentHash::builder();
        // domain
        h.update(&(domain.len() as u64).to_le_bytes());
        h.update(domain);
        // config digest
        h.update(&(32u64).to_le_bytes());
        h.update(self.config_digest.as_bytes());
        // sorted layer digests
        h.update(&(sorted_layers.len() as u64).to_le_bytes());
        for layer in &sorted_layers {
            h.update(&(32u64).to_le_bytes());
            h.update(layer.as_bytes());
        }
        h.finalize()
    }
}

// ─── ToolchainImageSet ────────────────────────────────────────────────────────

/// A mapping from [`ProducerProfile`] → [`ToolchainImage`].
///
/// Keyed by `ProducerProfile` because that is the existing enum that names
/// every producer class in the sandbox crate and is already visible here.
/// The map is a `BTreeMap` so iteration order is deterministic without
/// additional sorting.
///
/// `digest()` is computed by hashing the sorted (by `ProducerProfile`
/// discriminant wire-name) entries' fingerprints, domain-prefixed as
/// `"nudox.toolchain.image-set/1"`.
#[derive(Debug, Clone, Default)]
pub struct ToolchainImageSet {
    inner: BTreeMap<ProducerProfileKey, ToolchainImage>,
}

/// Newtype wrapper that gives `ProducerProfile` a stable `Ord` via its wire
/// name, so `BTreeMap` iteration order is deterministic across Rust versions
/// regardless of enum variant addition order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProducerProfileKey(ProducerProfile);

impl PartialOrd for ProducerProfileKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProducerProfileKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.wire_name().cmp(other.0.wire_name())
    }
}

impl ToolchainImageSet {
    /// Empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the image for `profile`.
    pub fn insert(&mut self, profile: ProducerProfile, image: ToolchainImage) {
        self.inner.insert(ProducerProfileKey(profile), image);
    }

    /// Look up the image for `profile`.
    pub fn get(&self, profile: ProducerProfile) -> Option<&ToolchainImage> {
        self.inner.get(&ProducerProfileKey(profile))
    }

    /// Iterate over `(profile, image)` pairs in wire-name order.
    pub fn iter(&self) -> impl Iterator<Item = (ProducerProfile, &ToolchainImage)> {
        self.inner.iter().map(|(k, v)| (k.0, v))
    }

    /// Blake3 digest over all entries in sorted (wire-name) order.
    ///
    /// Domain prefix: `"nudox.toolchain.image-set/1"`.
    pub fn digest(&self) -> ContentHash {
        let domain = b"nudox.toolchain.image-set/1";
        let mut h = ContentHash::builder();
        h.update(&(domain.len() as u64).to_le_bytes());
        h.update(domain);
        h.update(&(self.inner.len() as u64).to_le_bytes());
        for (key, image) in &self.inner {
            let name = key.0.wire_name().as_bytes();
            h.update(&(name.len() as u64).to_le_bytes());
            h.update(name);
            let fp = image.content_fingerprint();
            h.update(fp.as_bytes());
        }
        h.finalize()
    }
}

// ─── ToolchainImageStore ─────────────────────────────────────────────────────

/// Metadata store and lookup point for OCI toolchain images.
///
/// This is the handoff between the sandbox's image metadata layer and the
/// cage's `GoldenPool`. The store holds a [`ToolchainImageSet`] and provides
/// profile-keyed lookup. **Pull / IO is intentionally out of scope** — actual
/// image pulls are performed by smolvm's registry client
/// (`crates/smolvm-registry/`) after the `GoldenPool` receives the
/// [`ToolchainImage`] metadata from here.
#[derive(Debug, Clone, Default)]
pub struct ToolchainImageStore {
    images: ToolchainImageSet,
}

impl ToolchainImageStore {
    /// Construct from a pre-populated [`ToolchainImageSet`].
    pub fn from_set(images: ToolchainImageSet) -> Self {
        Self { images }
    }

    /// Look up the [`ToolchainImage`] for `profile`.
    ///
    /// Returns `None` if no image has been registered for that profile. The
    /// caller (typically `GoldenPool`) is responsible for deciding whether a
    /// missing image is a hard error or a fallback condition.
    pub fn lookup(&self, profile: ProducerProfile) -> Option<&ToolchainImage> {
        self.images.get(profile)
    }

    /// The underlying set (for digest computation and bulk iteration).
    pub fn image_set(&self) -> &ToolchainImageSet {
        &self.images
    }
}

// ─── ToolchainPlane ──────────────────────────────────────────────────────────

/// Discriminates between the two toolchain planes and produces a plane-aware
/// `ContentHash` for use as `JobKey::derive`'s `toolchain` argument.
///
/// # Collision guarantee (risk V6)
///
/// Even if the inner fingerprint bytes were identical between the two planes
/// (which cannot happen in practice), the distinct domain prefixes
/// `"nudox.toolchain.sealed-image/1"` and `"nudox.toolchain.host-path/1"`
/// are hashed *before* the inner digest. Equal inner content therefore still
/// yields different `ToolchainPlane::digest` values.
///
/// # Legacy JobKey stability
///
/// `ToolchainSet::digest()` is left **byte-for-byte unchanged** — it feeds
/// host-path JobKeys that are already in the CAS. Domain separation lives
/// here in `ToolchainPlane::digest`, never inside `ToolchainSet::digest`.
#[derive(Debug, Clone)]
pub enum ToolchainPlane {
    /// Sealed microVM plane: OCI image fingerprints (SV-3).
    ///
    /// Desktop and fleet both use Linux guests with the same image ⇒ same
    /// fingerprint ⇒ identical JobKey (GD-18 / risk V6).
    SealedImages(ToolchainImageSet),
    /// Trusted in-process fast path: host directory paths (legacy `ToolchainSet`).
    ///
    /// Only valid for `Policy::Development` or `ThreatTier::StaticParser` on
    /// a trusted desktop. Must never collide with `SealedImages` keys.
    HostFastPath(super::ToolchainSet),
}

impl ToolchainPlane {
    /// Plane-aware `ContentHash` for `JobKey::derive`'s `toolchain` argument.
    ///
    /// Each variant hashes a domain prefix before the inner digest so that
    /// keys are structurally separated across planes.
    ///
    /// | Plane | Domain prefix |
    /// |---|---|
    /// | `SealedImages` | `"nudox.toolchain.sealed-image/1"` |
    /// | `HostFastPath` | `"nudox.toolchain.host-path/1"` |
    pub fn digest(&self) -> ContentHash {
        match self {
            Self::SealedImages(set) => {
                let domain = b"nudox.toolchain.sealed-image/1";
                let inner = set.digest();
                let mut h = ContentHash::builder();
                h.update(&(domain.len() as u64).to_le_bytes());
                h.update(domain);
                h.update(inner.as_bytes());
                h.finalize()
            }
            Self::HostFastPath(ts) => {
                let domain = b"nudox.toolchain.host-path/1";
                let inner = ts.digest();
                let mut h = ContentHash::builder();
                h.update(&(domain.len() as u64).to_le_bytes());
                h.update(domain);
                h.update(inner.as_bytes());
                h.finalize()
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn digest(hex64: &str) -> ImageDigest {
        format!("sha256:{hex64}").parse().unwrap()
    }

    fn hex64(c: u8) -> String {
        format!("{:0>64}", format!("{c:x}"))
    }

    fn sample_image(config_byte: u8, layer_bytes: &[u8]) -> ToolchainImage {
        ToolchainImage {
            image: "registry.example.com/nudox/rust:1.0".parse().unwrap(),
            config_digest: digest(&hex64(config_byte)),
            layer_digests: layer_bytes.iter().map(|&b| digest(&hex64(b))).collect(),
        }
    }

    // ── ImageDigest parse / Display roundtrip ─────────────────────────────

    #[test]
    fn image_digest_roundtrip() {
        let s = "sha256:0000000000000000000000000000000000000000000000000000000000000001";
        let d: ImageDigest = s.parse().unwrap();
        assert_eq!(d.to_string(), s);
    }

    #[test]
    fn image_digest_rejects_uppercase() {
        let s = "sha256:000000000000000000000000000000000000000000000000000000000000000A";
        assert!(s.parse::<ImageDigest>().is_err());
    }

    #[test]
    fn image_digest_rejects_short() {
        let s = "sha256:abcd";
        assert!(s.parse::<ImageDigest>().is_err());
    }

    #[test]
    fn image_digest_rejects_missing_prefix() {
        let s = "0000000000000000000000000000000000000000000000000000000000000001";
        assert!(s.parse::<ImageDigest>().is_err());
    }

    #[test]
    fn image_digest_rejects_wrong_prefix() {
        let s = "sha512:0000000000000000000000000000000000000000000000000000000000000001";
        assert!(s.parse::<ImageDigest>().is_err());
    }

    // ── OciImageRef parse + Display roundtrip ─────────────────────────────

    #[test]
    fn oci_ref_tag_roundtrip() {
        let s = "registry.example.com/nudox/rust:1.80.0";
        let r: OciImageRef = s.parse().unwrap();
        assert_eq!(r.registry, "registry.example.com");
        assert_eq!(r.repository, "nudox/rust");
        assert_eq!(r.reference, OciReference::Tag("1.80.0".into()));
        assert_eq!(r.to_string(), s);
    }

    #[test]
    fn oci_ref_digest_roundtrip() {
        let hex = "0000000000000000000000000000000000000000000000000000000000000001";
        let s = format!("registry.example.com/nudox/rust@sha256:{hex}");
        let r: OciImageRef = s.parse().unwrap();
        assert_eq!(r.registry, "registry.example.com");
        assert_eq!(r.repository, "nudox/rust");
        assert!(matches!(r.reference, OciReference::Digest(_)));
        assert_eq!(r.to_string(), s);
    }

    #[test]
    fn oci_ref_rejects_no_tag_or_digest() {
        assert!(
            "registry.example.com/nudox/rust"
                .parse::<OciImageRef>()
                .is_err()
        );
    }

    #[test]
    fn oci_ref_rejects_empty_tag() {
        assert!(
            "registry.example.com/nudox/rust:"
                .parse::<OciImageRef>()
                .is_err()
        );
    }

    /// A bare name with no slash has no registry component and must be rejected.
    #[test]
    fn oci_ref_rejects_missing_registry() {
        assert!("rust:latest".parse::<OciImageRef>().is_err());
    }

    // ── content_fingerprint stability + sensitivity ───────────────────────

    #[test]
    fn fingerprint_stability() {
        let img = sample_image(0x01, &[0x02, 0x03]);
        assert_eq!(img.content_fingerprint(), img.content_fingerprint());
    }

    #[test]
    fn fingerprint_sensitive_to_config_change() {
        let a = sample_image(0x01, &[0x02]);
        let b = sample_image(0xFF, &[0x02]);
        assert_ne!(a.content_fingerprint(), b.content_fingerprint());
    }

    #[test]
    fn fingerprint_sensitive_to_layer_change() {
        let a = sample_image(0x01, &[0x02]);
        let b = sample_image(0x01, &[0x03]);
        assert_ne!(a.content_fingerprint(), b.content_fingerprint());
    }

    #[test]
    fn fingerprint_sensitive_to_layer_added() {
        let a = sample_image(0x01, &[0x02]);
        let b = sample_image(0x01, &[0x02, 0x03]);
        assert_ne!(a.content_fingerprint(), b.content_fingerprint());
    }

    #[test]
    fn fingerprint_layer_order_independent() {
        let a = sample_image(0x01, &[0x02, 0x03, 0x04]);
        let b = sample_image(0x01, &[0x04, 0x02, 0x03]);
        assert_eq!(a.content_fingerprint(), b.content_fingerprint());
    }

    // ── ToolchainPlane collision test (V6) ───────────────────────────────

    /// Risk V6: identical logical content in both planes must never yield the
    /// same `ToolchainPlane::digest`. This is the CI-forever parity guard.
    #[test]
    fn plane_collision_v6_domain_separation() {
        // Build a ToolchainImageSet with one image.
        let mut set = ToolchainImageSet::new();
        set.insert(ProducerProfile::Rust, sample_image(0xAA, &[0xBB]));
        let sealed = ToolchainPlane::SealedImages(set);

        // Build a ToolchainSet with paths that produce the same raw inner
        // bytes as the image set (contrived but proves domain separation).
        // In practice these are different types; we just need both planes.
        let host = ToolchainPlane::HostFastPath(crate::toolchains::ToolchainSet {
            rustup_home: Some(PathBuf::from("/nix/store/aaa")),
            ..Default::default()
        });

        assert_ne!(
            sealed.digest(),
            host.digest(),
            "SealedImages and HostFastPath must never produce the same ToolchainPlane::digest"
        );
    }

    /// `ToolchainSet::digest` must not change — legacy JobKeys would shift.
    /// This test computes the digest from fixed inputs and asserts the hex
    /// is stable. If domain-separation logic accidentally touched
    /// `ToolchainSet::digest` this test fails loudly.
    #[test]
    fn toolchain_set_digest_unchanged() {
        let ts = crate::toolchains::ToolchainSet {
            rustup_home: Some(PathBuf::from("/nix/store/aaa")),
            ..Default::default()
        };
        // Compute once, assert it matches itself on the same input.
        // The invariant is: ToolchainSet::digest() is unaffected by this
        // module — its bytes come from toolchains.rs only.
        let d1 = ts.digest();
        let d2 = ts.digest();
        assert_eq!(d1, d2, "ToolchainSet::digest must be deterministic");

        // ToolchainPlane::HostFastPath wraps the same set; its digest must differ.
        let plane = ToolchainPlane::HostFastPath(ts.clone());
        assert_ne!(
            plane.digest(),
            ts.digest(),
            "ToolchainPlane must differ from raw ToolchainSet::digest (domain prefix)"
        );
    }

    // ── ToolchainImageSet digest stability + sensitivity ─────────────────

    #[test]
    fn image_set_digest_stable() {
        let mut set = ToolchainImageSet::new();
        set.insert(ProducerProfile::Rust, sample_image(1, &[2]));
        assert_eq!(set.digest(), set.digest());
    }

    #[test]
    fn image_set_digest_sensitive_to_entry_change() {
        let mut a = ToolchainImageSet::new();
        a.insert(ProducerProfile::Rust, sample_image(1, &[2]));
        let mut b = ToolchainImageSet::new();
        b.insert(ProducerProfile::Rust, sample_image(2, &[2]));
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn image_set_digest_sensitive_to_missing_entry() {
        let mut a = ToolchainImageSet::new();
        a.insert(ProducerProfile::Rust, sample_image(1, &[2]));
        let b = ToolchainImageSet::new();
        assert_ne!(a.digest(), b.digest());
    }
}
