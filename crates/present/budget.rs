//! Deterministic response projections and payload budgets.
//!
//! JSON is the interchange format for both the CLI and MCP surfaces.  Keeping
//! the projection and budget code here means a caller can ask either surface
//! for the same detail level and receive the same fields, while the transport
//! can still apply a final byte ceiling without manufacturing a result.

use serde::Serialize;
#[cfg(test)]
use serde_json::Value;
use std::io::{self, Write};

use crate::Answer;
use crate::dto::{
    CapabilitiesDto, CoverageDto, FaultDto, IdentityDto, OutlineDto, OutlineNodeDto, PageDto,
    ProductDto, ProductRecordDto, RecordDto, RecordListDto, ShelfDto, ShelfEntryDto, StatusDto,
};
use crate::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};

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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Detail {
    /// Keep identity, status, coverage, and short row summaries.
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
    const MARKER: &str =
        "\n\n… output truncated; request a narrower page or detail=summary";
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
                SummaryPageDto {
                    identity: IdentityDto::new(page.identity()),
                    kind: page.kind().map(|kind| kind.name().to_owned()),
                    language: page.language().name().to_owned(),
                },
                budget,
            ),
            Answer::Records(list) => {
                let records = list
                    .records()
                    .iter()
                    .map(|record| {
                        let record = RecordDto::new(record);
                        SummaryRecordDto {
                            identity: record.identity,
                            kind: record.kind,
                            language: record.language,
                            state: record.state,
                            summary: record.summary,
                            score: record.score,
                        }
                    })
                    .collect();
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryRecordsDto {
                        query: list.query(),
                        coverage: CoverageDto::all(list.coverage()),
                        readiness: list.coverage().readiness(),
                        records,
                        more: list.has_more(),
                    },
                    budget,
                )
            }
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
                let product = ProductDto::new(product);
                encode_typed(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryProductDto {
                        heading: product.heading,
                        records: product.records,
                        note: product.note,
                        fault: product.fault,
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
            PageDto::new(page),
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
            ProductDto::new(product),
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
        detail: detail.name(),
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
    detail: &'static str,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    next_cursor: Option<&'a str>,
    budget: BudgetMetadata,
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
struct SummaryPageDto {
    identity: IdentityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    language: String,
}

#[derive(Serialize)]
struct SummaryRecordDto {
    identity: IdentityDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<u32>,
}

#[derive(Serialize)]
struct SummaryRecordsDto<'a> {
    query: &'a str,
    coverage: Vec<CoverageDto>,
    readiness: &'a str,
    records: Vec<SummaryRecordDto>,
    more: bool,
}

#[derive(Serialize)]
struct SummaryShelfEntryDto {
    identity: IdentityDto,
    readiness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    declarations: u64,
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
}

#[derive(Serialize)]
struct SummaryStatusDto {
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
        assert_eq!(worst.bytes.len(), 100_025);
        assert_eq!(worst.budget.estimated_tokens, 25_007);
        assert_eq!(
            worst.bytes.as_ref(),
            include_bytes!("fixtures/worst-200-full-records.json")
        );
        let refused = encode_answer(&worst_answer, Detail::Full, None, 48 * 1024)
            .expect_err("an oversized complete page is refused before allocation");
        assert_eq!(refused.budget, 48 * 1024);
        assert_eq!(refused.bytes, 100_025);
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
            assert_eq!(result.expect("encoded result").bytes.len(), 100_025);
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
