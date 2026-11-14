//! Runtime verification of retained exact binding evidence against host bytes.
//!
//! Binding offsets persisted by the syntax pipeline are claims about one input
//! revision. They become source extents only after both input and semantic
//! content identities and UTF-8 boundaries have been checked against the host.

use crate::{InputContentSchema, InputContentVersion, SourceBindingCapture, typed_of};
use backend_version::{ContentId, SourceFactDomain};
use std::{fmt, ops::Range};

/// Why exact source evidence could not be verified against a host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceVerificationError {
    /// Host bytes are not valid UTF-8.
    NonUtf8,
    /// Host bytes do not match the expected input-content identity.
    InputRevisionMismatch,
    /// Host bytes do not match the expected semantic source identity.
    SourceIdentityMismatch,
    /// A claimed range is reversed, out of bounds, or not on UTF-8 boundaries.
    InvalidExtent,
}

impl fmt::Display for SourceVerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NonUtf8 => "source is not UTF-8",
            Self::InputRevisionMismatch => "source input revision does not match",
            Self::SourceIdentityMismatch => "semantic source identity does not match",
            Self::InvalidExtent => "source extent is not a valid UTF-8 byte range",
        })
    }
}
impl std::error::Error for SourceVerificationError {}

/// Actual source text admitted against both domain-specific host identities.
#[derive(Clone, Debug)]
pub struct VerifiedSourceText<'a> {
    text: &'a str,
    content: InputContentVersion,
    source: ContentId<SourceFactDomain>,
}
impl<'a> VerifiedSourceText<'a> {
    /// Mints both source identities from actual UTF-8 host bytes.
    ///
    /// # Errors
    /// Returns [`SourceVerificationError::NonUtf8`] for non-UTF-8 bytes.
    pub fn capture(bytes: &'a [u8]) -> Result<Self, SourceVerificationError> {
        let text = std::str::from_utf8(bytes).map_err(|_| SourceVerificationError::NonUtf8)?;
        Ok(Self {
            text,
            content: typed_of::<InputContentSchema>(bytes),
            source: ContentId::<SourceFactDomain>::from_canonical_bytes(bytes),
        })
    }

    /// Identity minted from the actual host input.
    #[must_use]
    pub const fn content(&self) -> InputContentVersion {
        self.content
    }

    /// Verifies actual host bytes, without reconstructing a typed input identity
    /// from an untrusted digest.
    ///
    /// # Errors
    /// Returns an error when either identity or UTF-8 validation fails.
    pub fn admit(
        bytes: &'a [u8],
        expected_input: InputContentVersion,
        expected_source: ContentId<SourceFactDomain>,
    ) -> Result<Self, SourceVerificationError> {
        Self::admit_stored(bytes, expected_input.to_bytes(), expected_source)
    }

    /// Verifies an untrusted persisted input digest by hashing actual bytes first.
    /// A typed input identity is returned only from those bytes, never from
    /// the untrusted stored digest. The caller must first authenticate that
    /// both expected identities belong to the selected source record; this
    /// method does not admit a path, row, or workspace on its own.
    ///
    /// # Errors
    /// Returns an error when either identity or UTF-8 validation fails.
    pub fn admit_stored(
        bytes: &'a [u8],
        expected_input: [u8; 32],
        expected_source: ContentId<SourceFactDomain>,
    ) -> Result<Self, SourceVerificationError> {
        let captured = Self::capture(bytes)?;
        if captured.content.to_bytes() != expected_input {
            return Err(SourceVerificationError::InputRevisionMismatch);
        }
        if captured.source != expected_source {
            return Err(SourceVerificationError::SourceIdentityMismatch);
        }
        Ok(captured)
    }

    /// Checks both observed owner and name extents against these host bytes.
    /// This proves coordinates only; it does not independently establish that
    /// the name text denotes a declaration or matches a display name. A
    /// page-relative binding must first be restored to absolute host offsets
    /// using its authenticated page base; bounds checks alone cannot detect
    /// an omitted page base.
    ///
    /// # Errors
    /// Returns [`SourceVerificationError::InvalidExtent`] when any endpoint is
    /// outside the host or lies within a UTF-8 code point.
    pub fn binding(
        &self,
        capture: SourceBindingCapture,
    ) -> Result<VerifiedSourceBinding<'a>, SourceVerificationError> {
        let (owner_start, owner_end) = capture.owner();
        let (name_start, name_end) = capture.name();
        Ok(VerifiedSourceBinding {
            owner: self.extent(owner_start as usize..owner_end as usize)?,
            name: self.extent(name_start as usize..name_end as usize)?,
        })
    }

    /// Checks a half-open byte range in the verified host.
    ///
    /// This establishes only source coordinates and UTF-8 boundaries. The
    /// caller remains responsible for proving the meaning and provenance of
    /// the range; in particular, page-relative offsets require restoration.
    ///
    /// # Errors
    /// Returns [`SourceVerificationError::InvalidExtent`] for reversed,
    /// out-of-bounds, or non-UTF-8-boundary offsets.
    pub fn extent(
        &self,
        range: Range<usize>,
    ) -> Result<VerifiedSourceExtent<'a>, SourceVerificationError> {
        let text = self
            .text
            .get(range.clone())
            .ok_or(SourceVerificationError::InvalidExtent)?;
        Ok(VerifiedSourceExtent {
            text,
            content: self.content,
            source: self.source,
            start: range.start,
            end: range.end,
        })
    }
}

/// Verified owner and name ranges for one retained source binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSourceBinding<'a> {
    owner: VerifiedSourceExtent<'a>,
    name: VerifiedSourceExtent<'a>,
}
impl<'a> VerifiedSourceBinding<'a> {
    /// Exact declaration owner extent, which may differ from an excerpt.
    #[must_use]
    pub const fn owner(&self) -> &VerifiedSourceExtent<'a> {
        &self.owner
    }
    /// Exact captured-name extent.
    #[must_use]
    pub const fn name(&self) -> &VerifiedSourceExtent<'a> {
        &self.name
    }
}

/// A checked half-open host-byte range in two verified content domains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSourceExtent<'a> {
    text: &'a str,
    content: InputContentVersion,
    source: ContentId<SourceFactDomain>,
    start: usize,
    end: usize,
}
impl<'a> VerifiedSourceExtent<'a> {
    /// Exact bytes interpreted as UTF-8.
    #[must_use]
    pub const fn text(&self) -> &'a str {
        self.text
    }
    /// Half-open offsets into the complete host input.
    #[must_use]
    pub const fn byte_range(&self) -> Range<usize> {
        self.start..self.end
    }
    /// Exact input revision.
    #[must_use]
    pub const fn content(&self) -> InputContentVersion {
        self.content
    }
    /// Exact semantic source identity.
    #[must_use]
    pub const fn source(&self) -> ContentId<SourceFactDomain> {
        self.source
    }

    /// Maps a child byte range to the same host only when it is a verbatim,
    /// contiguous slice. Decoded or generated text requires a source map.
    ///
    /// # Errors
    /// Rejects invalid, non-UTF-8-boundary, or overflowing child ranges.
    pub fn subregion(&self, relative: Range<usize>) -> Result<Self, SourceVerificationError> {
        let text = self
            .text
            .get(relative.clone())
            .ok_or(SourceVerificationError::InvalidExtent)?;
        let start = self
            .start
            .checked_add(relative.start)
            .ok_or(SourceVerificationError::InvalidExtent)?;
        let end = self
            .start
            .checked_add(relative.end)
            .ok_or(SourceVerificationError::InvalidExtent)?;
        Ok(Self {
            text,
            content: self.content,
            source: self.source,
            start,
            end,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identities(bytes: &[u8]) -> (InputContentVersion, ContentId<SourceFactDomain>) {
        (
            typed_of::<InputContentSchema>(bytes),
            ContentId::from_canonical_bytes(bytes),
        )
    }

    #[test]
    fn verifies_host_and_binding_boundaries_before_mapping() {
        let bytes = "// π\r\nconst café = '😀';\r\n".as_bytes();
        let (input, source) = identities(bytes);
        let verified = VerifiedSourceText::admit(bytes, input, source).expect("host");
        assert!(VerifiedSourceText::admit_stored(bytes, input.to_bytes(), source).is_ok());
        assert_eq!(
            VerifiedSourceText::admit_stored(bytes, [0; 32], source).unwrap_err(),
            SourceVerificationError::InputRevisionMismatch
        );
        let host = std::str::from_utf8(bytes).expect("utf8");
        let owner_start = host.find("const").expect("owner");
        let owner_end = host.find(';').expect("end") + 1;
        let name_start = host.find("café").expect("name");
        let name_end = name_start + "café".len();
        let capture = SourceBindingCapture::new(
            owner_start as u32,
            owner_end as u32,
            name_start as u32,
            name_end as u32,
        )
        .expect("capture");
        assert_eq!(
            verified
                .extent(name_start..name_end)
                .expect("exact host range")
                .text(),
            "café"
        );
        assert!(verified.extent(bytes.len()..bytes.len()).is_ok());
        assert!(verified.extent(4..4).is_err());
        assert!(verified.extent(0..bytes.len() + 1).is_err());
        assert!(verified.extent(2..1).is_err());
        let binding = verified.binding(capture).expect("binding");
        assert_eq!(binding.owner().text(), "const café = '😀';");
        assert_eq!(binding.name().text(), "café");
        assert_eq!(binding.name().byte_range(), name_start..name_end);
        assert_eq!(
            binding.owner().subregion(6..11).expect("subregion").text(),
            "café"
        );
        assert!(binding.owner().subregion(9..10).is_err());
        assert!(binding.owner().subregion(0..usize::MAX).is_err());
        assert!(binding.owner().subregion(8..7).is_err());
        assert!(binding.owner().subregion(0..0).is_ok());
        let mut invalid = bytes.to_vec();
        invalid[3] = b'x';
        invalid[4] = b'x';
        assert_eq!(invalid.len(), bytes.len());
        assert_eq!(
            VerifiedSourceText::admit(&invalid, input, source).unwrap_err(),
            SourceVerificationError::InputRevisionMismatch
        );
        let (other_input, _) = identities(&invalid);
        assert_eq!(
            VerifiedSourceText::admit(&invalid, other_input, source).unwrap_err(),
            SourceVerificationError::SourceIdentityMismatch
        );
        let invalid_utf8 = [0xff];
        let (binary_input, binary_source) = identities(&invalid_utf8);
        assert_eq!(
            VerifiedSourceText::admit(&invalid_utf8, binary_input, binary_source).unwrap_err(),
            SourceVerificationError::NonUtf8
        );
        assert!(SourceBindingCapture::new(10, 20, 9, 11).is_none());
        assert!(SourceBindingCapture::new(20, 10, 12, 13).is_none());
        let inside_name = SourceBindingCapture::new(
            owner_start as u32,
            owner_end as u32,
            (name_end - 1) as u32,
            name_end as u32,
        )
        .expect("ordered claim");
        assert_eq!(
            verified.binding(inside_name).unwrap_err(),
            SourceVerificationError::InvalidExtent
        );
        let outside_owner = SourceBindingCapture::new(
            owner_start as u32,
            u32::MAX,
            name_start as u32,
            name_end as u32,
        )
        .expect("ordered claim");
        assert_eq!(
            verified.binding(outside_owner).unwrap_err(),
            SourceVerificationError::InvalidExtent
        );
        let inside_owner =
            SourceBindingCapture::new(4, owner_end as u32, name_start as u32, name_end as u32)
                .expect("ordered claim");
        assert_eq!(
            verified.binding(inside_owner).unwrap_err(),
            SourceVerificationError::InvalidExtent
        );
    }
}
