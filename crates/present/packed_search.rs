//! Surface adapters over the shared pure query-page output recipe.
use crate::{Answer, BudgetExceeded, Detail};
use backend_library::{Coverage, PackedCallContext, PagePackingScope, Row, SemanticSearchStatus};
pub use backend_page_output::{packed_record_commitment, packed_search_records, packing_detail};
#[must_use]
pub fn packed_search_answer(
    query: &str,
    rows: &[Row],
    coverage: &[Coverage],
    status: SemanticSearchStatus,
    more: bool,
) -> Answer {
    Answer::Records(Box::new(packed_search_records(
        query, rows, coverage, status, more,
    )))
}
#[must_use]
pub fn measure_packed_search(
    answer: &Answer,
    detail: Detail,
    next: Option<&str>,
    call: &PackedCallContext,
    scope: PagePackingScope,
    credit: usize,
) -> usize {
    match answer {
        Answer::Records(list) => {
            backend_page_output::measure_packed_search(list, detail, next, call, scope, credit)
        }
        _ => usize::MAX,
    }
}
pub fn packed_search_tool_result(
    answer: &Answer,
    detail: Detail,
    next: Option<&str>,
    call: &PackedCallContext,
    scope: PagePackingScope,
    credit: usize,
) -> Result<serde_json::Value, BudgetExceeded> {
    match answer {
        Answer::Records(list) => {
            backend_page_output::packed_search_tool_result(list, detail, next, call, scope, credit)
        }
        _ => Err(BudgetExceeded {
            bytes: usize::MAX,
            budget: credit,
        }),
    }
}
