//! Build the checked-in MCP payload fixtures from the same typed projections
//! used by CLI and MCP.  This binary is a development/test tool; it is not a
//! dependency of either request path.

use allocation_counter::measure;
use backend_library::{
    object_version, symbol_key, view_state_root, Basis, Coverage, DeclarationKind, Row, RowId,
};
use backend_library::{HealthReport, Library, OutlineExtent, OutlineNode};
use backend_present::{
    encode_answer, encode_serializable, fault_value, oversized_fault, Answer, Cause, CauseSlug,
    CoverageLine, Detail, Fault, FaultSlug, Identity, Operand, OutlineEntry, OutlineTree,
    PackagePath, Page, ProductView, ProjectRef, Prose, Readiness, Record, RecordList, Shelf,
    ShelfEntry, Signature, Source, SourceSite, Status, Truncation,
};
use serde::Serialize;
use serde_json::Value;
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

fn main() {
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
