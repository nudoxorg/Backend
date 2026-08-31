//! Fixed widths, typed BLAKE3 personalizations, and allocation-free hexadecimal formatting.
//! Personalization values remain typed so content and artifact kernels cannot be interchanged.
//! Formatting writes directly into the caller's formatter without staging a temporary string.
/// The byte width of every cryptographic identity in this crate.
pub const HASH_BYTES: usize = 32;

/// Fixed byte width of every protocol-owned domain and encoding label.
pub const TAG_BYTES: usize = 16;

/// Typed BLAKE3 personalization for logical content identities.
#[repr(transparent)]
pub(crate) struct ContentPersonalization([u8; 25]);

/// Typed BLAKE3 personalization for encoded artifact identities.
#[repr(transparent)]
pub(crate) struct ArtifactPersonalization([u8; 26]);

pub(crate) const CONTENT_PERSONALIZATION: ContentPersonalization =
    ContentPersonalization(*b"heart.content.identity.v1");
pub(crate) const ARTIFACT_PERSONALIZATION: ArtifactPersonalization =
    ArtifactPersonalization(*b"heart.artifact.identity.v1");

impl AsRef<[u8]> for ContentPersonalization {
    /// Borrows the exact content-identity personalization bytes.
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for ArtifactPersonalization {
    /// Borrows the exact artifact-identity personalization bytes.
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Writes one complete identity as lowercase hexadecimal without allocating.
pub(crate) fn write_hex(
    formatter: &mut fmt::Formatter<'_>,
    bytes: &[u8; HASH_BYTES],
) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}
use core::fmt;
