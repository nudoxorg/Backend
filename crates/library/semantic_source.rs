//! Immutable captured semantic-source identity and transport-independent ranges.
//!
//! A source reference names a complete record, never an abbreviated generation
//! or a mutable selected head. The owner binds the reference to its native
//! roots; reconstruction preserves the exact canonical record bytes.

use crate::{MAX_PRODUCT_TEXT_BYTES, SemanticVersionRecord};
use backend_version::{ObjectVersion, Schema};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

mod reference;
mod schema;
mod versions;
pub use reference::{
    SemanticHistoryProofReference, SemanticSourceHistory, SemanticSourceMetadata,
    SemanticSourceReference, VerifiedSemanticSourceReference,
};
pub use versions::{
    SemanticVersionDescriptor, SemanticVersionsCursor, SemanticVersionsCursorClaims,
    SemanticVersionsPage, VerifiedSemanticVersionsCursor,
};

/// Canonical UTF-8 JSON encoding of the admitted source DTO, versioned
/// independently from presentation and transport metadata.
pub struct SemanticSourceRecordSchema;
impl Schema for SemanticSourceRecordSchema {
    const DOMAIN: u8 = 0x10;
    const TYPE: u16 = 15;
    const VERSION: u8 = 1;
    type Value = str;
    fn encode(value: &str, out: &mut Vec<u8>) {
        out.extend_from_slice(value.as_bytes());
    }
}

/// Exact identity and extent of one complete captured source operand.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceIdentity {
    /// Explicit source encoding, independent of a CLI or MCP envelope.
    pub schema: u16,
    /// Domain-separated version of the complete canonical source bytes.
    pub version: [u8; 32],
    /// Complete canonical UTF-8 byte extent; this is never a partial count.
    pub total_bytes: u32,
}

/// A bounded native range request within an already captured exact source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceRange {
    /// Byte offset from the preceding native range, or zero to restart.
    pub offset: u32,
    /// Caller bound on original UTF-8 bytes, within the existing product text
    /// bound. The escaped output still passes the complete transport gate.
    pub max_bytes: u16,
}

/// Native source read selected by an owner-authenticated exact reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceRequest {
    /// Exact portable source guard returned by version discovery.
    pub reference: SemanticSourceReference,
    /// Byte range to read or resume from that same complete source.
    #[serde(default)]
    pub range: SemanticSourceRange,
}

impl SemanticSourceRequest {
    /// Builds a standard resources/read URI carrying the complete bounded
    /// portable guard and exact range. The digest path alone grants no access.
    pub fn resource_uri(&self) -> Result<String, SemanticSourceError> {
        let mut output = SourceBuffer {
            bytes: Vec::new(),
            limit: crate::MAX_INDEX_SEARCH_CURSOR_BYTES,
        };
        serde_json::to_writer(&mut output, self).map_err(|_| SemanticSourceError::Range)?;
        Ok(format!(
            "backend://semantic-source/{}?request={}",
            crate::encode_id(&self.reference.source.version),
            URL_SAFE_NO_PAD.encode(output.bytes)
        ))
    }
    /// Reopens only a canonical complete request URI. Owner authentication
    /// remains mandatory at native lookup, exactly as for the named tool.
    pub fn from_resource_uri(uri: &str) -> Result<Self, SemanticSourceError> {
        let (version, encoded) = uri
            .strip_prefix("backend://semantic-source/")
            .and_then(|value| value.split_once("?request="))
            .ok_or(SemanticSourceError::Identity)?;
        if encoded.len() > crate::MAX_INDEX_SEARCH_CURSOR_BYTES.div_ceil(3) * 4 {
            return Err(SemanticSourceError::Range);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| SemanticSourceError::Record)?;
        let request: Self =
            serde_json::from_slice(&bytes).map_err(|_| SemanticSourceError::Record)?;
        if version != crate::encode_id(&request.reference.source.version)
            || request.resource_uri()? != uri
        {
            return Err(SemanticSourceError::Identity);
        }
        Ok(request)
    }
}

impl Default for SemanticSourceRange {
    fn default() -> Self {
        Self {
            offset: 0,
            max_bytes: MAX_PRODUCT_TEXT_BYTES as u16,
        }
    }
}

/// One exact native byte range. Canonical unpadded base64url preserves arbitrary
/// byte offsets, including an offset inside a multibyte UTF-8 code point.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSourceChunk {
    /// Same content identity and complete extent on every range.
    pub source: SemanticSourceIdentity,
    /// Exact offset of the first original UTF-8 byte in this chunk.
    pub offset: u32,
    /// Original canonical bytes in compact canonical unpadded base64url.
    pub base64url: String,
    /// Exact offset to request next; absent only at the complete extent.
    pub next: Option<u32>,
    /// Further exact source bytes remain after this range.
    pub more: bool,
    /// This range ends at the complete captured extent. It does not replace
    /// digest admission of the whole ordered byte union.
    pub complete: bool,
}
/// One decoded native chunk admitted under the complete wire grammar. The
/// private constructor prevents unchecked bytes from entering a range union.
/// Admission owns the one decoded allocation and copies its compact identity;
/// the larger wire DTO need not remain resident through client reconstruction.
#[derive(Debug)]
pub struct AdmittedSemanticSourceChunk {
    source: SemanticSourceIdentity,
    offset: u32,
    complete: bool,
    bytes: Vec<u8>,
}
impl AdmittedSemanticSourceChunk {
    /// True only when the checked range ends at the exact complete extent.
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Correlates this already admitted chunk with the exact requested range.
    pub fn correlate(
        &self,
        source: &SemanticSourceIdentity,
        range: &SemanticSourceRange,
    ) -> Result<(), SemanticSourceError> {
        if &self.source != source || self.offset != range.offset {
            return Err(SemanticSourceError::Identity);
        }
        if range.max_bytes == 0 || usize::from(range.max_bytes) > MAX_PRODUCT_TEXT_BYTES {
            return Err(SemanticSourceError::Range);
        }
        let remaining = self.source.total_bytes - self.offset;
        if u32::try_from(self.bytes.len()).ok() != Some(remaining.min(u32::from(range.max_bytes))) {
            return Err(SemanticSourceError::Range);
        }
        Ok(())
    }
}
impl SemanticSourceChunk {
    /// Decodes once and checks the complete native range grammar. Base64url's
    /// canonical decoder rejects padding and nonzero trailing bits, so no
    /// second encoding allocation is required to admit the payload.
    pub fn admit_decoded(&self) -> Result<AdmittedSemanticSourceChunk, SemanticSourceError> {
        if self.source.schema != 1
            || self.source.total_bytes == 0
            || self.source.total_bytes > crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES
            || self.base64url.len() > MAX_PRODUCT_TEXT_BYTES.div_ceil(3) * 4
        {
            return Err(SemanticSourceError::Identity);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.base64url)
            .map_err(|_| SemanticSourceError::Range)?;
        let length = u32::try_from(bytes.len()).map_err(|_| SemanticSourceError::Range)?;
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SemanticSourceError::Range)?;
        if length == 0
            || bytes.len() > MAX_PRODUCT_TEXT_BYTES
            || end > self.source.total_bytes
            || self.next != (end < self.source.total_bytes).then_some(end)
            || self.more != self.next.is_some()
            || self.complete != self.next.is_none()
        {
            return Err(SemanticSourceError::Range);
        }
        Ok(AdmittedSemanticSourceChunk {
            source: self.source.clone(),
            offset: self.offset,
            complete: self.complete,
            bytes,
        })
    }
    /// Checks the native wire DTO without retaining its decoded payload.
    pub fn admit(&self) -> Result<(), SemanticSourceError> {
        self.admit_decoded().map(|_| ())
    }
    /// Checks request correlation independently of the complete-source union.
    pub fn admit_request(
        &self,
        request: &SemanticSourceRequest,
    ) -> Result<(), SemanticSourceError> {
        self.admit_range(&request.reference.source, &request.range)
    }
    /// Checks an exact request's immutable identity and range with one decode.
    pub fn admit_range(
        &self,
        source: &SemanticSourceIdentity,
        range: &SemanticSourceRange,
    ) -> Result<(), SemanticSourceError> {
        self.admit_decoded()?.correlate(source, range)
    }
}

/// Failure at a source identity, range, or reconstruction boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSourceError {
    /// Source DTO or its canonical encoding is invalid.
    Record,
    /// Complete identity, schema, or total extent differs from the captured one.
    Identity,
    /// Range is empty, overflowing, out of sequence, or outside the object.
    Range,
    /// The complete range union does not match the captured content version.
    Digest,
    /// The original complete source has not yet been reconstructed.
    Incomplete,
}
impl std::fmt::Display for SemanticSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "semantic source admission failed: {self:?}")
    }
}
impl std::error::Error for SemanticSourceError {}

/// Encodes the admitted complete record once in the source schema's fixed
/// field order. Reopening a source verifies this same encoding, not a display
/// projection and not the insertion order of untrusted JSON maps.
pub fn canonical_semantic_source(
    record: &SemanticVersionRecord,
) -> Result<String, SemanticSourceError> {
    record.admit().map_err(|_| SemanticSourceError::Record)?;
    canonical_source_with_limit(record, crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES as usize)
}

// The library cannot depend on present::HardCapWriter; this fallible buffer
// applies the same streaming cap before extending the canonical allocation.
struct SourceBuffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for SourceBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|end| *end <= self.limit)
            .ok_or_else(|| std::io::Error::other("canonical source byte bound exceeded"))?;
        if end > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .checked_mul(2)
                .unwrap_or(self.limit)
                .max(end)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn canonical_source_with_limit(
    record: &SemanticVersionRecord,
    limit: usize,
) -> Result<String, SemanticSourceError> {
    let mut output = SourceBuffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, record).map_err(|_| SemanticSourceError::Range)?;
    String::from_utf8(output.bytes).map_err(|_| SemanticSourceError::Record)
}

impl SemanticSourceIdentity {
    /// Creates an exact identity from the complete admitted canonical record.
    pub fn from_record(record: &SemanticVersionRecord) -> Result<Self, SemanticSourceError> {
        let bytes = canonical_semantic_source(record)?;
        Self::from_canonical(&bytes)
    }
    /// Checks the complete extent and encodes one immutable source identity.
    pub fn from_canonical(bytes: &str) -> Result<Self, SemanticSourceError> {
        let total_bytes = u32::try_from(bytes.len()).map_err(|_| SemanticSourceError::Range)?;
        if total_bytes == 0 || total_bytes > crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES {
            return Err(SemanticSourceError::Range);
        }
        Ok(Self {
            schema: 1,
            version: ObjectVersion::<SemanticSourceRecordSchema>::from_value(bytes).to_bytes(),
            total_bytes,
        })
    }
    /// Checks canonical content against the already captured exact identity.
    pub fn admit_canonical(
        &self,
        bytes: &str,
    ) -> Result<SemanticVersionRecord, SemanticSourceError> {
        if Self::from_canonical(bytes)? != *self {
            return Err(SemanticSourceError::Digest);
        }
        let record: SemanticVersionRecord =
            serde_json::from_str(bytes).map_err(|_| SemanticSourceError::Record)?;
        if canonical_semantic_source(&record)? != bytes {
            return Err(SemanticSourceError::Record);
        }
        Ok(record)
    }
}

/// One checked immutable source read plan. Native range emission borrows its
/// admitted bytes and never reconstructs or re-hashes the full proof per page.
#[derive(Debug)]
pub struct SemanticSourceDocument {
    source: SemanticSourceIdentity,
    bytes: String,
}
impl SemanticSourceDocument {
    /// Captures one complete admitted source exactly once.
    pub fn new(record: &SemanticVersionRecord) -> Result<Self, SemanticSourceError> {
        let bytes = canonical_semantic_source(record)?;
        let source = SemanticSourceIdentity::from_canonical(&bytes)?;
        Ok(Self { source, bytes })
    }
    /// Reopens exact bytes under an independently captured source identity.
    pub fn reopen(
        source: SemanticSourceIdentity,
        bytes: String,
    ) -> Result<Self, SemanticSourceError> {
        source.admit_canonical(&bytes)?;
        Ok(Self { source, bytes })
    }
    /// Returns the complete canonical content identity and extent.
    pub const fn source(&self) -> &SemanticSourceIdentity {
        &self.source
    }
    /// Borrows the checked complete canonical UTF-8 bytes.
    pub fn bytes(&self) -> &str {
        &self.bytes
    }
    /// Selects an exact byte range from the native UTF-8 object. Every byte
    /// offset is supported; transport encoding never trims a code point.
    pub fn chunk(
        &self,
        range: &SemanticSourceRange,
    ) -> Result<SemanticSourceChunk, SemanticSourceError> {
        let bytes = self.bytes.as_str();
        let offset = usize::try_from(range.offset).map_err(|_| SemanticSourceError::Range)?;
        let bound = usize::from(range.max_bytes);
        if bound == 0 || bound > MAX_PRODUCT_TEXT_BYTES || offset >= bytes.len() {
            return Err(SemanticSourceError::Range);
        }
        let end = offset
            .checked_add(bound)
            .ok_or(SemanticSourceError::Range)?
            .min(bytes.len());
        if end == offset {
            return Err(SemanticSourceError::Range);
        }
        let next = (end < bytes.len())
            .then(|| u32::try_from(end))
            .transpose()
            .map_err(|_| SemanticSourceError::Range)?;
        Ok(SemanticSourceChunk {
            source: self.source.clone(),
            offset: range.offset,
            base64url: URL_SAFE_NO_PAD.encode(&bytes.as_bytes()[offset..end]),
            next,
            more: next.is_some(),
            complete: next.is_none(),
        })
    }
}

/// An affine range union for one exact source. Failed admission never appends
/// bytes, and a final source is exposed only after canonical digest admission.
#[derive(Debug)]
pub struct SemanticSourceAssembly {
    source: SemanticSourceIdentity,
    bytes: Vec<u8>,
}
impl SemanticSourceAssembly {
    /// Begins an empty union for the captured identity.
    pub fn new(source: SemanticSourceIdentity) -> Result<Self, SemanticSourceError> {
        if source.schema != 1
            || source.total_bytes == 0
            || source.total_bytes > crate::MAX_SEMANTIC_SHAPE_IMAGE_BYTES
        {
            return Err(SemanticSourceError::Identity);
        }
        Ok(Self {
            source,
            bytes: Vec::new(),
        })
    }
    /// Returns the exact next byte offset, usable after transport reconnection.
    pub fn offset(&self) -> Result<u32, SemanticSourceError> {
        u32::try_from(self.bytes.len()).map_err(|_| SemanticSourceError::Range)
    }
    /// Admits one next native range. Gaps, duplicates, foreign identities,
    /// changed totals, reordered chunks, and false terminal markers refuse.
    pub fn append(&mut self, chunk: &SemanticSourceChunk) -> Result<(), SemanticSourceError> {
        self.append_admitted(chunk.admit_decoded()?)
    }
    /// Consumes an admitted payload without decoding it again. Identity and
    /// union order are checked before reserve or append, so refusal leaves
    /// the already accepted bytes unchanged.
    pub fn append_admitted(
        &mut self,
        admitted: AdmittedSemanticSourceChunk,
    ) -> Result<(), SemanticSourceError> {
        if admitted.source != self.source {
            return Err(SemanticSourceError::Identity);
        }
        if admitted.offset != self.offset()? {
            return Err(SemanticSourceError::Range);
        }
        self.bytes
            .try_reserve(admitted.bytes.len())
            .map_err(|_| SemanticSourceError::Range)?;
        self.bytes.extend_from_slice(&admitted.bytes);
        Ok(())
    }
    /// Returns the complete native record only after identity and canonical
    /// encoding checks. A partial byte union cannot be used as a shape source.
    pub fn finish(self) -> Result<SemanticVersionRecord, SemanticSourceError> {
        if self.bytes.len()
            != usize::try_from(self.source.total_bytes).map_err(|_| SemanticSourceError::Range)?
        {
            return Err(SemanticSourceError::Incomplete);
        }
        self.source.admit_canonical(
            std::str::from_utf8(&self.bytes).map_err(|_| SemanticSourceError::Record)?,
        )
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn captured() -> SemanticVersionRecord {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../present/fixtures/semantic-versions-semver-public.json"
        ))
        .expect("complete real capture");
        serde_json::from_value(packet["surface"]["data"][0].clone()).expect("complete source DTO")
    }

    #[test]
    fn complete_real_42_image_source_is_exact_across_ranges_and_cold_reopen() {
        let record = captured();
        let document = SemanticSourceDocument::new(&record).expect("admitted captured source");
        let identity = document.source().clone();
        let original = document.bytes().to_owned();
        let reopened = SemanticSourceDocument::reopen(identity.clone(), original.clone())
            .expect("same content identity after reopen");
        let mut assembled = SemanticSourceAssembly::new(identity).expect("source union");
        loop {
            let chunk = reopened
                .chunk(&SemanticSourceRange {
                    offset: assembled.offset().expect("exact offset"),
                    max_bytes: 1993,
                })
                .expect("native whole byte range");
            let terminal = chunk.next.is_none();
            assembled.append(&chunk).expect("strict ordered union");
            if terminal {
                break;
            }
        }
        let restored = assembled.finish().expect("complete source admitted");
        assert_eq!(restored, record);
        assert_eq!(
            canonical_semantic_source(&restored).expect("canonical bytes"),
            original
        );
        let crate::SemanticHistoryPublicationStatus::Published { proof, .. } =
            restored.history_status
        else {
            panic!("actual published capture");
        };
        assert_eq!(proof.images.len(), 42);
    }

    #[test]
    fn decoded_capability_correlates_once_and_preserves_affine_union_on_refusal() {
        let document = SemanticSourceDocument::new(&captured()).expect("exact source");
        let range = SemanticSourceRange {
            offset: 0,
            max_bytes: 31,
        };
        let chunk = document.chunk(&range).expect("first chunk");
        let admitted = chunk.admit_decoded().expect("canonical decoder admission");
        admitted
            .correlate(document.source(), &range)
            .expect("exact requested range");
        assert_eq!(admitted.bytes.len(), 31);
        let mut assembly = SemanticSourceAssembly::new(document.source().clone()).expect("union");
        assembly
            .append_admitted(admitted)
            .expect("consume one decoded payload");
        assert_eq!(assembly.offset(), Ok(31));
        assert_eq!(
            assembly.append_admitted(chunk.admit_decoded().expect("valid wire duplicate")),
            Err(SemanticSourceError::Range)
        );
        assert_eq!(assembly.offset(), Ok(31));
        for payload in ["Zg=", "Zh", "Zg==", "Zg\n"] {
            let mut malformed = chunk.clone();
            malformed.base64url = payload.to_owned();
            assert!(
                malformed.admit_decoded().is_err(),
                "noncanonical base64url {payload:?}"
            );
        }
    }

    #[test]
    fn canonical_serialization_stops_before_extending_a_full_buffer() {
        let record = captured();
        assert_eq!(
            canonical_source_with_limit(&record, 31),
            Err(SemanticSourceError::Range)
        );
        let mut output = SourceBuffer {
            bytes: Vec::new(),
            limit: 31,
        };
        assert!(serde_json::to_writer(&mut output, &record).is_err());
        assert!(output.bytes.len() <= 31);
        assert!(output.bytes.capacity() <= 31);
    }

    #[test]
    fn range_union_refuses_gaps_duplicates_foreign_content_false_totals_and_false_terminal() {
        let document = SemanticSourceDocument::new(&captured()).expect("source");
        let first = document
            .chunk(&SemanticSourceRange {
                offset: 0,
                max_bytes: 31,
            })
            .expect("first");
        let second = document
            .chunk(&SemanticSourceRange {
                offset: 31,
                max_bytes: 31,
            })
            .expect("second");
        let mut assembly = SemanticSourceAssembly::new(document.source().clone()).expect("union");
        assert_eq!(assembly.append(&second), Err(SemanticSourceError::Range));
        assert_eq!(assembly.offset().expect("unchanged"), 0);
        for changed in 0..4 {
            let mut chunk = first.clone();
            match changed {
                0 => chunk.source.version[0] ^= 1,
                1 => chunk.source.total_bytes += 1,
                2 => chunk.next = None,
                _ => chunk.offset = u32::MAX,
            }
            assert!(assembly.append(&chunk).is_err());
            assert_eq!(assembly.offset().expect("unchanged"), 0);
        }
        assembly.append(&first).expect("first exact range");
        assert_eq!(assembly.append(&first), Err(SemanticSourceError::Range));
        assert_eq!(assembly.offset().expect("unchanged"), 31);
        assert_eq!(assembly.finish(), Err(SemanticSourceError::Incomplete));
    }

    #[test]
    fn byte_ranges_resume_inside_utf8_and_canonical_source_rejects_duplicate_json_fields() {
        let mut record = captured();
        record.package =
            crate::PackageReference::parse("/abs/é-source").expect("UTF-8 source label");
        record
            .selected_source_frontier
            .as_mut()
            .expect("actual frontier")
            .package = record.package.clone();
        let document = SemanticSourceDocument::new(&record).expect("admitted Unicode source");
        let inside = document.bytes().find('é').expect("multibyte source label") + 1;
        let chunk = document
            .chunk(&SemanticSourceRange {
                offset: inside as u32,
                max_bytes: 1,
            })
            .expect("byte offset inside code point");
        assert_eq!(
            URL_SAFE_NO_PAD.decode(chunk.base64url).expect("bytes"),
            document.bytes().as_bytes()[inside..inside + 1]
        );
        let duplicate =
            document
                .bytes()
                .replacen("\"package\":", "\"complete\":true,\"package\":", 1);
        let identity =
            SemanticSourceIdentity::from_canonical(&duplicate).expect("claim-only identity");
        assert_eq!(
            identity.admit_canonical(&duplicate),
            Err(SemanticSourceError::Record)
        );
        assert_eq!(
            document.source().admit_canonical(&duplicate),
            Err(SemanticSourceError::Digest)
        );
    }
}
