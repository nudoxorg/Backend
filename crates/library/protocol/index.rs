//! Wire-side vocabulary for local-first immutable-index synchronization.
//!
//! This module deliberately models received source coordinates as untrusted transport data. The
//! only owned source-span fact eligible for server serialization is constructed in the canonical
//! retrieval proof boundary; a routed search hit or a document candidate has no conversion here.

use core::fmt;

use backend_version::{HASH_BYTES, IndexSnapshotDomain, IndexSnapshotId};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, SeqAccess, Visitor},
    ser::SerializeStruct,
};

use crate::protocol::{CANONICAL_CONTENT_ID_TEXT_BYTES, CanonicalContentId};

/// Exact fixed transport width of an entity-document correlation identity.
pub const UNTRUSTED_DOCUMENT_ID_BYTES: usize = HASH_BYTES + 4;
/// Largest path retained in one source-span transport record.
pub const MAX_UNTRUSTED_SOURCE_PATH_BYTES: usize = 4096;

/// Structurally valid opaque document bytes accompanying an untrusted remote source coordinate.
///
/// This type deliberately does not claim that the document identity came from a canonical image;
/// only the server retrieval proof boundary may make that claim before serialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UntrustedDocumentId([u8; UNTRUSTED_DOCUMENT_ID_BYTES]);

impl UntrustedDocumentId {
    /// Wraps fixed-width document bytes produced by a trusted server-side serialization boundary.
    #[must_use]
    pub const fn from_fixed_bytes(bytes: [u8; UNTRUSTED_DOCUMENT_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// Admits exactly one fixed-width remote document correlation value.
    ///
    /// # Errors
    ///
    /// Returns [`UntrustedDocumentIdError::Width`] unless `bytes` has the exact wire width.
    pub const fn from_encoded_bytes(bytes: &[u8]) -> Result<Self, UntrustedDocumentIdError> {
        if bytes.len() != UNTRUSTED_DOCUMENT_ID_BYTES {
            return Err(UntrustedDocumentIdError::Width {
                actual: bytes.len(),
                expected: UNTRUSTED_DOCUMENT_ID_BYTES,
            });
        }
        let mut encoded = [0; UNTRUSTED_DOCUMENT_ID_BYTES];
        encoded.copy_from_slice(bytes);
        Ok(Self(encoded))
    }

    /// Returns the exact opaque document-correlation bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; UNTRUSTED_DOCUMENT_ID_BYTES] {
        &self.0
    }
}

/// Structural rejection for an opaque remote document correlation identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UntrustedDocumentIdError {
    /// The remote document field did not have the fixed wire width.
    Width {
        /// Number of bytes received.
        actual: usize,
        /// Exact required wire width.
        expected: usize,
    },
}

impl fmt::Display for UntrustedDocumentIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Width { actual, expected } => {
                write!(
                    formatter,
                    "document correlation has {actual} bytes, expected {expected}"
                )
            }
        }
    }
}

impl std::error::Error for UntrustedDocumentIdError {}

/// Received source-coordinate bytes that have only structural wire validation.
///
/// This value is intentionally not a trusted source fact. A client may associate it with a
/// pinned remote response by `snapshot` and `document`, but it cannot be converted into canonical
/// source provenance by this crate.
#[derive(Debug, Eq, PartialEq)]
pub struct UntrustedSourceSpan {
    path: Vec<u8>,
    start: u32,
    end: u32,
    snapshot: IndexSnapshotId,
    document: UntrustedDocumentId,
}

/// Structural source-span wire rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UntrustedSourceSpanError {
    /// The received path exceeded the bounded source-span wire contract.
    PathLength {
        /// Number of received path bytes.
        actual: usize,
        /// Largest retained source path.
        maximum: usize,
    },
    /// A half-open coordinate ended before it started.
    Reversed {
        /// Received inclusive start coordinate.
        start: u32,
        /// Received exclusive end coordinate.
        end: u32,
    },
}

/// Explicit rejection when an untrusted source span cannot join the caller's pinned response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UntrustedSourceSpanAuthorityError {
    /// The span was carried by a different remote snapshot than the pinned response.
    ForeignSnapshot {
        /// Snapshot authority pinned by the caller.
        expected: IndexSnapshotId,
        /// Snapshot authority carried by the untrusted transport record.
        observed: IndexSnapshotId,
    },
}

impl fmt::Display for UntrustedSourceSpanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathLength { actual, maximum } => {
                write!(
                    formatter,
                    "source path has {actual} bytes, maximum {maximum}"
                )
            }
            Self::Reversed { start, end } => {
                write!(
                    formatter,
                    "source span ends at {end} before it starts at {start}"
                )
            }
        }
    }
}

impl std::error::Error for UntrustedSourceSpanError {}

impl UntrustedSourceSpan {
    /// Validates a structurally legal received half-open coordinate without asserting source proof.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the path exceeds the bounded wire contract or the span reverses.
    pub fn new(
        path: Vec<u8>,
        start: u32,
        end: u32,
        snapshot: IndexSnapshotId,
        document: UntrustedDocumentId,
    ) -> Result<Self, UntrustedSourceSpanError> {
        if path.len() > MAX_UNTRUSTED_SOURCE_PATH_BYTES {
            return Err(UntrustedSourceSpanError::PathLength {
                actual: path.len(),
                maximum: MAX_UNTRUSTED_SOURCE_PATH_BYTES,
            });
        }
        if start > end {
            return Err(UntrustedSourceSpanError::Reversed { start, end });
        }
        Ok(Self {
            path,
            start,
            end,
            snapshot,
            document,
        })
    }

    /// Returns the exact received path bytes, which may not be UTF-8.
    #[must_use]
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Returns the received inclusive byte offset.
    #[must_use]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// Returns the received exclusive byte offset.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.end
    }

    /// Returns the remote pinned snapshot with which the span was correlated.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the opaque remote document correlation value.
    #[must_use]
    pub const fn document(&self) -> &UntrustedDocumentId {
        &self.document
    }

    /// Requires this remote transport record to match an already pinned response snapshot.
    ///
    /// This confirms only response correlation; it never upgrades received coordinates into
    /// canonical source proof.
    ///
    /// # Errors
    ///
    /// Returns [`UntrustedSourceSpanAuthorityError::ForeignSnapshot`] for a foreign response.
    pub fn require_snapshot(
        &self,
        expected: IndexSnapshotId,
    ) -> Result<(), UntrustedSourceSpanAuthorityError> {
        if self.snapshot != expected {
            return Err(UntrustedSourceSpanAuthorityError::ForeignSnapshot {
                expected,
                observed: self.snapshot,
            });
        }
        Ok(())
    }
}

impl Serialize for UntrustedSourceSpan {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        let mut state = serializer.serialize_struct("UntrustedSourceSpan", 5)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("start", &self.start)?;
        state.serialize_field("end", &self.end)?;
        state.serialize_field("snapshot", &SnapshotDisplay(self.snapshot))?;
        let document: &[u8] = self.document.as_bytes();
        state.serialize_field("document", document)?;
        state.end()
    }
}

struct SnapshotDisplay(IndexSnapshotId);

impl Serialize for SnapshotDisplay {
    fn serialize<Output>(&self, serializer: Output) -> Result<Output::Ok, Output::Error>
    where
        Output: Serializer,
    {
        serializer.collect_str(&CanonicalContentId::<IndexSnapshotDomain>::from(self.0))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UntrustedSourceSpanWire {
    path: BoundedSourcePath,
    start: u32,
    end: u32,
    snapshot: SnapshotText,
    document: FixedDocument,
}

impl<'de> Deserialize<'de> for UntrustedSourceSpan {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        let wire = UntrustedSourceSpanWire::deserialize(deserializer)?;
        Self::new(
            wire.path.0,
            wire.start,
            wire.end,
            wire.snapshot.0,
            UntrustedDocumentId::from_fixed_bytes(wire.document.0),
        )
        .map_err(Input::Error::custom)
    }
}

struct BoundedSourcePath(Vec<u8>);

impl<'de> Deserialize<'de> for BoundedSourcePath {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        deserializer.deserialize_seq(BoundedSourcePathVisitor)
    }
}

struct BoundedSourcePathVisitor;

impl<'de> Visitor<'de> for BoundedSourcePathVisitor {
    type Value = BoundedSourcePath;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "at most {MAX_UNTRUSTED_SOURCE_PATH_BYTES} source-path bytes"
        )
    }

    fn visit_seq<Input>(self, mut sequence: Input) -> Result<Self::Value, Input::Error>
    where
        Input: SeqAccess<'de>,
    {
        let initial = sequence
            .size_hint()
            .unwrap_or(0)
            .min(MAX_UNTRUSTED_SOURCE_PATH_BYTES);
        let mut path = Vec::new();
        path.try_reserve_exact(initial)
            .map_err(Input::Error::custom)?;
        while let Some(byte) = sequence.next_element::<u8>()? {
            if path.len() == MAX_UNTRUSTED_SOURCE_PATH_BYTES {
                return Err(Input::Error::invalid_length(
                    MAX_UNTRUSTED_SOURCE_PATH_BYTES + 1,
                    &self,
                ));
            }
            if path.len() == path.capacity() {
                let next_capacity = path
                    .capacity()
                    .max(64)
                    .saturating_mul(2)
                    .min(MAX_UNTRUSTED_SOURCE_PATH_BYTES);
                path.try_reserve_exact(next_capacity - path.capacity())
                    .map_err(Input::Error::custom)?;
            }
            path.push(byte);
        }
        Ok(BoundedSourcePath(path))
    }
}

struct FixedDocument([u8; UNTRUSTED_DOCUMENT_ID_BYTES]);

impl<'de> Deserialize<'de> for FixedDocument {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        deserializer.deserialize_seq(FixedDocumentVisitor)
    }
}

struct FixedDocumentVisitor;

impl<'de> Visitor<'de> for FixedDocumentVisitor {
    type Value = FixedDocument;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "exactly {UNTRUSTED_DOCUMENT_ID_BYTES} document bytes"
        )
    }

    fn visit_seq<Input>(self, mut sequence: Input) -> Result<Self::Value, Input::Error>
    where
        Input: SeqAccess<'de>,
    {
        let mut document = [0; UNTRUSTED_DOCUMENT_ID_BYTES];
        let mut index = 0;
        while let Some(byte) = sequence.next_element::<u8>()? {
            let Some(slot) = document.get_mut(index) else {
                return Err(Input::Error::invalid_length(index + 1, &self));
            };
            *slot = byte;
            index += 1;
        }
        if index != UNTRUSTED_DOCUMENT_ID_BYTES {
            return Err(Input::Error::invalid_length(index, &self));
        }
        Ok(FixedDocument(document))
    }
}

struct SnapshotText(IndexSnapshotId);

impl<'de> Deserialize<'de> for SnapshotText {
    fn deserialize<Input>(deserializer: Input) -> Result<Self, Input::Error>
    where
        Input: Deserializer<'de>,
    {
        deserializer.deserialize_str(SnapshotTextVisitor)
    }
}

struct SnapshotTextVisitor;

impl<'de> Visitor<'de> for SnapshotTextVisitor {
    type Value = SnapshotText;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a {CANONICAL_CONTENT_ID_TEXT_BYTES}-byte canonical snapshot identity"
        )
    }

    fn visit_str<Error>(self, snapshot: &str) -> Result<Self::Value, Error>
    where
        Error: serde::de::Error,
    {
        if snapshot.len() != CANONICAL_CONTENT_ID_TEXT_BYTES {
            return Err(Error::invalid_length(snapshot.len(), &self));
        }
        CanonicalContentId::<IndexSnapshotDomain>::try_from(snapshot)
            .map(IndexSnapshotId::from)
            .map(SnapshotText)
            .map_err(Error::custom)
    }

    fn visit_borrowed_str<Error>(self, snapshot: &'de str) -> Result<Self::Value, Error>
    where
        Error: serde::de::Error,
    {
        self.visit_str(snapshot)
    }
}
