//! One projection/cost recipe shared by the packed search producer and MCP
//! egress. Counting streams the typed payload; only an admitted page becomes
//! a JSON Value. The duplicate preview has its independent 16 KiB ceiling.

use crate::{
    BudgetExceeded, Detail, JSONRPC_PREVIEW_MARKER, MAX_PREVIEW_TEXT_BYTES, RecordList,
    RecordsProjection, estimate_tokens, record_list_from_rows, serialized_json_bytes,
};
use backend_library::{
    Coverage, PackedCallContext, PagePackingDetail, PagePackingScope, Row, SemanticSearchStatus,
};
use serde::Serialize;
use serde_json::Value;

/// Exact search projection, with the original rows and truthful lane status.
#[must_use]
pub fn packed_search_records(
    query: &str,
    rows: &[Row],
    coverage: &[Coverage],
    status: SemanticSearchStatus,
    more: bool,
) -> RecordList {
    record_list_from_rows(query, rows, coverage, more).with_semantic_search_status(status)
}

/// Canonical projected row commitment, shared by range reopening and packing.
pub fn packed_record_commitment(row: &Row, detail: PagePackingDetail) -> Result<[u8; 32], String> {
    let list = record_list_from_rows("", std::slice::from_ref(row), &[], false);
    let record =
        crate::records_projection::ProjectedRecord::new(&list.records()[0], packing_detail(detail));
    crate::packed_projected_row_commitment(&record)
}

/// Shared stable detail lowering.
#[must_use]
pub const fn packing_detail(detail: PagePackingDetail) -> Detail {
    match detail {
        PagePackingDetail::Summary => Detail::Summary,
        PagePackingDetail::Standard => Detail::Standard,
        PagePackingDetail::Full => Detail::Full,
    }
}

#[derive(Serialize)]
struct Content<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireBudget {
    bytes: usize,
    estimated_tokens: usize,
    bytes_per_token: usize,
    limit_bytes: usize,
    scope: &'static str,
}
#[derive(Serialize)]
struct Meta {
    #[serde(rename = "backend/wireBudget")]
    budget: WireBudget,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tool<'a> {
    content: [Content<'a>; 1],
    structured_content: RecordsProjection<'a>,
    is_error: bool,
    #[serde(rename = "_meta")]
    meta: Meta,
}
#[derive(Serialize)]
struct Rpc<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: Tool<'a>,
}

fn tool<'a>(
    answer: &'a RecordList,
    detail: Detail,
    next: Option<&'a str>,
    text: &'a str,
    bytes: usize,
    scope: PagePackingScope,
    credit: usize,
) -> Tool<'a> {
    Tool {
        content: [Content { kind: "text", text }],
        structured_content: RecordsProjection {
            list: answer,
            detail,
            next_cursor: next,
        },
        is_error: false,
        meta: Meta {
            budget: WireBudget {
                bytes,
                estimated_tokens: estimate_tokens(bytes),
                bytes_per_token: crate::ESTIMATED_BYTES_PER_TOKEN,
                limit_bytes: credit,
                scope: if scope == PagePackingScope::StdioJsonRpcLine {
                    "complete_jsonrpc_line"
                } else {
                    "complete_jsonrpc_body"
                },
            },
        },
    }
}

fn count_mcp(
    answer: &RecordList,
    detail: Detail,
    next: Option<&str>,
    text: &str,
    id: &Value,
    scope: PagePackingScope,
    credit: usize,
) -> usize {
    let suffix = usize::from(scope == PagePackingScope::StdioJsonRpcLine);
    let mut bytes = 0;
    for _ in 0..8 {
        let measured = serialized_json_bytes(&Rpc {
            jsonrpc: "2.0",
            id,
            result: tool(answer, detail, next, text, bytes, scope, credit),
        })
        .saturating_add(suffix);
        if measured == bytes {
            return bytes;
        }
        bytes = measured;
    }
    serialized_json_bytes(&Rpc {
        jsonrpc: "2.0",
        id,
        result: tool(answer, detail, next, text, bytes, scope, credit),
    })
    .saturating_add(suffix)
}

fn preview(answer: &RecordList) -> String {
    let list = answer;
    let mut text = String::with_capacity(MAX_PREVIEW_TEXT_BYTES);
    // Append source strings directly under the preview bound. No complete
    // Markdown rendering or oversized duplicate text is materialized first.
    for record in list.records() {
        let name = record.identity().name();
        let coordinate = record.identity().coordinate().as_str();
        let section_bytes = name
            .len()
            .saturating_add(coordinate.len())
            .saturating_add(3);
        let room = MAX_PREVIEW_TEXT_BYTES
            .saturating_sub(text.len())
            .saturating_sub(JSONRPC_PREVIEW_MARKER.len());
        if section_bytes > room {
            text.push_str(JSONRPC_PREVIEW_MARKER);
            return text;
        }
        text.push_str(name);
        text.push('\n');
        text.push_str(coordinate);
        text.push_str("\n\n");
    }
    text
}

fn plan(
    answer: &RecordList,
    detail: Detail,
    next: Option<&str>,
    call: &PackedCallContext,
    scope: PagePackingScope,
    credit: usize,
) -> (usize, String) {
    let PackedCallContext::Mcp { id, .. } = call else {
        return (
            serialized_json_bytes(&RecordsProjection {
                list: answer,
                detail,
                next_cursor: next,
            }),
            String::new(),
        );
    };
    let text = preview(answer);
    let bytes = count_mcp(answer, detail, next, &text, id, scope, credit);
    if bytes <= credit {
        return (bytes, text);
    }
    let minimum = count_mcp(
        answer,
        detail,
        next,
        JSONRPC_PREVIEW_MARKER,
        id,
        scope,
        credit,
    );
    if minimum > credit {
        return (minimum, JSONRPC_PREVIEW_MARKER.to_owned());
    }
    let prefix = text.strip_suffix(JSONRPC_PREVIEW_MARKER).unwrap_or(&text);
    let (mut lower, mut upper) = (0, prefix.len());
    let mut best = JSONRPC_PREVIEW_MARKER.to_owned();
    let mut best_bytes = minimum;
    while lower < upper {
        let midpoint = lower + (upper - lower).div_ceil(2);
        let mut end = midpoint;
        while !prefix.is_char_boundary(end) {
            end -= 1;
        }
        // Duplicate previews retain complete copyable record operands only.
        end = prefix[..end].rfind("\n\n").map_or(0, |at| at + 2);
        let candidate = format!("{}{JSONRPC_PREVIEW_MARKER}", &prefix[..end]);
        let measured = count_mcp(answer, detail, next, &candidate, id, scope, credit);
        if measured <= credit {
            lower = midpoint;
            best = candidate;
            best_bytes = measured;
        } else {
            upper = midpoint - 1;
        }
    }
    (best_bytes, best)
}

/// Complete applicable response measurement, including current MCP framing.
#[must_use]
pub fn measure_packed_search(
    answer: &RecordList,
    detail: Detail,
    next: Option<&str>,
    call: &PackedCallContext,
    scope: PagePackingScope,
    credit: usize,
) -> usize {
    plan(answer, detail, next, call, scope, credit).0
}

/// Exact size of the smallest no-record response under the selected source
/// coverage and call framing. Available is the shortest semantic status;
/// Complete has no cursor and the empty preview contributes no text. This is
/// a lower bound only, never a fabricated query result or a More estimate.
#[must_use]
pub fn minimum_packed_search_bytes(
    query: &str,
    coverage: &[Coverage],
    request: &backend_library::PackedQueryRequest,
) -> usize {
    let policy = request.policy();
    let records =
        packed_search_records(query, &[], coverage, SemanticSearchStatus::Available, false);
    let detail = packing_detail(policy.detail());
    match request.call() {
        PackedCallContext::CliJson => serialized_json_bytes(&RecordsProjection {
            list: &records,
            detail,
            next_cursor: None,
        }),
        PackedCallContext::Mcp { id, .. } => count_mcp(
            &records,
            detail,
            None,
            "",
            id,
            policy.scope(),
            policy.bytes() as usize,
        ),
    }
}

/// Materializes the same admitted MCP result measured by the owner.
pub fn packed_search_tool_result(
    answer: &RecordList,
    detail: Detail,
    next: Option<&str>,
    call: &PackedCallContext,
    scope: PagePackingScope,
    credit: usize,
) -> Result<Value, BudgetExceeded> {
    let (bytes, text) = plan(answer, detail, next, call, scope, credit);
    if bytes > credit {
        return Err(BudgetExceeded {
            bytes,
            budget: credit,
        });
    }
    serde_json::to_value(tool(answer, detail, next, &text, bytes, scope, credit)).map_err(|_| {
        BudgetExceeded {
            bytes: usize::MAX,
            budget: credit,
        }
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{JsonRpcFraming, PackedPageWorker, jsonrpc_response_bytes};
    use backend_library::{
        Cursor, PackedPageCheckpoint, PackedPageContext, PagePackingPolicy, PagePackingProfile,
        QueryLimit, SemanticSearchStatus,
    };
    use serde_json::json;

    #[test]
    fn escaped_dynamic_id_is_metered_without_truncation_under_original_envelope_credit() {
        let owner = Cursor::new();
        let scope = PagePackingScope::StdioJsonRpcLine;
        let credit = backend_library::MAX_PACKED_WIRE_BYTES;
        let policy = PagePackingPolicy::new(
            QueryLimit::new(200).expect("rows"),
            credit,
            PagePackingDetail::Full,
            PagePackingProfile::McpToolV1,
            scope,
        )
        .expect("policy");
        let context = PackedPageContext::new(
            owner,
            owner.root().into(),
            backend_library::view_key(b"actual ID envelope"),
            policy,
        )
        .expect("context");
        for id in [json!("x".repeat(8193)), json!("\0".repeat(8193))] {
            let call = PackedCallContext::Mcp {
                id: id.clone(),
                expiry: 123,
            };
            let records =
                packed_search_records("self", &[], &[], SemanticSearchStatus::Available, false);
            let required =
                measure_packed_search(&records, Detail::Full, None, &call, scope, credit as usize);
            let mut worker = PackedPageWorker::new(std::iter::empty::<Row>(), context, 0);
            let result = worker.next_page(
                |row| packed_record_commitment(row, PagePackingDetail::Full),
                |rows, _| {
                    let records = packed_search_records(
                        "self",
                        rows,
                        &[],
                        SemanticSearchStatus::Available,
                        false,
                    );
                    Ok::<_, String>(measure_packed_search(
                        &records,
                        Detail::Full,
                        None,
                        &call,
                        scope,
                        credit as usize,
                    ))
                },
            );
            if required <= credit as usize {
                let page = result.expect("large scalar ID still fits");
                assert!(page.rows.is_empty());
                assert_eq!(
                    page.checkpoint.stop(),
                    backend_library::PackedPageStop::Complete
                );
                let result = packed_search_tool_result(
                    &records,
                    Detail::Full,
                    None,
                    &call,
                    scope,
                    credit as usize,
                )
                .expect("actual leaf egress");
                let response = json!({"jsonrpc":"2.0", "id":id, "result":result});
                assert_eq!(response["id"], id);
                assert_eq!(
                    jsonrpc_response_bytes(&response, JsonRpcFraming::StdioLine),
                    required
                );
            } else {
                assert!(
                    matches!(result, Err(crate::PackedPageFailure::EnvelopeExceedsWireCredit {
                    required_bytes, credit_bytes
                }) if required_bytes == required && credit_bytes == credit as usize)
                );
                assert!(required > credit as usize);
                assert_eq!(call, PackedCallContext::Mcp { id, expiry: 123 });
            }
        }
    }

    #[test]
    fn actual_record_projection_cost_equals_complete_rpc_bytes_under_original_credit() {
        let owner = Cursor::new();
        let basis =
            backend_library::Basis::new(owner.root(), backend_library::object_version(b"records"));
        let all = (0..200)
            .map(|at| {
                backend_library::Row::new(
                    backend_library::RowId::Symbol(backend_library::symbol_key(&format!(
                        "record-{at}"
                    ))),
                    basis,
                    format!("/project::src/lib.py:{}::record_{at}_é", at + 1),
                )
                .with_signature(format!(
                    "def record_{at}({})",
                    "é_quoted_\\\"argument, ".repeat(60)
                ))
            })
            .collect::<Vec<_>>();
        for credit in [12 * 1024, 48 * 1024] {
            for detail in [PagePackingDetail::Summary, PagePackingDetail::Full] {
                for scope in [
                    PagePackingScope::StdioJsonRpcLine,
                    PagePackingScope::HttpJsonRpcBody,
                ] {
                    let policy = PagePackingPolicy::new(
                        QueryLimit::new(200).expect("rows"),
                        credit,
                        detail,
                        PagePackingProfile::McpToolV1,
                        scope,
                    )
                    .expect("policy");
                    let context = PackedPageContext::new(
                        owner,
                        owner.root().into(),
                        backend_library::view_key(b"actual records"),
                        policy,
                    )
                    .expect("context");
                    let mut worker = PackedPageWorker::new(all.clone().into_iter(), context, 0);
                    let mut observed = Vec::new();
                    for ordinal in 0..100 {
                        let id = json!(format!("dynamic-id-{ordinal}-é-\\\""));
                        let call = PackedCallContext::Mcp {
                            id: id.clone(),
                            expiry: 123,
                        };
                        let page = worker
                            .next_page(
                                |row| packed_record_commitment(row, detail),
                                |rows, checkpoint| {
                                    let cursor = checkpoint
                                        .stop()
                                        .next_row()
                                        .map(|_| PackedPageCheckpoint::cost_token());
                                    let records = packed_search_records(
                                        "é",
                                        rows,
                                        &[],
                                        SemanticSearchStatus::Available,
                                        cursor.is_some(),
                                    );
                                    Ok::<_, String>(measure_packed_search(
                                        &records,
                                        packing_detail(detail),
                                        cursor.as_deref(),
                                        &call,
                                        scope,
                                        credit as usize,
                                    ))
                                },
                            )
                            .expect("actual record page");
                        let cursor = page
                            .checkpoint
                            .stop()
                            .next_row()
                            .map(|_| PackedPageCheckpoint::cost_token());
                        let records = packed_search_records(
                            "é",
                            &page.rows,
                            &[],
                            SemanticSearchStatus::Available,
                            cursor.is_some(),
                        );
                        let result = packed_search_tool_result(
                            &records,
                            packing_detail(detail),
                            cursor.as_deref(),
                            &call,
                            scope,
                            credit as usize,
                        )
                        .expect("actual shared egress");
                        assert_eq!(
                            result["structuredContent"]["records"]
                                .as_array()
                                .expect("records")
                                .len(),
                            page.rows.len()
                        );
                        assert_eq!(result["_meta"]["backend/wireBudget"]["limitBytes"], credit);
                        let preview = result["content"][0]["text"].as_str().expect("preview");
                        let shown = preview
                            .strip_suffix(JSONRPC_PREVIEW_MARKER)
                            .unwrap_or(preview);
                        assert!(
                            shown.is_empty() || shown.ends_with("\n\n"),
                            "complete record operands"
                        );
                        let response = json!({"jsonrpc":"2.0", "id":id, "result":result});
                        let framing = if scope == PagePackingScope::StdioJsonRpcLine {
                            JsonRpcFraming::StdioLine
                        } else {
                            JsonRpcFraming::HttpBody
                        };
                        let actual = jsonrpc_response_bytes(&response, framing);
                        assert_eq!(actual, page.checkpoint.measured_bytes() as usize);
                        assert!(actual <= credit as usize);
                        observed.extend(page.rows);
                        if !page.checkpoint.stop().next_row().is_some() {
                            break;
                        }
                    }
                    assert_eq!(
                        observed, all,
                        "every actual row and complete signature survives"
                    );
                }
            }
        }
    }

    #[test]
    fn coherently_remeasured_complete_metadata_cannot_replace_an_issued_more_claim() {
        let owner = Cursor::new();
        let scope = PagePackingScope::StdioJsonRpcLine;
        let credit = backend_library::MAX_PACKED_WIRE_BYTES;
        let policy = PagePackingPolicy::new(
            QueryLimit::new(200).expect("rows"),
            credit,
            PagePackingDetail::Full,
            PagePackingProfile::McpToolV1,
            scope,
        )
        .expect("policy");
        let context = PackedPageContext::new(
            owner,
            owner.root().into(),
            backend_library::view_key(b"receipt law"),
            policy,
        )
        .expect("context");
        let row = Row::new(
            backend_library::RowId::Symbol(backend_library::symbol_key("retained row")),
            backend_library::Basis::new(owner.root(), backend_library::object_version(b"row")),
            "/project::src/lib.py:1::retained_row",
        );
        let call = PackedCallContext::Mcp {
            id: json!(1),
            expiry: 123,
        };
        let cursor = PackedPageCheckpoint::cost_token();
        let records = packed_search_records(
            "self",
            std::slice::from_ref(&row),
            &[],
            SemanticSearchStatus::Available,
            true,
        );
        let original_bytes = measure_packed_search(
            &records,
            Detail::Full,
            Some(&cursor),
            &call,
            scope,
            credit as usize,
        );
        let mut prefix = backend_library::PackedPrefixCommitment::new(context, 0);
        prefix
            .push(
                0,
                packed_record_commitment(&row, PagePackingDetail::Full).expect("row commitment"),
            )
            .expect("prefix");
        let checkpoint = prefix
            .checkpoint(
                backend_library::PackedPageStop::WireCreditReached([2; 32]),
                original_bytes as u32,
            )
            .expect("More checkpoint");
        let receipt = backend_library::PackedQueryReceipt::issue(&checkpoint, |_| [1; 32]);
        let mut value = serde_json::to_value(&receipt).expect("actual receipt");
        serde_json::from_value::<backend_library::PackedQueryReceipt>(value.clone())
            .expect("canonical receipt");
        let complete =
            packed_search_records("self", &[row], &[], SemanticSearchStatus::Available, false);
        let complete_bytes =
            measure_packed_search(&complete, Detail::Full, None, &call, scope, credit as usize);
        assert_ne!(original_bytes, complete_bytes);
        value["more"] = json!(false);
        value["bytes"] = json!(complete_bytes);
        assert!(
            serde_json::from_value::<backend_library::PackedQueryReceipt>(value).is_err(),
            "a final byte equality cannot override unchanged authenticated stop claims"
        );
    }

    #[test]
    fn minimum_preflight_allows_a_fitting_terminal_without_speculative_cursor_credit() {
        let owner = Cursor::new();
        let row = Row::new(
            backend_library::RowId::Symbol(backend_library::symbol_key("one")),
            backend_library::Basis::new(owner.root(), backend_library::object_version(b"one")),
            "/project::one.py:1::one",
        );
        for scope in [
            PagePackingScope::CliJsonBody,
            PagePackingScope::StdioJsonRpcLine,
            PagePackingScope::HttpJsonRpcBody,
        ] {
            let (profile, call) = if scope == PagePackingScope::CliJsonBody {
                (PagePackingProfile::CliJsonV1, PackedCallContext::CliJson)
            } else {
                (
                    PagePackingProfile::McpToolV1,
                    PackedCallContext::Mcp {
                        id: json!("é-\\\""),
                        expiry: 123,
                    },
                )
            };
            let records = packed_search_records(
                "self",
                std::slice::from_ref(&row),
                &[Coverage::Complete],
                SemanticSearchStatus::Available,
                false,
            );
            let credit = measure_packed_search(
                &records,
                Detail::Full,
                None,
                &call,
                scope,
                backend_library::MAX_PACKED_WIRE_BYTES as usize,
            ) as u32;
            let policy = PagePackingPolicy::new(
                QueryLimit::new(200).expect("rows"),
                credit,
                PagePackingDetail::Full,
                profile,
                scope,
            )
            .expect("actual terminal credit");
            let request =
                backend_library::PackedQueryRequest::new(policy, call.clone(), None).expect("call");
            let minimum = minimum_packed_search_bytes("self", &[Coverage::Complete], &request);
            assert!(
                minimum <= credit as usize,
                "minimum never reserves a speculative More cursor"
            );
            let more = packed_search_records(
                "self",
                &[row.clone()],
                &[Coverage::Complete],
                SemanticSearchStatus::Available,
                true,
            );
            assert!(
                measure_packed_search(
                    &more,
                    Detail::Full,
                    Some(&PackedPageCheckpoint::cost_token()),
                    &call,
                    scope,
                    credit as usize
                ) > credit as usize
            );
            let context = PackedPageContext::new(
                owner,
                owner.root().into(),
                backend_library::view_key(b"one terminal"),
                policy,
            )
            .expect("context");
            let mut worker = PackedPageWorker::new(std::iter::once(row.clone()), context, 0);
            let page = worker
                .next_page(
                    |row| packed_record_commitment(row, PagePackingDetail::Full),
                    |rows, checkpoint| {
                        let next = checkpoint
                            .stop()
                            .next_row()
                            .map(|_| PackedPageCheckpoint::cost_token());
                        let records = packed_search_records(
                            "self",
                            rows,
                            &[Coverage::Complete],
                            SemanticSearchStatus::Available,
                            next.is_some(),
                        );
                        Ok::<_, String>(measure_packed_search(
                            &records,
                            Detail::Full,
                            next.as_deref(),
                            &call,
                            scope,
                            credit as usize,
                        ))
                    },
                )
                .expect("actual one-row terminal fits");
            assert_eq!(page.rows, vec![row.clone()]);
            assert_eq!(
                page.checkpoint.stop(),
                backend_library::PackedPageStop::Complete
            );
        }
    }
}
