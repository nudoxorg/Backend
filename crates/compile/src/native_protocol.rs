//! Canonical payloads exchanged with native authority helpers.
//!
//! The process/session framing in [`crate::frame`] proves that a peer is
//! speaking the authority session protocol.  This module proves the separate
//! semantic payload contract carried by a cold process (or by a persistent
//! helper's payload channel).  In particular, an arbitrary compiler message
//! can never become a fact merely because it was written to stdout.

use backend_semantic::FacetKind;
use std::fmt;

#[path = "native_protocol_admission.rs"]
mod admission;
#[path = "native_protocol_codec.rs"]
mod codec;
#[path = "native_request.rs"]
mod message;
#[path = "native_protocol_session.rs"]
mod session;
pub use admission::{Bound, EnvelopeState, NativeEnvelope, Unbound};
pub use message::{NativeRequest, NativeRequestInput};

#[cfg(test)]
#[path = "native_protocol_tests.rs"]
mod tests;

/// Version of the canonical native payload envelope.
pub const NATIVE_PAYLOAD_VERSION: u16 = 1;
/// Maximum number of records admitted from one authority response.
pub const MAX_NATIVE_RECORDS: usize = 4_096;
/// Maximum encoded bytes admitted for one authority response.
pub const MAX_NATIVE_PAYLOAD_BYTES: usize = 256 * 1024;
/// Maximum encoded bytes in one record key.
pub const MAX_NATIVE_KEY_BYTES: usize = 4 * 1024;
/// Maximum encoded bytes in one record value.
pub const MAX_NATIVE_VALUE_BYTES: usize = 64 * 1024;
/// Maximum number of input fields in one helper request.
pub const MAX_NATIVE_INPUTS: usize = 256;
/// Maximum encoded bytes in one helper request.
pub const MAX_NATIVE_REQUEST_BYTES: usize = 256 * 1024;

const PAYLOAD_MAGIC: [u8; 4] = *b"BCN\0";
const PAYLOAD_HEADER_BYTES: usize = 118;
const RECORD_HEADER_BYTES: usize = 10;
const REQUEST_MAGIC: [u8; 4] = *b"BCQ\0";
const REQUEST_HEADER_BYTES: usize = 106;

/// Coverage claimed by a helper payload.
///
/// This is a payload claim, not an authority witness. The native adapter must
/// still pass a complete claim and its typed records through the independent
/// authority registry before any complete coverage can be published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeCoverage {
    /// The helper observed every fact in the requested authority scope.
    Complete,
    /// The helper observed only the records it returned.
    Partial,
}

impl NativeCoverage {
    const fn tag(self) -> u8 {
        match self {
            Self::Complete => 0,
            Self::Partial => 1,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, NativeProtocolError> {
        match tag {
            0 => Ok(Self::Complete),
            1 => Ok(Self::Partial),
            other => Err(NativeProtocolError::UnknownCoverage(other)),
        }
    }
}

/// One recognized semantic record emitted by a native authority.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NativeRecordKind {
    /// A declaration/header fact.
    Declaration = 1,
    /// A resolved or inferred type fact.
    Type = 2,
    /// A graph edge or occurrence relation.
    Edge = 3,
    /// A source/type/checking diagnostic.
    Diagnostic = 4,
    /// A positive dependency observed by the authority.
    Dependency = 5,
    /// A dependency lookup that was observed to be absent.
    NegativeDependency = 6,
}

impl NativeRecordKind {
    fn from_tag(tag: u8) -> Result<Self, NativeProtocolError> {
        match tag {
            1 => Ok(Self::Declaration),
            2 => Ok(Self::Type),
            3 => Ok(Self::Edge),
            4 => Ok(Self::Diagnostic),
            5 => Ok(Self::Dependency),
            6 => Ok(Self::NegativeDependency),
            other => Err(NativeProtocolError::UnknownRecordKind(other)),
        }
    }

    /// Returns the typed semantic family represented by this wire kind.
    #[must_use]
    pub const fn semantic_facet(self) -> FacetKind {
        match self {
            Self::Declaration => FacetKind::Entity,
            Self::Type => FacetKind::Type,
            Self::Edge => FacetKind::Edge,
            Self::Diagnostic => FacetKind::Facet,
            Self::Dependency | Self::NegativeDependency => FacetKind::Configuration,
        }
    }
}

/// A key/value native semantic record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRecord {
    kind: NativeRecordKind,
    key: String,
    value: Vec<u8>,
}

impl NativeRecord {
    /// Creates a record after checking key and value bounds.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when the key is not canonical or the
    /// value exceeds the protocol bound.
    pub fn new(
        kind: NativeRecordKind,
        key: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Result<Self, NativeProtocolError> {
        let key = key.into();
        let value = value.into();
        validate_key(&key)?;
        if value.len() > MAX_NATIVE_VALUE_BYTES {
            return Err(NativeProtocolError::ValueLimit {
                actual: value.len(),
                maximum: MAX_NATIVE_VALUE_BYTES,
            });
        }
        Ok(Self { kind, key, value })
    }

    /// Returns the recognized record family.
    #[must_use]
    pub const fn kind(&self) -> NativeRecordKind {
        self.kind
    }

    /// Returns the stable record key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the canonical payload bytes.
    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

fn validate_records(records: &[NativeRecord]) -> Result<(), NativeProtocolError> {
    if records.len() > MAX_NATIVE_RECORDS {
        return Err(NativeProtocolError::RecordCountLimit {
            actual: records.len(),
            maximum: MAX_NATIVE_RECORDS,
        });
    }
    for pair in records.windows(2) {
        let left = (pair[0].kind, pair[0].key.as_bytes());
        let right = (pair[1].kind, pair[1].key.as_bytes());
        match left.cmp(&right) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => {
                return Err(NativeProtocolError::DuplicateRecord {
                    kind: pair[0].kind,
                    key: pair[0].key.clone(),
                });
            }
            std::cmp::Ordering::Greater => {
                return Err(NativeProtocolError::NonCanonicalOrder);
            }
        }
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), NativeProtocolError> {
    if key.is_empty() {
        return Err(NativeProtocolError::EmptyKey);
    }
    if key.len() > MAX_NATIVE_KEY_BYTES {
        return Err(NativeProtocolError::KeyLimit {
            actual: key.len(),
            maximum: MAX_NATIVE_KEY_BYTES,
        });
    }
    if key.as_bytes().contains(&0) {
        return Err(NativeProtocolError::InvalidKey);
    }
    Ok(())
}

pub(super) fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<(), NativeProtocolError> {
    let value = u32::try_from(value).map_err(|_| NativeProtocolError::LengthOverflow)?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn read_u32(bytes: &[u8]) -> Result<u32, NativeProtocolError> {
    let raw: [u8; 4] = bytes
        .get(..4)
        .ok_or(NativeProtocolError::Truncated)?
        .try_into()
        .map_err(|_| NativeProtocolError::Truncated)?;
    Ok(u32::from_be_bytes(raw))
}

fn read_u16(bytes: &[u8]) -> Result<u16, NativeProtocolError> {
    let raw: [u8; 2] = bytes
        .get(..2)
        .ok_or(NativeProtocolError::Truncated)?
        .try_into()
        .map_err(|_| NativeProtocolError::Truncated)?;
    Ok(u16::from_be_bytes(raw))
}

fn read_u64(bytes: &[u8]) -> Result<u64, NativeProtocolError> {
    let raw: [u8; 8] = bytes
        .get(..8)
        .ok_or(NativeProtocolError::Truncated)?
        .try_into()
        .map_err(|_| NativeProtocolError::Truncated)?;
    Ok(u64::from_be_bytes(raw))
}

fn read_id(bytes: &[u8]) -> Result<[u8; 32], NativeProtocolError> {
    bytes.try_into().map_err(|_| NativeProtocolError::Truncated)
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    length: usize,
) -> Result<&'a [u8], NativeProtocolError> {
    let end = cursor
        .checked_add(length)
        .ok_or(NativeProtocolError::LengthOverflow)?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(NativeProtocolError::Truncated)?;
    *cursor = end;
    Ok(value)
}

pub(super) fn put_u16(output: &mut Vec<u8>, value: usize) -> Result<(), NativeProtocolError> {
    let value = u16::try_from(value).map_err(|_| NativeProtocolError::LengthOverflow)?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Strict native payload/request decoding failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeProtocolError {
    /// The input ended before the declared payload was complete.
    Truncated,
    /// The payload magic did not identify the canonical native format.
    InvalidMagic,
    /// The payload version is not understood.
    UnsupportedVersion(u16),
    /// The coverage tag is unknown.
    UnknownCoverage(u8),
    /// A reserved flags byte was non-zero.
    InvalidFlags(u8),
    /// A record kind is not recognized by this protocol version.
    UnknownRecordKind(u8),
    /// A key was empty.
    EmptyKey,
    /// A key contained a forbidden byte.
    InvalidKey,
    /// A key exceeded the configured limit.
    KeyLimit {
        /// Number of key bytes supplied by the peer.
        actual: usize,
        /// Maximum key bytes admitted by this protocol.
        maximum: usize,
    },
    /// A record value exceeded the configured limit.
    ValueLimit {
        /// Number of value bytes supplied by the peer.
        actual: usize,
        /// Maximum value bytes admitted by this protocol.
        maximum: usize,
    },
    /// The record count exceeded the configured limit.
    RecordCountLimit {
        /// Number of records supplied by the peer.
        actual: usize,
        /// Maximum records admitted by this protocol.
        maximum: usize,
    },
    /// The input field count exceeded the configured limit.
    InputCountLimit {
        /// Number of input fields supplied by the caller.
        actual: usize,
        /// Maximum input fields admitted by this protocol.
        maximum: usize,
    },
    /// A record's kind/key pair was repeated.
    DuplicateRecord {
        /// Record family whose key was repeated.
        kind: NativeRecordKind,
        /// Repeated record key.
        key: String,
    },
    /// An input field name was repeated.
    DuplicateInput {
        /// Repeated input field name.
        name: String,
    },
    /// Records were not encoded in canonical order.
    NonCanonicalOrder,
    /// A UTF-8 key was malformed.
    InvalidUtf8,
    /// Bytes remained after the declared records.
    TrailingBytes {
        /// Number of bytes after the declared record list.
        actual: usize,
    },
    /// A declared length could not be represented safely.
    LengthOverflow,
    /// A bound envelope used an authority different from its session key.
    BindingMismatch,
    /// A complete encoded payload exceeded its byte budget.
    ByteLimit {
        /// Number of encoded bytes supplied by the peer.
        actual: usize,
        /// Maximum encoded bytes admitted by this protocol.
        maximum: usize,
    },
}

impl fmt::Display for NativeProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("native payload is truncated"),
            Self::InvalidMagic => f.write_str("native payload has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "native payload version {version} is unsupported")
            }
            Self::UnknownCoverage(tag) => write!(f, "native payload coverage tag {tag} is unknown"),
            Self::InvalidFlags(flags) => write!(f, "native payload flags {flags} are invalid"),
            Self::UnknownRecordKind(kind) => {
                write!(f, "native payload record kind {kind} is unknown")
            }
            Self::EmptyKey => f.write_str("native payload record key is empty"),
            Self::InvalidKey => f.write_str("native payload record key is invalid"),
            Self::KeyLimit { actual, maximum } => {
                write!(
                    f,
                    "native payload key is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::ValueLimit { actual, maximum } => {
                write!(
                    f,
                    "native payload value is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::RecordCountLimit { actual, maximum } => {
                write!(
                    f,
                    "native payload has {actual} records; maximum is {maximum}"
                )
            }
            Self::InputCountLimit { actual, maximum } => {
                write!(
                    f,
                    "native request has {actual} inputs; maximum is {maximum}"
                )
            }
            Self::DuplicateRecord { kind, key } => {
                write!(f, "native payload repeats {kind:?} record {key:?}")
            }
            Self::DuplicateInput { name } => write!(f, "native request repeats input {name:?}"),
            Self::NonCanonicalOrder => f.write_str("native payload records are not canonical"),
            Self::InvalidUtf8 => f.write_str("native payload key is not UTF-8"),
            Self::TrailingBytes { actual } => {
                write!(f, "native payload has {actual} trailing bytes")
            }
            Self::LengthOverflow => f.write_str("native payload length overflows its bound"),
            Self::BindingMismatch => {
                f.write_str("native payload authority does not match its session key")
            }
            Self::ByteLimit { actual, maximum } => {
                write!(f, "native payload is {actual} bytes; maximum is {maximum}")
            }
        }
    }
}

impl std::error::Error for NativeProtocolError {}
