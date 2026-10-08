//! Pure source-row to record-list assembly shared by producers and surfaces.
use crate::{CoverageLine, Record, RecordList};
use backend_library::{Coverage, Row};

/// Builds one result page from rows a surface already holds.
///
/// The desktop reads rows and coverage out of a reply without retaining the
/// snapshot they arrived in; it calls this so its result rows are derived by
/// the same function the CLI and MCP surfaces use rather than by a second
/// projection that could drift from it.
#[must_use]
pub fn record_list_from_rows(
    query: &str,
    rows: &[Row],
    coverage: &[Coverage],
    more: bool,
) -> RecordList {
    let line = CoverageLine::new(coverage, u64::try_from(rows.len()).ok());
    let mut records = rows
        .iter()
        .map(|row| {
            let record = Record::from_row(row);
            match row_summary(row) {
                Some(text) => record.with_summary(text),
                None => record,
            }
        })
        .collect::<Vec<_>>();
    // View rows are ordered by canonical identity for hashing and paging.
    // Scored search rows carry the owner's absolute rank independently of
    // that storage order. Restore it only in the readable projection; a
    // stable sort leaves unscored rows and equal scores in their input order.
    records.sort_by_key(|record| std::cmp::Reverse(record.score().map(crate::record::Score::get)));
    RecordList::new(query, line, records).with_more(more)
}

/// First prose line when it is nonempty and differs from the declaration label.
/// Record and member projections use the same source summary derivation.
#[must_use]
pub fn row_summary(row: &Row) -> Option<String> {
    let text = row.document.iter().find_map(|fragment| match fragment {
        backend_library::Fragment::Text(text) => Some(text.clone()),
        _ => None,
    })?;
    let first = text.lines().next().unwrap_or_default().trim().to_owned();
    (!first.is_empty() && first != row.label).then_some(first)
}
