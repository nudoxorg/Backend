//! Build the checked-in MCP payload fixtures from the same typed projections
//! used by CLI and MCP.  This binary is a development/test tool; it is not a
//! dependency of either request path.

use allocation_counter::measure;
use backend_library::{
    Basis, Coverage, DeclarationKind, Row, RowId, object_version, symbol_key, view_state_root,
};
use backend_library::{HealthReport, Library, OutlineExtent, OutlineNode};
use backend_present::{
    Answer, Cause, CauseSlug, CoverageLine, Detail, Fault, FaultSlug, Identity, Operand,
    OutlineEntry, OutlineTree, PackagePath, Page, ProductRecord, ProductView, ProjectRef, Prose,
    Readiness, Record, RecordList, RowCount, Shelf, ShelfEntry, Signature, Source, SourceSite,
    Status, Truncation, bounded_text, encode_answer, encode_serializable, fault_value, markdown,
    oversized_fault,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;

const DEFAULT_BUDGET: usize = backend_present::DEFAULT_RESPONSE_BUDGET_BYTES;
const MAX_BUDGET: usize = backend_present::MAX_RESPONSE_BUDGET_BYTES;

#[derive(Serialize)]
struct AllocationMeasurement {
    count_total: u64,
    bytes_total: u64,
    count_max: u64,
    bytes_max: u64,
}

#[derive(Serialize)]
struct Refusal {
    observed_bytes: usize,
    budget_bytes: usize,
    partial_bytes: usize,
}

#[derive(Serialize)]
struct QueryPageFixture {
    revision: String,
    source: String,
    rows: Vec<QueryRowFixture>,
    terminal: String,
}

#[derive(Serialize)]
struct QueryRowFixture {
    coordinate: String,
    fields: Vec<(String, String)>,
}

#[derive(Serialize)]
struct GraphQueryFixture {
    revision: String,
    rows: Vec<BTreeMap<String, String>>,
    terminal: String,
    emitted: usize,
}

fn main() {
    if env::args().any(|argument| argument == "--tools") {
        println!(
            "{}",
            serde_json::to_string(&tool_matrix()).expect("tool matrix is serializable")
        );
        return;
    }
    let write = env::args().any(|argument| argument == "--write");
    let output = fixtures();
    if write {
        eprintln!("--write is intentionally handled by measure.py; stdout remains JSON");
    }
    println!(
        "{}",
        serde_json::to_string(&output).expect("fixture output is serializable")
    );
}

/// Build the complete MCP call matrix from the same registry projection that
/// `tools/list` serves. Each row is a real JSON-RPC success envelope, so the
/// byte count includes both the readable text block and structured content.
/// The matrix intentionally uses bounded worst-case fixtures: 200 records,
/// long source/document pages, and 200 graph rows. An oversized projection is
/// represented by the same typed fault the server returns, with no partial
/// payload retained.
fn tool_matrix() -> Value {
    let tools = backend_mcp::token_budget_tools()["tools"]
        .as_array()
        .expect("canonical MCP tool table is an array")
        .clone();
    let rows = tools
        .into_iter()
        .flat_map(|tool| {
            let tool_name = tool["name"]
                .as_str()
                .expect("every MCP tool has a name")
                .to_owned();
            let details = detail_modes(&tool);
            details.into_iter().map(move |name| {
                let detail = matrix_detail(&tool, &name);
                tool_matrix_row(&tool_name, &name, detail)
            })
        })
        .collect::<Vec<_>>();
    Value::Array(rows)
}

fn detail_modes(tool: &Value) -> Vec<String> {
    let Some(values) = tool["inputSchema"]["properties"]["detail"]["enum"].as_array() else {
        return vec!["default".to_owned()];
    };
    let mut modes = Vec::new();
    if values.iter().any(|value| value.as_str() == Some("summary")) {
        modes.push("compact".to_owned());
    }
    if values
        .iter()
        .any(|value| value.as_str() == Some("standard"))
    {
        modes.push("default".to_owned());
    }
    if values.iter().any(|value| value.as_str() == Some("full")) {
        modes.push("full".to_owned());
    }
    if !modes.iter().any(|mode| mode == "default") {
        modes.insert(0, "default".to_owned());
    }
    modes
}

fn matrix_detail(tool: &Value, name: &str) -> Detail {
    match name {
        "full" => Detail::Full,
        "default" => match tool["inputSchema"]["properties"]["detail"]["default"].as_str() {
            Some("standard") => Detail::Standard,
            _ => Detail::Summary,
        },
        _ => Detail::Summary,
    }
}

#[derive(Serialize)]
struct ToolMatrixMeasurement {
    tool: String,
    detail: String,
    fixture: String,
    admitted: bool,
    structured_bytes: usize,
    tool_result_bytes: usize,
    rpc_bytes: usize,
    estimated_tokens_upper_bound: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal_bytes: Option<usize>,
    payload: String,
}

fn tool_matrix_row(tool: &str, detail_name: &str, detail: Detail) -> Value {
    if tool == "backend.query" {
        return query_matrix_row(tool, detail_name, detail);
    }

    let fixture = answer_fixture_name(tool);
    let answer = answer_fixture(tool);
    match encode_answer(&answer, detail, None, DEFAULT_BUDGET) {
        Ok(encoded) => {
            let structured: Value =
                serde_json::from_slice(&encoded.bytes).expect("typed answer is JSON");
            matrix_success(
                tool,
                detail_name,
                fixture,
                bounded_text(&markdown::answer(&answer)),
                structured,
            )
        }
        Err(error) => matrix_refusal(tool, detail_name, fixture, error.bytes),
    }
}

fn query_matrix_row(tool: &str, detail_name: &str, detail: Detail) -> Value {
    let body = worst_graph_page();
    let fixture = "graph";
    match encode_serializable("query", detail, None, &body, DEFAULT_BUDGET) {
        Ok(encoded) => {
            let structured: Value =
                serde_json::from_slice(&encoded.bytes).expect("typed query answer is JSON");
            let text = bounded_text(&query_page_text(&body));
            matrix_success(tool, detail_name, fixture, text, structured)
        }
        Err(error) => matrix_refusal(tool, detail_name, fixture, error.bytes),
    }
}

fn query_page_text(body: &GraphQueryFixture) -> String {
    let mut text = format!(
        "~query {} row(s) · complete · revision {}\n",
        body.rows.len(),
        &body.revision[..12]
    );
    for row in &body.rows {
        let fields = row
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>();
        text.push_str(&fields.join("  "));
        text.push('\n');
    }
    text
}

fn matrix_success(
    tool: &str,
    detail_name: &str,
    fixture: &str,
    text: String,
    structured: Value,
) -> Value {
    let (rpc, observed_bytes) = backend_mcp::token_budget_rpc_response_with_observed(
        serde_json::json!(1),
        &text,
        structured,
        false,
    );
    let rpc_bytes = serde_json::to_vec(&rpc).expect("MCP success envelope is JSON");
    let admitted = rpc.get("error").is_none()
        && rpc
            .get("result")
            .and_then(|result| result.get("isError"))
            .and_then(Value::as_bool)
            != Some(true);
    let tool_result_bytes = rpc
        .get("result")
        .map(|result| {
            serde_json::to_vec(result)
                .expect("MCP tool result is JSON")
                .len()
        })
        .unwrap_or(0);
    serde_json::to_value(ToolMatrixMeasurement {
        tool: tool.to_owned(),
        detail: detail_name.to_owned(),
        fixture: fixture.to_owned(),
        admitted,
        structured_bytes: rpc
            .get("result")
            .and_then(|result| result.get("structuredContent"))
            .map(|structured| {
                serde_json::to_vec(structured)
                    .expect("structured content is JSON")
                    .len()
            })
            .unwrap_or(0),
        tool_result_bytes,
        rpc_bytes: rpc_bytes.len(),
        estimated_tokens_upper_bound: backend_present::estimate_tokens(rpc_bytes.len()),
        refusal_bytes: (!admitted).then_some(observed_bytes),
        payload: String::from_utf8(rpc_bytes).expect("MCP JSON is UTF-8"),
    })
    .expect("tool matrix row is serializable")
}

fn matrix_refusal(tool: &str, detail_name: &str, fixture: &str, observed_bytes: usize) -> Value {
    let fault = oversized_fault(backend_present::BudgetExceeded {
        bytes: observed_bytes,
        budget: DEFAULT_BUDGET,
    });
    let rpc = backend_mcp::token_budget_rpc_response(
        serde_json::json!(1),
        &bounded_text(&markdown::fault(&fault)),
        fault_value(&fault),
        true,
    );
    let rpc_bytes = serde_json::to_vec(&rpc).expect("MCP refusal envelope is JSON");
    let tool_result_bytes = rpc
        .get("result")
        .map(|result| {
            serde_json::to_vec(result)
                .expect("MCP tool refusal is JSON")
                .len()
        })
        .unwrap_or(0);
    serde_json::to_value(ToolMatrixMeasurement {
        tool: tool.to_owned(),
        detail: detail_name.to_owned(),
        fixture: fixture.to_owned(),
        admitted: false,
        structured_bytes: rpc
            .get("result")
            .and_then(|result| result.get("structuredContent"))
            .map(|structured| {
                serde_json::to_vec(structured)
                    .expect("fault content is JSON")
                    .len()
            })
            .unwrap_or(0),
        tool_result_bytes,
        rpc_bytes: rpc_bytes.len(),
        estimated_tokens_upper_bound: backend_present::estimate_tokens(rpc_bytes.len()),
        refusal_bytes: Some(observed_bytes),
        payload: String::from_utf8(rpc_bytes).expect("MCP JSON is UTF-8"),
    })
    .expect("tool matrix refusal is serializable")
}

fn answer_fixture_name(tool: &str) -> &'static str {
    match tool {
        "backend.document" | "backend.source" => "document",
        "backend.search" | "backend.resolve" | "backend.related" | "backend.graph" => "records",
        "backend.outline" => "outline",
        "backend.status" => "status",
        "backend.packages" => "shelf",
        _ => "product",
    }
}

fn answer_fixture(tool: &str) -> Answer {
    match answer_fixture_name(tool) {
        "document" => long_page(),
        "records" => worst_records(),
        "outline" => worst_outline_answer(),
        "status" => status_answer(),
        "shelf" => worst_shelf_answer(),
        _ => worst_product_answer(),
    }
}

fn fixtures() -> Value {
    let mut output = serde_json::Map::new();
    output.insert(
        "common_empty_records_summary".to_owned(),
        encoded(encode_answer_measured(
            &empty_records(),
            Detail::Summary,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "worst_200_full_records".to_owned(),
        encoded(encode_answer_measured(
            &worst_records(),
            Detail::Full,
            None,
            MAX_BUDGET,
        )),
    );
    output.insert(
        "unicode_rtl_records".to_owned(),
        encoded(encode_answer_measured(
            &unicode_rtl_records(),
            Detail::Full,
            None,
            MAX_BUDGET,
        )),
    );
    output.insert(
        "long_page_document_source".to_owned(),
        encoded(encode_answer_measured(
            &long_page(),
            Detail::Full,
            None,
            MAX_BUDGET,
        )),
    );
    output.insert(
        "continuation_envelope".to_owned(),
        encoded(encode_answer_measured(
            &empty_records(),
            Detail::Summary,
            Some("mcp1-1876543210-owner-cursor-with-a-stable-width-00000001"),
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "shelf_summary".to_owned(),
        encoded(encode_answer_measured(
            &shelf_answer(),
            Detail::Summary,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "outline_summary".to_owned(),
        encoded(encode_answer_measured(
            &outline_answer(),
            Detail::Summary,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "status_summary".to_owned(),
        encoded(encode_answer_measured(
            &status_answer(),
            Detail::Summary,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "product_summary".to_owned(),
        encoded(encode_answer_measured(
            &product_answer(),
            Detail::Summary,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert("query_page".to_owned(), encoded(encode_query_measured()));
    output.insert(
        "error_fault".to_owned(),
        plain(fault_value(&Fault::new(
            FaultSlug::Usage,
            Operand::Argument("coordinate".to_owned()),
            Cause::new(CauseSlug::Malformed, "the coordinate is not valid"),
            backend_present::Affordance::None,
        ))),
    );
    output.insert(
        "oversized_fault".to_owned(),
        plain(fault_value(&oversized_fault(
            backend_present::BudgetExceeded {
                bytes: 100_025,
                budget: DEFAULT_BUDGET,
            },
        ))),
    );
    output.insert(
        "atomic_oversized_refusal".to_owned(),
        refused(encode_answer_measured(
            &worst_records(),
            Detail::Full,
            None,
            DEFAULT_BUDGET,
        )),
    );
    output.insert(
        "tool_schema".to_owned(),
        plain(backend_mcp::token_budget_tools()),
    );
    Value::Object(output)
}

fn encoded(
    (result, allocations): (
        Result<backend_present::EncodedPayload, backend_present::BudgetExceeded>,
        AllocationMeasurement,
    ),
) -> Value {
    match result {
        Ok(payload) => {
            let mut result = Measurement {
                bytes: 0,
                estimated_tokens: 0,
                allocations,
                admitted: false,
                payload: None,
            };
            result.bytes = payload.budget.bytes;
            result.estimated_tokens = payload.budget.estimated_tokens;
            result.admitted = true;
            result.payload =
                Some(String::from_utf8(payload.bytes.into_vec()).expect("JSON is UTF-8"));
            serde_json::to_value(result).expect("fixture result is serializable")
        }
        Err(error) => {
            serde_json::to_value(RefusalMeasurement::from(error)).expect("refusal is serializable")
        }
    }
}

fn plain(payload: Value) -> Value {
    let mut bytes = Vec::new();
    let allocations = allocation_for(|| {
        bytes = serde_json::to_vec(&payload).expect("fixture value is serializable");
    });
    let mut result = Measurement {
        bytes: 0,
        estimated_tokens: 0,
        allocations,
        admitted: false,
        payload: None,
    };
    result.bytes = bytes.len();
    result.estimated_tokens = backend_present::estimate_tokens(bytes.len());
    result.admitted = true;
    result.payload = Some(String::from_utf8(bytes).expect("JSON is UTF-8"));
    serde_json::to_value(result).expect("fixture result is serializable")
}

fn refused(
    (result, allocations): (
        Result<backend_present::EncodedPayload, backend_present::BudgetExceeded>,
        AllocationMeasurement,
    ),
) -> Value {
    match result {
        Ok(payload) => panic!(
            "oversized fixture unexpectedly admitted {} bytes",
            payload.bytes.len()
        ),
        Err(error) => serde_json::to_value(RefusalMeasurement {
            bytes: error.bytes,
            estimated_tokens: backend_present::estimate_tokens(error.bytes),
            allocations,
            admitted: false,
            payload: None,
            refusal: Some(Refusal {
                observed_bytes: error.bytes,
                budget_bytes: error.budget,
                partial_bytes: 0,
            }),
        })
        .expect("refusal is serializable"),
    }
}

#[derive(Serialize)]
struct RefusalMeasurement {
    bytes: usize,
    estimated_tokens: usize,
    allocations: AllocationMeasurement,
    admitted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal: Option<Refusal>,
}

impl From<backend_present::BudgetExceeded> for RefusalMeasurement {
    fn from(error: backend_present::BudgetExceeded) -> Self {
        Self {
            bytes: error.bytes,
            estimated_tokens: backend_present::estimate_tokens(error.bytes),
            allocations: AllocationMeasurement {
                count_total: 0,
                bytes_total: 0,
                count_max: 0,
                bytes_max: 0,
            },
            admitted: false,
            payload: None,
            refusal: Some(Refusal {
                observed_bytes: error.bytes,
                budget_bytes: error.budget,
                partial_bytes: 0,
            }),
        }
    }
}

#[derive(Serialize)]
struct Measurement {
    bytes: usize,
    estimated_tokens: usize,
    allocations: AllocationMeasurement,
    admitted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<String>,
}

fn allocation_for(operation: impl FnOnce()) -> AllocationMeasurement {
    let info = measure(operation);
    AllocationMeasurement {
        count_total: info.count_total,
        bytes_total: info.bytes_total,
        count_max: info.count_max,
        bytes_max: info.bytes_max,
    }
}

fn encode_answer_measured(
    answer: &Answer,
    detail: Detail,
    next_cursor: Option<&str>,
    budget: usize,
) -> (
    Result<backend_present::EncodedPayload, backend_present::BudgetExceeded>,
    AllocationMeasurement,
) {
    let mut result = None;
    let allocations = allocation_for(|| {
        result = Some(encode_answer(answer, detail, next_cursor, budget));
    });
    (
        result.expect("encoder always returns a result"),
        allocations,
    )
}

fn encode_query_measured() -> (
    Result<backend_present::EncodedPayload, backend_present::BudgetExceeded>,
    AllocationMeasurement,
) {
    let body = QueryPageFixture {
        revision: "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        source: "1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
        rows: vec![QueryRowFixture {
            coordinate: "/workspace/project::src/lib.rs:2::ferris".to_owned(),
            fields: vec![
                ("kind".to_owned(), "function".to_owned()),
                ("language".to_owned(), "rust".to_owned()),
            ],
        }],
        terminal: "complete".to_owned(),
    };
    let mut result = None;
    let allocations = allocation_for(|| {
        result = Some(encode_serializable(
            "query",
            Detail::Summary,
            None,
            &body,
            DEFAULT_BUDGET,
        ));
    });
    (
        result.expect("query encoder always returns a result"),
        allocations,
    )
}

fn worst_graph_page() -> GraphQueryFixture {
    GraphQueryFixture {
        revision: backend_library::encode_id(&[0; 32]),
        rows: (0..200)
            .map(|index| {
                BTreeMap::from([
                    (
                        "coordinate".to_owned(),
                        format!(
                            "/workspace/مرحبا/project::src/module_{index}/declaration_{index}:2147483647::declaration_{index}_東京"
                        ),
                    ),
                    (
                        "documentation".to_owned(),
                        "bounded graph documentation ".repeat(24),
                    ),
                    (
                        "language".to_owned(),
                        "rust".to_owned(),
                    ),
                    (
                        "signature".to_owned(),
                        "pub fn declaration(value: &str) -> Result<(), Error>".to_owned(),
                    ),
                ])
            })
            .collect(),
        terminal: "complete".to_owned(),
        emitted: 200,
    }
}

fn basis() -> Basis {
    Basis::new(view_state_root(&[]), object_version(&[]))
}

fn empty_records() -> Answer {
    Answer::Records(Box::new(RecordList::new(
        "ferris",
        CoverageLine::new(&[], Some(0)),
        Vec::<Record>::new(),
    )))
}

fn worst_records() -> Answer {
    let basis = basis();
    let rows = (0..200)
        .map(|index| {
            let row = Row::new(
                RowId::Symbol(symbol_key(&format!("typed-budget-{index}"))),
                basis,
                format!(
                    "/workspace/project::src/module_{index}/declaration_{index}:1::declaration_{index}"
                ),
            )
            .with_kind(DeclarationKind::Function)
            .with_signature("pub fn declaration() -> Result<(), Error>");
            Record::from_row(&row).with_summary("A bounded fixture declaration.")
        })
        .collect::<Vec<_>>();
    Answer::Records(Box::new(
        RecordList::new(
            "ferris",
            CoverageLine::new(&[Coverage::Complete], Some(200)),
            rows,
        )
        .with_more(true),
    ))
}

fn unicode_rtl_records() -> Answer {
    let basis = basis();
    let row = Row::new(
        RowId::Symbol(symbol_key("/workspace/مرحبا::src/שלום.rs:7::函数_東京")),
        basis,
        "/workspace/مرحبا::src/שלום.rs:7::函数_東京",
    )
    .with_kind(DeclarationKind::Function)
    .with_signature("pub fn שלום_東京() -> Result<(), Ошибка>");
    Answer::Records(Box::new(RecordList::new(
        "café 你好 مرحبا שלום",
        CoverageLine::new(&[Coverage::Complete], Some(1)),
        vec![Record::from_row(&row).with_summary("Unicode and RTL summary: العربية עברית 日本語.")],
    )))
}

fn long_page() -> Answer {
    let identity = Identity::parse(
        "/workspace/long-project::src/非常に長いモジュール名/long_declaration_name_with_stable_identifier:2147483647::long_declaration_name_with_stable_identifier",
    );
    let source_text = (0..48)
        .map(|line| format!("    let value_{line:02} = \"source line {line} — مرحبا שלום\";"))
        .collect::<Vec<_>>()
        .join("\n");
    let prose = (0..12)
        .map(|paragraph| {
            Prose::Text(format!(
                "documentation paragraph {paragraph}: {}",
                "long documentation text ".repeat(20)
            ))
        })
        .collect::<Vec<_>>();
    let source = Source::Captured {
        site: SourceSite::new(
            PackagePath::new("src/非常に長いモジュール名/long_declaration_name.rs"),
            backend_present::LineNumber::new(17).expect("nonzero line"),
        ),
        lines: Source::number_lines(
            &source_text,
            backend_present::LineNumber::new(17).expect("nonzero line"),
        ),
        truncation: Truncation::Complete,
    };
    Answer::Page(Box::new(
        Page::new(identity, Some(DeclarationKind::Function), source)
            .with_signature(Signature::tokenize(
                "pub fn long_declaration_name_with_stable_identifier(value: &str) -> Result<(), Error>",
                backend_present::Language::Rust,
            ))
            .with_prose(prose),
    ))
}

fn shelf_answer() -> Answer {
    let identity = Identity::parse("/workspace/project");
    Answer::Shelf(Box::new(Shelf::new(
        backend_present::KeyTag::from_key(&[0xab; 32]),
        vec![
            ShelfEntry::new(identity.clone(), Readiness::Ready),
            ShelfEntry::new(identity, Readiness::Requested),
        ],
    )))
}

fn worst_shelf_answer() -> Answer {
    let entries = (0..200)
        .map(|index| {
            ShelfEntry::new(
                Identity::parse(&format!("/workspace/مرحبا/project_{index}/src/東京")),
                Readiness::Indexing {
                    rows: RowCount::new(index as u64 * 17),
                },
            )
        })
        .collect::<Vec<_>>();
    Answer::Shelf(Box::new(Shelf::new(
        backend_present::KeyTag::from_key(&[0xcd; 32]),
        entries,
    )))
}

fn outline_answer() -> Answer {
    let node = OutlineNode {
        symbol: symbol_key("/workspace/project::src/lib.rs"),
        children: Box::new([]),
    };
    let mut resolver = |_symbol| None;
    let entry = OutlineEntry::resolve(&node, &mut resolver);
    Answer::Outline(Box::new(OutlineTree::new(
        Identity::parse("/workspace/project"),
        vec![entry],
        OutlineExtent::Truncated,
    )))
}

fn worst_outline_answer() -> Answer {
    let roots = (0..200)
        .map(|index| {
            let node = OutlineNode {
                symbol: symbol_key(&format!(
                    "/workspace/project::src/module_{index}/declaration_{index}"
                )),
                children: Box::new([]),
            };
            let mut resolver = |_symbol| {
                Some((
                    Identity::parse(&format!(
                        "/workspace/project::src/module_{index}/declaration_{index}"
                    )),
                    Some(DeclarationKind::Function),
                ))
            };
            OutlineEntry::resolve(&node, &mut resolver)
        })
        .collect::<Vec<_>>();
    Answer::Outline(Box::new(OutlineTree::new(
        Identity::parse("/workspace/project"),
        roots,
        OutlineExtent::Truncated,
    )))
}

fn status_answer() -> Answer {
    let library = Library::new();
    let report = HealthReport::from_root(library.view(), library.cursor());
    Answer::Status(Box::new(Status::from_report(
        &report,
        Some(ProjectRef::new("/workspace/project")),
    )))
}

fn product_answer() -> Answer {
    Answer::Product(Box::new(ProductView::stated("tree", "no node is open")))
}

fn worst_product_answer() -> Answer {
    let records = (0..200)
        .map(|index| {
            ProductRecord::new(
                format!("package {index} — مرحبا 東京 declaration"),
                Some(format!(
                    "pkg:cargo/example-{index}@1.0.0::src/module_{index}"
                )),
                vec![
                    "registry".to_owned(),
                    "compiler".to_owned(),
                    "bounded product fixture".to_owned(),
                ],
            )
        })
        .collect();
    Answer::Product(Box::new(ProductView::assembled("surface", records)))
}
