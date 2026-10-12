//! Deterministic response projections and payload budgets.
//!
//! JSON is the interchange format for both the CLI and MCP surfaces.  Keeping
//! the projection and budget code here means a caller can ask either surface
//! for the same detail level and receive the same fields, while the transport
//! can still apply a final byte ceiling without manufacturing a result.

use serde::{Serialize, Serializer, ser::SerializeSeq};
#[cfg(test)]
use serde_json::Value;
use std::io::{self, Write};

use crate::dto::{
    CapabilitiesDto, CoverageDto, FaultDto, IdentityDto, OutlineDto, OutlineNodeDto, PageDto,
    ProductDto, ProductIndexSearchPageDto, ProductRecordDto, RecordListDto, ShelfDto,
    ShelfEntryDto, StatusDto,
};
use crate::{Affordance, Answer, Cause, CauseSlug, Fault, FaultSlug, Operand, PageProjection};

/// Bytes-per-token ratio used for a stable, provider-independent estimate.
///
/// This is deliberately an estimate, not a tokenizer claim.  Measuring bytes
/// first keeps budgets reproducible across model providers and versions.
pub const ESTIMATED_BYTES_PER_TOKEN: usize = 4;

/// Default structured response budget for a context-sized answer.
pub const DEFAULT_RESPONSE_BUDGET_BYTES: usize = 48 * 1024;

/// Hard response budget for the largest complete typed payload admitted.
pub const MAX_RESPONSE_BUDGET_BYTES: usize = 256 * 1024;

/// Byte ceiling for human-readable duplicate text carried beside a typed
/// projection. The typed value remains the complete machine-readable answer;
/// this preview is deliberately kept small so the two together stay within a
/// context-sized MCP result.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 16 * 1024;

/// How much detail a caller requests from a presentation answer.
///
/// Declaration page text and JSON share this field policy. Other answer
/// variants retain their existing structured projection and text rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Detail {
    /// Keep identity, status, coverage, and short row summaries. A declaration
    /// page retains its signature and actual source/section faults.
    #[default]
    Summary,
    /// Keep the normal typed DTO, subject to the byte budget.
    Standard,
    /// Keep every available field, subject to the byte budget.
    Full,
}

impl Detail {
    /// Parses the stable wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "summary" | "minimal" => Some(Self::Summary),
            "standard" | "default" => Some(Self::Standard),
            "full" | "detail" => Some(Self::Full),
            _ => None,
        }
    }

    /// Returns the stable wire spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Standard => "standard",
            Self::Full => "full",
        }
    }
}

/// Deterministic size measurements emitted by adapters and fixtures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadBudget {
    /// UTF-8 JSON bytes, excluding framing newline bytes.
    pub bytes: usize,
    /// Provider-independent estimated tokens.
    pub estimated_tokens: usize,
}

/// A complete, bounded JSON payload produced by the typed encoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedPayload {
    /// UTF-8 JSON bytes, excluding framing newline bytes.
    pub bytes: Box<[u8]>,
    /// Measured payload budget, including the budget object itself.
    pub budget: PayloadBudget,
}

/// A typed admission failure for a response that cannot fit the requested
/// context budget without changing its DTO meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetExceeded {
    /// Encoded JSON bytes observed.
    pub bytes: usize,
    /// Maximum bytes admitted for this response.
    pub budget: usize,
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "response is {} bytes, above the {} byte budget; lower limit or request detail=summary",
            self.bytes, self.budget
        )
    }
}

/// Converts a refused complete payload into the shared typed oversize fault.
///
/// Callers receive a valid fault describing why the complete page was refused
/// instead of a fabricated partial answer.
#[must_use]
pub fn oversized_fault(error: BudgetExceeded) -> Fault {
    Fault::new(
        FaultSlug::Transport,
        Operand::Argument("response".to_owned()),
        Cause::new(CauseSlug::Oversized, error.to_string()),
        Affordance::None,
    )
}

/// Estimates tokens from bytes using the fixed repository ratio.
#[must_use]
pub const fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(ESTIMATED_BYTES_PER_TOKEN - 1) / ESTIMATED_BYTES_PER_TOKEN
}

/// Bounds a human-readable preview without splitting a UTF-8 code point.
///
/// This is shared by CLI Markdown and MCP text blocks. Keeping the truncation
/// marker stable makes their output byte-identical for the same answer and
/// gives a caller an actionable way to request a narrower page.
#[must_use]
pub fn bounded_text(text: &str) -> String {
    if text.len() <= MAX_PREVIEW_TEXT_BYTES {
        return text.to_owned();
    }
    const MARKER: &str = "\n\n… output truncated; request a narrower page or detail=summary";
    let mut end = MAX_PREVIEW_TEXT_BYTES.saturating_sub(MARKER.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], MARKER)
}

/// Serializes one answer from typed DTOs through a hard-cap writer.
///
/// The normal CLI/MCP answer path uses this function. It never lowers an
/// answer to `serde_json::Value`, and it counts before allocating the final
/// bytes. A payload that does not fit returns a typed admission error without
/// materializing an oversized response.
pub fn encode_answer(
    answer: &Answer,
    detail: Detail,
    next_cursor: Option<&str>,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    if matches!(detail, Detail::Summary) {
        return match answer {
            Answer::Page(page) => encode_typed(
                answer.kind(),
                detail,
                next_cursor,
                SummaryPageDto::new(&PageProjection::new(page, detail)),
                budget,
            ),
            Answer::Records(list) => encode_typed(
                answer.kind(),
                detail,
                next_cursor,
                SummaryRecordsDto {
                    query: list.query(),
                    coverage: CoverageDto::all(list.coverage()),
                    readiness: list.coverage().readiness(),
                    records: SummaryRecords(list.records()),
                    more: list.has_more(),
                    empty_reason: list.empty_reason(),
                    semantic_search: list.coverage().semantic_search_status(),
                },
                budget,
            ),
            Answer::Shelf(shelf) => {
                let projects = shelf
                    .entries()
                    .iter()
                    .map(|project| {
                        let project = ShelfEntryDto::new(project);
                        SummaryShelfEntryDto {
                            identity: project.identity,
                            readiness: project.readiness,
                            rows: project.rows,
                            declarations: project.declarations,
                            fault: project.fault,
                        }
                    })
                    .collect();
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryShelfDto {
                        revision: shelf.revision().to_string(),
                        projects,
                    },
                    budget,
                )
            }
            Answer::Outline(outline) => {
                let outline = OutlineDto::new(outline);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryOutlineDto {
                        package: outline.package,
                        roots: outline.roots,
                        extent: outline.extent,
                        declarations: outline.declarations,
                        unnamed: outline.unnamed,
                        page: outline.page,
                    },
                    budget,
                )
            }
            Answer::Status(status) => {
                let status = StatusDto::new(status);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryStatusDto {
                        availability: status.availability,
                        publication: status.publication,
                        readiness: status.readiness,
                        revision: status.revision,
                        revision_tag: status.revision_tag,
                        rows: status.rows,
                        project: status.project,
                        coverage: status.coverage,
                        capabilities: status.capabilities,
                    },
                    budget,
                )
            }
            Answer::Product(product) => {
                let product = ProductDto::for_detail(product, detail);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryProductDto {
                        heading: product.heading,
                        records: product.records,
                        note: product.note,
                        fault: product.fault,
                        index_search_page: product.index_search_page,
                        index_job: product.index_job,
                        index_operation: product.index_operation,
                        index_poll: product.index_poll,
                        selected_source_frontier: product.selected_source_frontier,
                        package_source_membership_page: product.package_source_membership_page,
                        semantic_data: product.semantic_data,
                        package_discovery: product.package_discovery,
                    },
                    budget,
                )
            }
        };
    }
    match answer {
        Answer::Page(page) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            PageDto::from_projection(&PageProjection::new(page, detail)),
            budget,
        ),
        Answer::Records(list) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            RecordListDto::new(list),
            budget,
        ),
        Answer::Shelf(shelf) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            ShelfDto::new(shelf),
            budget,
        ),
        Answer::Outline(outline) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            OutlineDto::new(outline),
            budget,
        ),
        Answer::Status(status) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            StatusDto::new(status),
            budget,
        ),
        Answer::Product(product) => encode_typed(
            answer.kind(),
            detail,
            next_cursor,
            ProductDto::for_detail(product, detail),
            budget,
        ),
    }
}

fn encode_typed<'a, T: Serialize>(
    answer: &'static str,
    detail: Detail,
    next_cursor: Option<&'a str>,
    body: T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    // Keep every adapter behind the same protocol ceiling, even if a caller
    // accidentally supplies an unbounded or oversized local budget.
    let budget = budget.min(MAX_RESPONSE_BUDGET_BYTES);
    let mut envelope = BudgetEnvelope {
        body,
        answer,
        projection: ProjectionDetail::new(answer, detail),
        next_cursor,
        budget: BudgetMetadata::default(),
    };
    let mut measured = count_json(&envelope);
    for _ in 0..4 {
        envelope.budget = BudgetMetadata::from_bytes(measured);
        let next = count_json(&envelope);
        if next == measured {
            break;
        }
        measured = next;
    }
    envelope.budget = BudgetMetadata::from_bytes(measured);
    measured = count_json(&envelope);
    if measured > budget {
        return Err(BudgetExceeded {
            bytes: measured,
            budget,
        });
    }
    let mut writer = HardCapWriter::new(budget);
    serde_json::to_writer(&mut writer, &envelope).map_err(|_| BudgetExceeded {
        bytes: measured,
        budget,
    })?;
    if writer.bytes.len() != measured {
        return Err(BudgetExceeded {
            bytes: writer.bytes.len(),
            budget,
        });
    }
    Ok(EncodedPayload {
        bytes: writer.bytes.into_boxed_slice(),
        budget: PayloadBudget {
            bytes: measured,
            estimated_tokens: estimate_tokens(measured),
        },
    })
}

/// Encodes another typed, map-shaped presentation body with the same bounded
/// envelope used by CLI/MCP answers.
pub fn encode_serializable<'a, T: Serialize>(
    answer: &'static str,
    detail: Detail,
    next_cursor: Option<&'a str>,
    body: T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    encode_typed(answer, detail, next_cursor, body, budget)
}

/// Serializes a typed JSON value under the same hard cap as answer envelopes.
///
/// Faults, protocol metadata, and other route-level values do not have an
/// [`Answer`] variant, but they still cross the same context boundary. This
/// helper lets those callers measure before allocating and refuse atomically.
pub fn encode_value<T: Serialize>(
    value: &T,
    budget: usize,
) -> Result<EncodedPayload, BudgetExceeded> {
    let budget = budget.min(MAX_RESPONSE_BUDGET_BYTES);
    let measured = count_json(value);
    if measured > budget {
        return Err(BudgetExceeded {
            bytes: measured,
            budget,
        });
    }
    let mut writer = HardCapWriter::new(budget);
    serde_json::to_writer(&mut writer, value).map_err(|_| BudgetExceeded {
        bytes: measured,
        budget,
    })?;
    if writer.bytes.len() != measured {
        return Err(BudgetExceeded {
            bytes: writer.bytes.len(),
            budget,
        });
    }
    Ok(EncodedPayload {
        bytes: writer.bytes.into_boxed_slice(),
        budget: PayloadBudget {
            bytes: measured,
            estimated_tokens: estimate_tokens(measured),
        },
    })
}

#[derive(Serialize)]
struct BudgetEnvelope<'a, T> {
    #[serde(flatten)]
    body: T,
    answer: &'static str,
    #[serde(flatten)]
    projection: ProjectionDetail,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    next_cursor: Option<&'a str>,
    budget: BudgetMetadata,
}

// FaultDto owns `detail` as its original diagnostic. Projection metadata must
// therefore use a separate key for fault envelopes, while answer envelopes
// retain their established `detail` contract.
#[derive(Serialize)]
#[serde(untagged)]
enum ProjectionDetail {
    Answer {
        detail: &'static str,
    },
    Fault {
        #[serde(rename = "responseDetail")]
        response_detail: &'static str,
    },
}

impl ProjectionDetail {
    fn new(answer: &str, detail: Detail) -> Self {
        match answer {
            "fault" => Self::Fault {
                response_detail: detail.name(),
            },
            _ => Self::Answer {
                detail: detail.name(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct BudgetMetadata {
    bytes: usize,
    #[serde(rename = "estimatedTokens")]
    estimated_tokens: usize,
    #[serde(rename = "bytesPerToken")]
    bytes_per_token: usize,
}

impl BudgetMetadata {
    const fn from_bytes(bytes: usize) -> Self {
        Self {
            bytes,
            estimated_tokens: estimate_tokens(bytes),
            bytes_per_token: ESTIMATED_BYTES_PER_TOKEN,
        }
    }
}

fn count_json<T: Serialize>(value: &T) -> usize {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)
        .map(|()| writer.bytes)
        .unwrap_or(usize::MAX)
}

struct HardCapWriter {
    bytes: Vec<u8>,
    cap: usize,
}

impl HardCapWriter {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(cap.min(64 * 1024)),
            cap,
        }
    }
}

impl Write for HardCapWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "serialized response exceeds the hard cap",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Serialize)]
struct SummaryPageDto<'a> {
    identity: SummaryIdentityDto<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    language: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_fault: Option<FaultDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<FaultDto>,
}

impl<'a> SummaryPageDto<'a> {
    fn new(page: &PageProjection<'a>) -> Self {
        Self {
            identity: SummaryIdentityDto::new(page.identity()),
            kind: page.kind().map(|kind| kind.name()),
            language: page.language().name(),
            signature: page.signature().map(crate::signature::Signature::text),
            source_fault: page.source_fault().map(FaultDto::new),
            notes: page.notes().iter().map(FaultDto::new).collect(),
        }
    }
}

#[derive(Serialize)]
struct SummaryRecordDto<'a> {
    identity: SummaryIdentityDto<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<u32>,
}

/// Summary retains both exact operand families and captured site facts. The
/// required trail/segments stay decodable as the public IdentityDto. Parsed
/// project and short key are optional display facets available in standard/full.
#[derive(Serialize)]
struct SummaryIdentityDto<'a> {
    coordinate: &'a str,
    trail: String,
    name: &'a str,
    shape: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
    segments: SummarySegments<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_data: Option<crate::dto::IdentitySemanticData>,
}

impl<'a> SummaryIdentityDto<'a> {
    fn new(identity: &'a crate::Identity) -> Self {
        Self {
            coordinate: identity.coordinate().as_str(),
            trail: identity.trail_within(None),
            name: identity.name(),
            shape: crate::dto::shape_name(identity),
            path: identity.path().map(crate::identity::PackagePath::as_str),
            line: identity.line().map(crate::identity::LineNumber::get),
            segments: SummarySegments(identity.trail().segments()),
            semantic_data: crate::dto::identity_semantic_data(identity),
        }
    }
}

struct SummarySegments<'a>(&'a [crate::identity::SymbolSegment]);

impl Serialize for SummarySegments<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut segments = serializer.serialize_seq(Some(self.0.len()))?;
        for segment in self.0 {
            segments.serialize_element(segment.as_str())?;
        }
        segments.end()
    }
}

struct SummaryRecords<'a>(&'a [crate::Record]);

impl Serialize for SummaryRecords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut rows = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            rows.serialize_element(&SummaryRecordDto {
                identity: SummaryIdentityDto::new(record.identity()),
                kind: record.kind().map(|kind| kind.name()),
                language: Some(record.language().name()),
                state: Some(record.state().name()),
                summary: record.summary(),
                score: record.score().map(crate::Score::get),
            })?;
        }
        rows.end()
    }
}

#[derive(Serialize)]
struct SummaryRecordsDto<'a> {
    query: &'a str,
    coverage: Vec<CoverageDto>,
    readiness: &'a str,
    records: SummaryRecords<'a>,
    more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    empty_reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_search: Option<backend_library::SemanticSearchStatus>,
}

#[derive(Serialize)]
struct SummaryShelfEntryDto {
    identity: IdentityDto,
    readiness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    declarations: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<FaultDto>,
}

#[derive(Serialize)]
struct SummaryShelfDto {
    revision: String,
    projects: Vec<SummaryShelfEntryDto>,
}

#[derive(Serialize)]
struct SummaryOutlineDto {
    package: IdentityDto,
    roots: Vec<OutlineNodeDto>,
    extent: String,
    declarations: usize,
    unnamed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<crate::dto::OutlinePageDto>,
}

#[derive(Serialize)]
struct SummaryStatusDto {
    availability: String,
    publication: String,
    readiness: String,
    revision: String,
    revision_tag: String,
    rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    coverage: Vec<CoverageDto>,
    capabilities: CapabilitiesDto,
}

#[derive(Serialize)]
struct SummaryProductDto {
    heading: String,
    records: Vec<ProductRecordDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<FaultDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_search_page: Option<ProductIndexSearchPageDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_job: Option<crate::product::IndexJobProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_operation: Option<backend_library::IndexOperationObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_poll: Option<crate::dto::IndexPollDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_source_frontier: Option<backend_library::SelectedProjectSourceFrontier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_source_membership_page: Option<backend_library::PackageSourceMembershipPageResultV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_data: Option<crate::product::ProductSemanticData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_discovery: Option<crate::product::PackageDiscoveryProjection>,
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_records_preserve_explanations_and_exact_semantic_lane_outcomes() {
        use backend_library::{Coverage, SemanticSearchReason, SemanticSearchStatus};
        let statuses = [
            None,
            Some(SemanticSearchStatus::Available),
            Some(SemanticSearchStatus::Unavailable {
                reason: SemanticSearchReason::Unconfigured,
            }),
            Some(SemanticSearchStatus::Stale {
                reason: SemanticSearchReason::StaleProjection,
            }),
        ];
        for status in statuses {
            for explanation in [None, Some("this declaration has no outgoing references")] {
                let mut list = crate::record::RecordList::new(
                    "draw",
                    crate::coverage::CoverageLine::new(&[Coverage::Complete], Some(0)),
                    Vec::<crate::record::Record>::new(),
                );
                if let Some(status) = status {
                    list = list.with_semantic_search_status(status);
                }
                if let Some(explanation) = explanation {
                    list = list.with_empty_reason(explanation);
                }
                let answer = Answer::Records(Box::new(list));
                let mut projections = Vec::new();
                for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
                    let encoded =
                        encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                            .expect("complete result with diagnostic context");
                    assert_eq!(encoded.budget.bytes, encoded.bytes.len());
                    let value: Value = serde_json::from_slice(&encoded.bytes).expect("JSON");
                    assert!(value.get("nextCursor").is_none());
                    assert_eq!(value["records"], serde_json::json!([]));
                    assert_eq!(value["more"], false);
                    assert_eq!(value.get("empty_reason").is_some(), explanation.is_some());
                    assert_eq!(value.get("semantic_search").is_some(), status.is_some());
                    if let Some(explanation) = explanation {
                        assert_eq!(value["empty_reason"], explanation);
                    }
                    if let Some(status) = status {
                        let actual: SemanticSearchStatus =
                            serde_json::from_value(value["semantic_search"].clone())
                                .expect("exact typed semantic outcome");
                        assert_eq!(actual, status);
                    }
                    projections.push(value);
                }
                for field in [
                    "query",
                    "coverage",
                    "readiness",
                    "empty_reason",
                    "semantic_search",
                ] {
                    assert_eq!(projections[0][field], projections[1][field], "{field}");
                    assert_eq!(projections[0][field], projections[2][field], "{field}");
                }
            }
        }
    }

    // Deserialize directly from serialized bytes before Value can collapse
    // duplicate object keys. This validator deliberately rejects the original
    // envelope collision as well as accepting the repaired producer.
    struct UniqueTopLevelKeys;

    impl<'de> serde::Deserialize<'de> for UniqueTopLevelKeys {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Keys;
            impl<'de> serde::de::Visitor<'de> for Keys {
                type Value = UniqueTopLevelKeys;

                fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter.write_str("a JSON object with unique top-level keys")
                }

                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Self::Value, A::Error> {
                    let mut keys = std::collections::BTreeSet::new();
                    while let Some(key) = map.next_key::<String>()? {
                        if !keys.insert(key.clone()) {
                            return Err(serde::de::Error::custom(format!("duplicate key: {key}")));
                        }
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                    Ok(UniqueTopLevelKeys)
                }
            }
            deserializer.deserialize_map(Keys)
        }
    }

    fn diagnostic_fault() -> Fault {
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path("/socket".to_owned()),
            Cause::new(
                CauseSlug::Unreachable,
                "private state parent /state µ was refused; repair its permissions",
            ),
            Affordance::None,
        )
    }

    #[test]
    fn serialized_fault_envelopes_keep_unique_diagnostic_and_projection_keys() {
        assert!(
            serde_json::from_slice::<UniqueTopLevelKeys>(
                br#"{"detail":"original refusal","detail":"full"}"#
            )
            .is_err(),
            "the byte-level control must reject the original duplicate-key bug"
        );
        let fault = diagnostic_fault();
        let direct = crate::fault_value(&fault);
        let serialized_dto = serde_json::to_vec(&FaultDto::new(&fault)).expect("direct fault DTO");
        serde_json::from_slice::<UniqueTopLevelKeys>(&serialized_dto)
            .expect("unique direct DTO keys");
        for (detail, expected_bytes) in [
            (Detail::Summary, 245),
            (Detail::Standard, 246),
            (Detail::Full, 242),
        ] {
            let payload = encode_serializable(
                "fault",
                detail,
                None,
                FaultDto::new(&fault),
                MAX_RESPONSE_BUDGET_BYTES,
            )
            .expect("complete fault envelope");
            serde_json::from_slice::<UniqueTopLevelKeys>(&payload.bytes)
                .expect("unique encoded envelope keys");
            let value: Value = serde_json::from_slice(&payload.bytes).expect("fault JSON");
            assert_eq!(value["detail"], fault.cause().sentence());
            assert_eq!(value["detail"], direct["detail"]);
            assert_eq!(value["responseDetail"], detail.name());
            assert_eq!(value["answer"], "fault");
            assert_eq!(value["slug"], direct["slug"]);
            assert_eq!(value["operand"], direct["operand"]);
            assert_eq!(value["cause"], direct["cause"]);
            assert_eq!(payload.bytes.len(), expected_bytes);
            assert_eq!(payload.budget.bytes, expected_bytes);
            assert_eq!(
                payload.budget.estimated_tokens,
                estimate_tokens(expected_bytes)
            );
            assert_eq!(value["budget"]["bytes"], expected_bytes);
            assert_eq!(
                value["budget"]["estimatedTokens"],
                estimate_tokens(expected_bytes)
            );
            assert_eq!(value["budget"]["bytesPerToken"], ESTIMATED_BYTES_PER_TOKEN);
            let exact =
                encode_serializable("fault", detail, None, FaultDto::new(&fault), expected_bytes)
                    .expect("the exact complete fault budget is admitted");
            assert_eq!(exact, payload);
            let below = encode_serializable(
                "fault",
                detail,
                None,
                FaultDto::new(&fault),
                expected_bytes - 1,
            )
            .expect_err("one byte below the complete fault is refused atomically");
            assert_eq!(below.bytes, expected_bytes);
            assert_eq!(below.budget, expected_bytes - 1);
        }
    }

    #[test]
    fn fault_metadata_uses_the_same_hard_ceiling_without_truncating_the_diagnostic() {
        let fault = Fault::new(
            FaultSlug::Endpoint,
            Operand::Whole,
            Cause::new(
                CauseSlug::Unreachable,
                "µ".repeat(MAX_RESPONSE_BUDGET_BYTES),
            ),
            Affordance::None,
        );
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let refused =
                encode_serializable("fault", detail, None, FaultDto::new(&fault), usize::MAX)
                    .expect_err("a complete oversized fault cannot bypass the protocol cap");
            assert_eq!(refused.budget, MAX_RESPONSE_BUDGET_BYTES);
            assert!(refused.bytes > MAX_RESPONSE_BUDGET_BYTES);
        }
    }

    #[test]
    fn ordinary_answer_envelopes_retain_the_existing_detail_contract() {
        let list = crate::record::RecordList::new(
            "unchanged projection metadata",
            crate::coverage::CoverageLine::new(&[backend_library::Coverage::Complete], Some(0)),
            Vec::<crate::record::Record>::new(),
        );
        let answer = Answer::Records(Box::new(list));
        for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
            let payload = encode_answer(&answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("normal answer envelope");
            serde_json::from_slice::<UniqueTopLevelKeys>(&payload.bytes)
                .expect("unique answer keys");
            let value: Value = serde_json::from_slice(&payload.bytes).expect("normal answer JSON");
            assert_eq!(value["detail"], detail.name());
            assert!(value.get("responseDetail").is_none());
            assert_eq!(payload.budget.bytes, payload.bytes.len());
        }
    }

    #[test]
    fn usability_document_summary_retains_the_standard_signature() {
        let page = crate::page::Page::new(
            crate::Identity::parse("/abs/app::src/entry.ts:1::entrySummary"),
            Some(backend_library::DeclarationKind::Function),
            crate::page::Source::Absent {
                fault: crate::Fault::lane(
                    backend_library::Lane::Exact,
                    backend_library::Reason::NoIndex,
                ),
            },
        )
        .with_signature(crate::signature::Signature::tokenize(
            "function entrySummary(entry: Entry): string",
            crate::Language::TypeScript,
        ));
        let answer = Answer::Page(Box::new(page));
        let summary = encode_answer(
            &answer,
            Detail::Summary,
            None,
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("summary fits");
        let standard = encode_answer(
            &answer,
            Detail::Standard,
            None,
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("standard fits");
        let summary: Value = serde_json::from_slice(&summary.bytes).expect("summary JSON");
        let standard: Value = serde_json::from_slice(&standard.bytes).expect("standard JSON");
        let typed: crate::SummaryPageDto = serde_json::from_value(summary.clone())
            .expect("the actual Summary page decodes through its public projection type");
        let identity: crate::IdentityDto = serde_json::from_value(summary["identity"].clone())
            .expect("Summary retains the existing public IdentityDto grammar");
        assert_eq!(typed.identity, identity);
        assert_eq!(
            typed.signature.as_deref(),
            Some("function entrySummary(entry: Entry): string")
        );
        for field in [
            "coordinate",
            "trail",
            "name",
            "shape",
            "path",
            "line",
            "segments",
            "semantic_data",
        ] {
            assert_eq!(summary["identity"][field], standard["identity"][field]);
        }
        assert_eq!(summary["signature"], standard["signature"]);
        assert_eq!(
            summary["signature"],
            "function entrySummary(entry: Entry): string"
        );
        assert!(summary.get("source").is_none());
    }

    #[test]
    fn summary_records_keep_exact_operands_sites_facts_and_pagination_in_owner_order() {
        use backend_library::{DeclarationKind, Row, RowId, SourceLocation, symbol_key};

        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(&[]),
        );
        let labels = [
            "/abs/你好::src/café.go:7::Outer::Inner".to_owned(),
            format!("/abs/你好::semantic::{}::Inner", "ab".repeat(32)),
            format!(
                "/abs/你好::external::symbol:{}::外部::Inner",
                "cd".repeat(32)
            ),
            "opaque | coordinate with `literal` separators".to_owned(),
        ];
        let records = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let (path, line) = if index == 0 {
                    ("src/café.go", 7)
                } else {
                    ("captured/独立.rs", 91)
                };
                let mut row = Row::new(
                    RowId::Symbol(symbol_key(&format!("retained-{index}"))),
                    basis,
                    label.as_str(),
                )
                .with_kind(DeclarationKind::Class)
                .with_source(SourceLocation::new(path, line).expect("actual captured site"));
                row.score = Some(index as u32);
                crate::Record::from_row(&row).with_summary(format!("retained summary {index}"))
            })
            .collect::<Vec<_>>();
        let answer = Answer::Records(Box::new(
            crate::RecordList::new(
                "Inner",
                crate::CoverageLine::new(&[backend_library::Coverage::Complete], Some(4)),
                records,
            )
            .with_more(true),
        ));
        let cursor = "opaque signed token retained byte-for-byte";
        let summary = encode_answer(
            &answer,
            Detail::Summary,
            Some(cursor),
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("summary fits");
        let full = encode_answer(
            &answer,
            Detail::Full,
            Some(cursor),
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("full fits");
        let summary_value: Value = serde_json::from_slice(&summary.bytes).expect("summary JSON");
        let full_value: Value = serde_json::from_slice(&full.bytes).expect("full JSON");
        let typed_summary: crate::RecordListDto = serde_json::from_value(summary_value.clone())
            .expect("actual Summary rows decode through the existing public RecordListDto");
        let typed_full: crate::RecordListDto =
            serde_json::from_value(full_value.clone()).expect("actual full rows decode");
        assert_eq!(typed_summary.query, typed_full.query);
        for field in ["query", "coverage", "readiness", "more", "nextCursor"] {
            assert_eq!(summary_value[field], full_value[field], "{field}");
        }
        let rows = summary_value["records"].as_array().expect("all records");
        assert_eq!(rows.len(), labels.len());
        for (index, (record, label)) in rows.iter().zip(labels).enumerate() {
            assert_eq!(record["identity"]["coordinate"], label);
            let (path, line) = if index == 0 {
                ("src/café.go", 7)
            } else {
                ("captured/独立.rs", 91)
            };
            assert_eq!(record["identity"]["path"], path);
            assert_eq!(record["identity"]["line"], line);
            for field in [
                "coordinate",
                "trail",
                "name",
                "shape",
                "path",
                "line",
                "segments",
                "semantic_data",
            ] {
                assert_eq!(
                    record["identity"][field],
                    full_value["records"][index]["identity"][field]
                );
            }
            let selected: crate::IdentitySemanticData =
                serde_json::from_value(record["identity"]["semantic_data"].clone())
                    .expect("exact selected operand accepted by the existing closed type");
            let crate::IdentitySemanticData::SelectedSymbolId(id) = selected;
            assert_eq!(
                id.bytes(),
                symbol_key(&format!("retained-{index}")).as_bytes()
            );
            for field in ["kind", "language", "state", "summary", "score"] {
                assert_eq!(
                    record[field], full_value["records"][index][field],
                    "{field}"
                );
            }
            for field in ["project", "key"] {
                assert!(record["identity"].get(field).is_none());
                assert!(
                    full_value["records"][index]["identity"]
                        .get(field)
                        .is_some()
                        || field == "project"
                );
            }
        }
        assert!(summary.bytes.len() < full.bytes.len());
        assert_eq!(
            encode_answer(&answer, Detail::Summary, Some(cursor), summary.bytes.len())
                .expect("exact boundary fits")
                .bytes,
            summary.bytes
        );
        assert!(
            encode_answer(
                &answer,
                Detail::Summary,
                Some(cursor),
                summary.bytes.len() - 1
            )
            .is_err()
        );
    }

    #[test]
    fn all_record_details_preserve_eight_metadata_combinations_and_public_decode() {
        use backend_library::{
            Cursor, PageContinuation, SemanticSearchReason, SemanticSearchStatus,
        };

        for flags in 0..8 {
            let mut list = crate::RecordList::new(
                "外部::Inner",
                crate::CoverageLine::new(&[backend_library::Coverage::Complete], Some(0)),
                Vec::<crate::Record>::new(),
            );
            if flags & 1 != 0 {
                list = list.with_empty_reason("No graph edges were recorded at this coordinate.");
            }
            if flags & 2 != 0 {
                list = list.with_semantic_search_status(SemanticSearchStatus::Unavailable {
                    reason: SemanticSearchReason::Unconfigured,
                });
            }
            if flags & 4 != 0 {
                list = list.with_continuation(Some(PageContinuation::from_cursor(Cursor::new())));
            }
            let expected = crate::RecordListDto::new(&list);
            let cursor = list.has_more().then_some("exact opaque continuation");
            let answer = Answer::Records(Box::new(list));
            for detail in [Detail::Summary, Detail::Standard, Detail::Full] {
                let encoded = encode_answer(&answer, detail, cursor, DEFAULT_RESPONSE_BUDGET_BYTES)
                    .expect("bounded metadata matrix");
                let value: Value = serde_json::from_slice(&encoded.bytes).expect("actual envelope");
                let actual: crate::RecordListDto = serde_json::from_value(value.clone())
                    .expect("every actual detail decodes through the public result-list DTO");
                assert_eq!(actual, expected, "flags={flags} detail={detail:?}");
                assert_eq!(value["nextCursor"].as_str(), cursor);
                assert_eq!(value["budget"]["bytes"], encoded.bytes.len());
                let exact = encode_answer(&answer, detail, cursor, encoded.bytes.len())
                    .expect("exact measured boundary fits");
                assert_eq!(exact.bytes, encoded.bytes);
                assert!(encode_answer(&answer, detail, cursor, encoded.bytes.len() - 1).is_err());
            }
        }
    }

    fn membership_answer(count: usize, path_bytes: usize) -> Answer {
        let package = backend_library::PackageReference::parse("budget-fixture")
            .expect("local package reference");
        let files = (0..count)
            .map(|index| {
                let mut file_key = [0; 32];
                file_key[..8].copy_from_slice(&(index as u64).to_be_bytes());
                let prefix = format!("src/{index:03}/");
                let path = format!("{prefix}{}", "x".repeat(path_bytes - prefix.len()));
                backend_library::PackageSourceMembershipFileV1 {
                    file_key,
                    path,
                    language: backend_library::PackageSourceMembershipLanguageV1::TypeScript,
                    content_version: [0x5a; 32],
                    source_identity: Some([0xa5; 32]),
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let page = backend_library::PackageSourceMembershipPageResultV1::Page {
            package,
            project_key: [0x11; 32],
            source_relation_root: [0x22; 32],
            source_version: [0x33; 32],
            file_count: u32::try_from(count).expect("fixture count fits"),
            start_offset: 0,
            scope: backend_library::PackageSourceMembershipScopeV1::IndexedProjectMembership,
            exclusions: backend_library::PackageSourceMembershipExclusionsV1::NotCaptured,
            files,
            next: None,
        };
        let reply = backend_library::SurfaceReply::PackageSourceMembershipPage(page);
        Answer::Product(Box::new(crate::product::product_view(&reply)))
    }

    #[test]
    fn token_estimate_is_deterministic() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
    }

    #[test]
    fn bounded_preview_has_an_exact_utf8_safe_ceiling() {
        let preview = bounded_text(&"λאב".repeat(MAX_PREVIEW_TEXT_BYTES));
        assert!(preview.len() <= MAX_PREVIEW_TEXT_BYTES);
        assert!(preview.is_char_boundary(preview.len()));
        assert!(preview.ends_with("request a narrower page or detail=summary"));
        assert_eq!(bounded_text("short"), "short");
    }

    #[test]
    fn oversized_payload_is_a_typed_fault_instead_of_partial_json() {
        let fault = oversized_fault(BudgetExceeded {
            bytes: 65_537,
            budget: 48 * 1024,
        });
        assert_eq!(fault.slug().as_str(), "transport");
        assert_eq!(fault.cause().slug().as_str(), "oversized");
        let value = crate::fault_value(&fault);
        assert_eq!(value["answer"], "fault");
        assert_eq!(value["cause"], "oversized");
    }

    #[test]
    fn typed_encoder_counts_before_materializing_and_preserves_summary_shape() {
        let list = crate::record::RecordList::new(
            "ferris",
            crate::coverage::CoverageLine::new(&[], Some(0)),
            Vec::<crate::record::Record>::new(),
        );
        let answer = Answer::Records(Box::new(list));
        let payload = encode_answer(&answer, Detail::Summary, None, 48 * 1024)
            .expect("empty typed page fits");
        assert_eq!(payload.budget.bytes, payload.bytes.len());
        let value: Value = serde_json::from_slice(&payload.bytes).expect("valid typed JSON");
        assert_eq!(value["answer"], "records");
        assert_eq!(value["detail"], "summary");
        assert_eq!(value["budget"]["bytes"], payload.bytes.len());
        assert!(value.get("signature").is_none());
        assert_eq!(payload.bytes.len(), 334);
        assert_eq!(
            payload.bytes.as_ref(),
            include_bytes!("fixtures/common-empty-records-summary.json")
        );
        let complete = crate::record::RecordList::new(
            "ferris",
            crate::coverage::CoverageLine::new(&[backend_library::Coverage::Complete], Some(0)),
            Vec::<crate::record::Record>::new(),
        );
        let complete = encode_answer(
            &Answer::Records(Box::new(complete)),
            Detail::Summary,
            None,
            48 * 1024,
        )
        .expect("complete typed page fits");
        assert_eq!(complete.bytes.len(), 324);
        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(&[]),
        );
        let rows = (0..200)
            .map(|index| {
                let row = backend_library::Row::new(
                    backend_library::RowId::Symbol(backend_library::symbol_key(&format!(
                        "typed-budget-{index}"
                    ))),
                    basis,
                    format!(
                        "/workspace/project::src/module_{index}/declaration_{index}:1::declaration_{index}"
                    ),
                )
                .with_kind(backend_library::DeclarationKind::Function)
                .with_signature("pub fn declaration() -> Result<(), Error>");
                crate::record::Record::from_row(&row)
                    .with_summary("A bounded fixture declaration.")
            })
            .collect::<Vec<_>>();
        let worst = crate::record::RecordList::new(
            "ferris",
            crate::coverage::CoverageLine::new(&[backend_library::Coverage::Complete], Some(200)),
            rows,
        )
        .with_more(true);
        let worst_answer = Answer::Records(Box::new(worst.clone()));
        let worst = encode_answer(&worst_answer, Detail::Full, None, 256 * 1024)
            .expect("worst typed page fits hard cap");
        assert_eq!(worst.bytes.len(), 134_052);
        assert_eq!(worst.budget.estimated_tokens, 33_513);
        assert_eq!(
            worst.bytes.as_ref(),
            include_bytes!("fixtures/worst-200-full-records-selected-id.json")
        );
        let current: serde_json::Value =
            serde_json::from_slice(&worst.bytes).expect("actual native JSON");
        assert_eq!(
            serde_json::to_vec(&current)
                .expect("Value serializer")
                .len(),
            worst.bytes.len(),
            "stream and Value encoders measure the same complete JSON"
        );
        let historical: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/worst-200-full-records.json"))
                .expect("retained prior golden bytes");
        let mut without_selected_ids = current;
        for (index, record) in without_selected_ids["records"]
            .as_array_mut()
            .expect("records")
            .iter_mut()
            .enumerate()
        {
            let selected = record["identity"]
                .as_object_mut()
                .expect("identity")
                .remove("semantic_data")
                .expect("exact selected symbol identity");
            assert_eq!(
                selected,
                serde_json::json!({
                    "kind": "selected-symbol-id",
                    "value": backend_library::symbol_key(&format!("typed-budget-{index}")).to_bytes(),
                })
            );
        }
        without_selected_ids["budget"] = historical["budget"].clone();
        assert_eq!(
            without_selected_ids, historical,
            "only exact selected IDs and derived budget metadata changed"
        );
        let refused = encode_answer(&worst_answer, Detail::Full, None, 48 * 1024)
            .expect_err("an oversized complete page is refused before allocation");
        assert_eq!(refused.budget, 48 * 1024);
        assert_eq!(refused.bytes, 134_052);
    }

    #[test]
    fn typed_encoder_enforces_exact_boundaries_without_unicode_truncation() {
        let list = crate::record::RecordList::new(
            "café 你好",
            crate::coverage::CoverageLine::new(&[backend_library::Coverage::Complete], Some(0)),
            Vec::<crate::record::Record>::new(),
        );
        let answer = Answer::Records(Box::new(list));
        let payload = encode_answer(&answer, Detail::Summary, None, MAX_RESPONSE_BUDGET_BYTES)
            .expect("unicode payload fits the hard maximum");
        let exact = payload.bytes.len();
        assert!(exact > 1);
        assert_eq!(
            encode_answer(&answer, Detail::Summary, None, exact)
                .expect("exact boundary is admitted")
                .bytes
                .len(),
            exact
        );
        let below = encode_answer(&answer, Detail::Summary, None, exact - 1)
            .expect_err("one byte below the complete payload is refused atomically");
        assert_eq!(below.bytes, exact);
        assert_eq!(below.budget, exact - 1);
        assert_eq!(
            encode_answer(&answer, Detail::Summary, None, exact + 1)
                .expect("one byte above the complete payload is admitted")
                .bytes
                .len(),
            exact
        );
        let encoded = encode_answer(&answer, Detail::Summary, None, exact)
            .expect("payload remains valid UTF-8 JSON");
        let value: Value = serde_json::from_slice(&encoded.bytes).expect("complete JSON");
        assert_eq!(value["query"], "café 你好");
        assert!(!encoded.bytes.is_ascii(), "Unicode must remain UTF-8");
    }

    #[test]
    fn membership_default_page_fits_reply_budget_and_oversize_refuses_atomically() {
        let default_count =
            usize::from(backend_library::DEFAULT_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES);
        assert_eq!(default_count, 32);
        let default = membership_answer(default_count, 72);
        for detail in [Detail::Summary, Detail::Full] {
            let payload = encode_answer(&default, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect("default typed membership page fits the normal reply budget");
            println!(
                "default membership reply: detail={detail:?}, files={default_count}, bytes={}",
                payload.budget.bytes
            );
            assert!(payload.budget.bytes <= DEFAULT_RESPONSE_BUDGET_BYTES);
            assert_eq!(
                encode_answer(&default, detail, None, payload.budget.bytes)
                    .expect("exact membership reply boundary is admitted")
                    .budget
                    .bytes,
                payload.budget.bytes
            );
            let below = encode_answer(&default, detail, None, payload.budget.bytes - 1)
                .expect_err("one byte below must refuse the complete page");
            assert_eq!(below.bytes, payload.budget.bytes);
            assert_eq!(below.budget, payload.budget.bytes - 1);
        }

        let maximum = membership_answer(
            usize::from(backend_library::MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES),
            72,
        );
        for detail in [Detail::Summary, Detail::Full] {
            let refused = encode_answer(&maximum, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES)
                .expect_err("the explicit maximum is admitted only when its full reply fits");
            println!(
                "maximum membership reply: detail={detail:?}, files={}, bytes={}",
                backend_library::MAX_PACKAGE_SOURCE_MEMBERSHIP_PAGE_FILES,
                refused.bytes
            );
            assert!(refused.bytes > DEFAULT_RESPONSE_BUDGET_BYTES);
            assert_eq!(refused.budget, DEFAULT_RESPONSE_BUDGET_BYTES);
        }

        let long_paths = membership_answer(
            default_count,
            backend_library::MAX_PACKAGE_SOURCE_MEMBERSHIP_PATH_BYTES,
        );
        let refused = encode_answer(&long_paths, Detail::Full, None, MAX_RESPONSE_BUDGET_BYTES)
            .expect_err("bounded long paths remain a typed refusal, never truncated");
        println!(
            "maximum-path membership reply: detail=Full, files={default_count}, path_bytes={}, bytes={}",
            backend_library::MAX_PACKAGE_SOURCE_MEMBERSHIP_PATH_BYTES,
            refused.bytes
        );
        assert!(refused.bytes > MAX_RESPONSE_BUDGET_BYTES);
        assert_eq!(refused.budget, MAX_RESPONSE_BUDGET_BYTES);
    }

    #[test]
    fn hard_cap_writer_never_returns_a_truncated_success() {
        let mut writer = HardCapWriter::new(4);
        assert!(writer.write_all(b"abc").is_ok());
        assert!(writer.write_all(b"de").is_err());
        assert_eq!(writer.bytes, b"abc");
    }

    #[test]
    fn fixture_measurements_are_stable_and_budget_is_self_consistent() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/payload-budgets.json"))
            .expect("valid payload budget fixture");
        assert_eq!(fixture["schema"], "backend-present.payload-budget/v2");
        assert_eq!(fixture["tokenizer"]["implementation"], "tiktoken");
        assert_eq!(fixture["tokenizer"]["version"], "0.11.0");
        assert_eq!(fixture["tokenizer"]["encoding"], "cl100k_base");
        assert_eq!(fixture["tokenizer"]["model_policy"], "mcp-cl100k-base-v1");
        assert_eq!(fixture["tokenizer"]["caps"]["default_bytes"], 49_152);
        assert_eq!(fixture["tokenizer"]["caps"]["default_tokens"], 12_000);
        assert_eq!(fixture["tokenizer"]["caps"]["hard_bytes"], 262_144);
        assert_eq!(fixture["tokenizer"]["caps"]["hard_tokens"], 65_536);
        let common = fixture["fixtures"]["common_empty_records_summary"]
            .as_object()
            .expect("common fixture");
        let worst = fixture["fixtures"]["worst_200_full_records"]
            .as_object()
            .expect("worst fixture");
        assert_eq!(common["bytes"], 334);
        assert_eq!(common["estimated_tokens"], 84);
        assert_eq!(common["real_tokens"], 87);
        assert_eq!(worst["bytes"], 100_025);
        assert_eq!(worst["estimated_tokens"], 25_007);
        assert_eq!(worst["real_tokens"], 23_460);
        for name in [
            "unicode_rtl_records",
            "long_page_document_source",
            "continuation_envelope",
            "shelf_summary",
            "outline_summary",
            "status_summary",
            "product_summary",
            "query_page",
            "error_fault",
            "oversized_fault",
            "tool_schema",
        ] {
            assert!(
                fixture["fixtures"][name]["bytes"].as_u64().is_some(),
                "{name} bytes"
            );
            assert!(
                fixture["fixtures"][name]["real_tokens"].as_u64().is_some(),
                "{name} exact tokenizer count"
            );
            assert!(
                fixture["fixtures"][name]["allocation_bytes"]
                    .as_u64()
                    .is_some(),
                "{name} allocation observation"
            );
        }
        assert_eq!(
            fixture["fixtures"]["atomic_oversized_refusal"]["admitted"],
            false
        );
        assert_eq!(
            fixture["fixtures"]["atomic_oversized_refusal"]["partial_bytes"],
            0
        );
        assert!(
            worst["bytes"].as_u64().expect("worst bytes")
                > common["bytes"].as_u64().expect("common bytes")
        );
    }

    #[test]
    fn randomized_typed_budgets_never_admit_an_oversized_payload() {
        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(&[]),
        );
        for length in 0..=64 {
            let rows = (0..length)
                .map(|index| {
                    let row = backend_library::Row::new(
                        backend_library::RowId::Symbol(backend_library::symbol_key(&format!(
                            "random-budget-{index}"
                        ))),
                        basis,
                        format!("pkg::module_{index}::declaration_{index}"),
                    );
                    crate::record::Record::from_row(&row).with_summary("bounded randomized fixture")
                })
                .collect::<Vec<_>>();
            let answer = Answer::Records(Box::new(
                crate::record::RecordList::new(
                    "q",
                    crate::coverage::CoverageLine::new(
                        &[backend_library::Coverage::Complete],
                        Some(length as u64),
                    ),
                    rows,
                )
                .with_more(length > 0),
            ));
            let result = encode_answer(&answer, Detail::Full, None, 1024);
            if let Ok(payload) = result {
                assert!(payload.bytes.len() <= 1024, "length={length}");
                assert_eq!(payload.bytes.len(), payload.budget.bytes);
                let _: Value = serde_json::from_slice(&payload.bytes).expect("valid JSON");
            }
        }
    }

    #[test]
    fn pinned_tokenizer_fixture_is_checked_in_as_measurement_evidence() {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/payload-budgets.json"))
            .expect("valid payload budget fixture");
        assert_eq!(fixture["tokenizer"]["implementation"], "tiktoken");
        assert_eq!(fixture["tokenizer"]["version"], "0.11.0");
        assert_eq!(fixture["tokenizer"]["encoding"], "cl100k_base");
        assert_eq!(
            fixture["tokenizer"]["artifact_sha256"],
            "8613f818e5af318d868379772ce5234e3a3e8519b6c611d94cf708fdfc580fbc"
        );
        assert_eq!(
            fixture["fixtures"]["common_empty_records_summary"]["bytes"],
            334
        );
        assert_eq!(
            fixture["fixtures"]["common_empty_records_summary"]["real_tokens"],
            87
        );
        assert_eq!(
            fixture["fixtures"]["worst_200_full_records"]["bytes"],
            100_025
        );
        assert_eq!(
            fixture["fixtures"]["worst_200_full_records"]["real_tokens"],
            23_460
        );
    }

    #[test]
    fn worst_case_projection_reports_bounded_latency_and_allocations() {
        use allocation_counter::measure;
        use std::time::Instant;

        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(&[]),
        );
        let rows = (0..200)
            .map(|index| {
                let row = backend_library::Row::new(
                    backend_library::RowId::Symbol(backend_library::symbol_key(&format!(
                        "typed-budget-{index}"
                    ))),
                    basis,
                    format!(
                        "/workspace/project::src/module_{index}/declaration_{index}:1::declaration_{index}"
                    ),
                )
                .with_kind(backend_library::DeclarationKind::Function)
                .with_signature("pub fn declaration() -> Result<(), Error>");
                crate::record::Record::from_row(&row)
                    .with_summary("A bounded fixture declaration.")
            })
            .collect::<Vec<_>>();
        let answer = Answer::Records(Box::new(
            crate::record::RecordList::new(
                "ferris",
                crate::coverage::CoverageLine::new(
                    &[backend_library::Coverage::Complete],
                    Some(200),
                ),
                rows,
            )
            .with_more(true),
        ));
        let mut timings = Vec::with_capacity(64);
        let mut allocations = Vec::with_capacity(64);
        for _ in 0..64 {
            let start = Instant::now();
            let mut result = None;
            let info = measure(|| {
                result = Some(
                    encode_answer(&answer, Detail::Full, None, 256 * 1024)
                        .expect("typed worst fixture fits"),
                );
            });
            assert_eq!(result.expect("encoded result").bytes.len(), 134_052);
            timings.push(start.elapsed().as_nanos());
            allocations.push(info.bytes_total);
        }
        timings.sort_unstable();
        allocations.sort_unstable();
        let p50 = timings[timings.len() / 2];
        let p95 = timings[timings.len() * 95 / 100];
        let a50 = allocations[allocations.len() / 2];
        let a95 = allocations[allocations.len() * 95 / 100];
        println!(
            "worst-case p50_ns={p50} p95_ns={p95} p50_alloc_bytes={a50} p95_alloc_bytes={a95}"
        );
        assert!(p95 > 0);
        assert!(a95 > 0);
    }
}
