//! Borrowed parsing of the two Maven package-URL spellings used by Java.

use thiserror::Error;

/// A validated Maven coordinate triple borrowed from its source PURL.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MavenCoordinates<'purl> {
    /// Maven group, including any internal dots.
    pub group: &'purl str,
    /// Maven artifact identifier.
    pub artifact: &'purl str,
    /// Maven version.
    pub version: &'purl str,
}

/// A rejected PURL and the first byte at which its grammar failed.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum PurlError<'purl> {
    /// The input does not use either supported Maven scheme spelling.
    #[error("unsupported Maven PURL scheme or shape at byte {position}: {input}")]
    UnsupportedScheme {
        /// The complete rejected source.
        input: &'purl str,
        /// The first rejected byte offset.
        position: usize,
    },
    /// A group or artifact segment is empty or contains a forbidden byte.
    #[error("malformed Maven coordinate segment at byte {position}: {input}")]
    MalformedSegment {
        /// The complete rejected source.
        input: &'purl str,
        /// The first rejected byte offset.
        position: usize,
    },
    /// The version is empty or contains a forbidden byte.
    #[error("malformed Maven version at byte {position}: {input}")]
    MalformedVersion {
        /// The complete rejected source.
        input: &'purl str,
        /// The first rejected byte offset.
        position: usize,
    },
}

impl<'purl> MavenCoordinates<'purl> {
    /// Parses `maven:group:artifact@version` or `pkg:maven/group/artifact@version`.
    pub fn parse(input: &'purl str) -> Result<Self, PurlError<'purl>> {
        let (body, prefix_len, separator) = if let Some(body) = input.strip_prefix("maven:") {
            (body, "maven:".len(), ':')
        } else if let Some(body) = input.strip_prefix("pkg:maven/") {
            (body, "pkg:maven/".len(), '/')
        } else {
            return Err(PurlError::UnsupportedScheme { input, position: 0 });
        };

        let at = body.find('@').ok_or(PurlError::MalformedVersion {
            input,
            position: input.len(),
        })?;
        let version_start = prefix_len + at + 1;
        let version = &body[at + 1..];
        if version.is_empty() {
            return Err(PurlError::MalformedVersion {
                input,
                position: version_start,
            });
        }
        if let Some(offset) = version.bytes().position(|byte| !is_version_byte(byte)) {
            return Err(PurlError::MalformedVersion {
                input,
                position: version_start + offset,
            });
        }

        let coordinate = &body[..at];
        let separator_offset = coordinate
            .find(separator)
            .ok_or(PurlError::MalformedSegment {
                input,
                position: prefix_len + coordinate.len(),
            })?;
        let group = &coordinate[..separator_offset];
        let artifact = &coordinate[separator_offset + 1..];
        validate_segment(input, prefix_len, group)?;
        validate_segment(input, prefix_len + separator_offset + 1, artifact)?;

        Ok(Self {
            group,
            artifact,
            version,
        })
    }
}

impl<'purl> TryFrom<&'purl str> for MavenCoordinates<'purl> {
    type Error = PurlError<'purl>;

    fn try_from(input: &'purl str) -> Result<Self, Self::Error> {
        Self::parse(input)
    }
}

fn validate_segment<'purl>(
    input: &'purl str,
    start: usize,
    segment: &str,
) -> Result<(), PurlError<'purl>> {
    if segment.is_empty() {
        return Err(PurlError::MalformedSegment {
            input,
            position: start,
        });
    }
    if segment.starts_with('.') || segment.ends_with('.') {
        let position = if segment.starts_with('.') {
            start
        } else {
            start + segment.len() - 1
        };
        return Err(PurlError::MalformedSegment { input, position });
    }
    if let Some(offset) = segment.bytes().position(|byte| !is_segment_byte(byte)) {
        return Err(PurlError::MalformedSegment {
            input,
            position: start + offset,
        });
    }
    Ok(())
}

fn is_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn is_version_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
}
