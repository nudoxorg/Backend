//! Deterministic response projections and payload budgets.
//!
//! JSON is the interchange format for both the CLI and MCP surfaces.  Keeping
//! the projection and budget code here means a caller can ask either surface
//! for the same detail level and receive the same fields, while the transport
//! can still apply a final byte ceiling without manufacturing a result.

use serde::Serialize;
#[cfg(test)]
use serde_json::Value;

use crate::Answer;
use crate::dto::{
    CapabilitiesDto, CoverageDto, FaultDto, IdentityDto, OutlineDto, OutlineNodeDto, PageDto,
    ProductDto, ProductIndexSearchPageDto, ProductRecordDto, ShelfDto, ShelfEntryDto, StatusDto,
};
use crate::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};

pub use backend_page_output::budget::{
    BudgetExceeded, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, ESTIMATED_BYTES_PER_TOKEN,
    EncodedPayload, MAX_PREVIEW_TEXT_BYTES, MAX_RESPONSE_BUDGET_BYTES, PayloadBudget,
    encode_serializable, encode_value, estimate_tokens,
};
use backend_page_output::budget::{PayloadProjection, encode_typed};

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
    project_answer(answer, detail, next_cursor, EncodeSink { budget })
}

/// Streaming canonical typed projection. Producers can count its complete
/// enclosing response without allocating an oversized JSON body.
pub struct AnswerProjection<'a> {
    /// Current complete typed answer.
    pub answer: &'a Answer,
    /// Original detail policy.
    pub detail: Detail,
    /// Actual or proved equal-width continuation text.
    pub next_cursor: Option<&'a str>,
}

impl Serialize for AnswerProjection<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        project_answer(
            self.answer,
            self.detail,
            self.next_cursor,
            SerializeSink(serializer),
        )
    }
}

trait ProjectionSink {
    type Output;
    fn records(
        self,
        list: &crate::RecordList,
        detail: Detail,
        cursor: Option<&str>,
    ) -> Self::Output;
    fn emit<T: Serialize>(
        self,
        answer: &'static str,
        detail: Detail,
        cursor: Option<&str>,
        body: T,
    ) -> Self::Output;
}
struct EncodeSink {
    budget: usize,
}
impl ProjectionSink for EncodeSink {
    type Output = Result<EncodedPayload, BudgetExceeded>;
    fn records(
        self,
        list: &crate::RecordList,
        detail: Detail,
        cursor: Option<&str>,
    ) -> Self::Output {
        backend_page_output::encode_records(list, detail, cursor, self.budget)
    }
    fn emit<T: Serialize>(
        self,
        answer: &'static str,
        detail: Detail,
        cursor: Option<&str>,
        body: T,
    ) -> Self::Output {
        encode_typed(answer, detail, cursor, body, self.budget)
    }
}
struct SerializeSink<S>(S);
impl<S: serde::Serializer> ProjectionSink for SerializeSink<S> {
    type Output = Result<S::Ok, S::Error>;
    fn records(
        self,
        list: &crate::RecordList,
        detail: Detail,
        cursor: Option<&str>,
    ) -> Self::Output {
        backend_page_output::RecordsProjection {
            list,
            detail,
            next_cursor: cursor,
        }
        .serialize(self.0)
    }
    fn emit<T: Serialize>(
        self,
        answer: &'static str,
        detail: Detail,
        cursor: Option<&str>,
        body: T,
    ) -> Self::Output {
        PayloadProjection {
            body,
            answer,
            detail,
            next_cursor: cursor,
        }
        .serialize(self.0)
    }
}

fn project_answer<S: ProjectionSink>(
    answer: &Answer,
    detail: Detail,
    next_cursor: Option<&str>,
    sink: S,
) -> S::Output {
    if matches!(detail, Detail::Summary) {
        return match answer {
            Answer::Page(page) => sink.emit(
                answer.kind(),
                detail,
                next_cursor,
                SummaryPageDto {
                    identity: IdentityDto::new(page.identity()),
                    kind: page.kind().map(|kind| kind.name().to_owned()),
                    language: page.language().name().to_owned(),
                },
            ),
            Answer::Records(list) => sink.records(list, detail, next_cursor),
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
                sink.emit(
                    answer.kind(),
                    detail,
                    next_cursor,
                    SummaryShelfDto {
                        revision: shelf.revision().to_string(),
                        projects,
                    },
                )
            }
            Answer::Outline(outline) => {
                let outline = OutlineDto::new(outline);
                sink.emit(
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
                )
            }
            Answer::Status(status) => {
                let status = StatusDto::new(status);
                sink.emit(
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
                )
            }
            Answer::Product(product) => {
                let product = ProductDto::new(product);
                sink.emit(
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
                        selected_source_frontier: product.selected_source_frontier,
                        package_source_membership_page: product.package_source_membership_page,
                        semantic_data: product.semantic_data,
                        package_discovery: product.package_discovery,
                    },
                )
            }
        };
    }
    match answer {
        Answer::Page(page) => sink.emit(answer.kind(), detail, next_cursor, PageDto::new(page)),
        Answer::Records(list) => sink.records(list, detail, next_cursor),
        Answer::Shelf(shelf) => sink.emit(answer.kind(), detail, next_cursor, ShelfDto::new(shelf)),
        Answer::Outline(outline) => {
            sink.emit(answer.kind(), detail, next_cursor, OutlineDto::new(outline))
        }
        Answer::Status(status) => {
            sink.emit(answer.kind(), detail, next_cursor, StatusDto::new(status))
        }
        Answer::Product(product) => {
            sink.emit(answer.kind(), detail, next_cursor, ProductDto::new(product))
        }
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
    selected_source_frontier: Option<backend_library::SelectedProjectSourceFrontier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_source_membership_page: Option<backend_library::PackageSourceMembershipPageResultV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_data: Option<crate::product::ProductSemanticData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package_discovery: Option<crate::product::PackageDiscoveryProjection>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
