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
    ContentPersonalization(*b"nudox.content.identity.v1");
pub(crate) const ARTIFACT_PERSONALIZATION: ArtifactPersonalization =
    ArtifactPersonalization(*b"nudox.artifact.identity.v1");

impl AsRef<[u8]> for ContentPersonalization {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for ArtifactPersonalization {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

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
