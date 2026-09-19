//! Bounded seek adapters over shared materialized indexes.

use super::{
    ArrangementPage, ChildRowKey, NamePostingKey, NamePostingTree, PackageRowKey,
    ProjectionArrangement, SearchPostingKey, SearchPostingTree, WorkCounters, search_grams,
    searchable_text, update,
};
use crate::{PackageKey, Row, RowId, SymbolKey};
use backend_flow::MaterializedIndex;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Eq, PartialEq)]
struct RankedRow {
    row: Row,
    normalized_label: String,
}

impl RankedRow {
    fn new(row: Row) -> Self {
        let normalized_label = row.label.to_lowercase();
        Self {
            row,
            normalized_label,
        }
    }
}

#[derive(Eq, PartialEq)]
struct NameRanked(RankedRow);

impl Ord for NameRanked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .normalized_label
            .cmp(&other.0.normalized_label)
            .then_with(|| self.0.row.id.cmp(&other.0.row.id))
    }
}

impl PartialOrd for NameRanked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Eq, PartialEq)]
struct SearchRanked(RankedRow);

impl Ord for SearchRanked {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .row
            .score
            .unwrap_or_default()
            .cmp(&self.0.row.score.unwrap_or_default())
            .then_with(|| self.0.normalized_label.cmp(&other.0.normalized_label))
            .then_with(|| self.0.row.id.cmp(&other.0.row.id))
    }
}

impl PartialOrd for SearchRanked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn retained_page_capacity(start: usize, limit: usize) -> usize {
    start.saturating_add(limit).saturating_add(1)
}

fn retain_best_names(rows: impl Iterator<Item = RankedRow>, capacity: usize) -> Vec<RankedRow> {
    let mut retained = BinaryHeap::with_capacity(capacity.min(usize::from(crate::QueryLimit::MAX)));
    for row in rows {
        retained.push(NameRanked(row));
        if retained.len() > capacity {
            retained.pop();
        }
    }
    retained.into_iter().map(|row| row.0).collect()
}

fn retain_best_search(rows: impl Iterator<Item = RankedRow>, capacity: usize) -> Vec<RankedRow> {
    let mut retained = BinaryHeap::with_capacity(capacity.min(usize::from(crate::QueryLimit::MAX)));
    for row in rows {
        retained.push(SearchRanked(row));
        if retained.len() > capacity {
            retained.pop();
        }
    }
    retained.into_iter().map(|row| row.0).collect()
}

fn sort_names(rows: &mut [RankedRow]) {
    rows.sort_unstable_by(|left, right| {
        left.normalized_label
            .cmp(&right.normalized_label)
            .then_with(|| left.row.id.cmp(&right.row.id))
    });
}

fn sort_search(rows: &mut [RankedRow]) {
    rows.sort_unstable_by(|left, right| {
        right
            .row
            .score
            .unwrap_or_default()
            .cmp(&left.row.score.unwrap_or_default())
            .then_with(|| left.normalized_label.cmp(&right.normalized_label))
            .then_with(|| left.row.id.cmp(&right.row.id))
    });
}

impl ProjectionArrangement {
    #[cfg(test)]
    pub(crate) fn name_posting_candidates(&self, text: &str) -> usize {
        let grams = search_grams(text);
        let Some(gram) = grams.iter().next_back() else {
            return 0;
        };
        let Some(postings) = self.name_postings.as_ref() else {
            return 0;
        };
        let lower = NamePostingKey {
            gram: gram.clone(),
            id: None,
        };
        postings
            .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
            .take_while(|(key, ())| key.gram.as_slice() == gram.as_slice())
            .count()
    }

    pub(crate) fn document(&self, symbol: SymbolKey) -> bool {
        update::required(self.documents.as_ref())
            .ok()
            .is_some_and(|tree| tree.get(&RowId::Symbol(symbol)).is_some())
    }

    pub(crate) fn has_package(&self, package: PackageKey) -> bool {
        self.packages
            .as_ref()
            .is_some_and(|tree| tree.get(&RowId::Package(package)).is_some())
    }

    pub(crate) fn packages_page(&self, limit: usize) -> ArrangementPage {
        self.packages_page_from(0, limit)
    }

    pub(crate) fn packages_page_from(&self, start: usize, limit: usize) -> ArrangementPage {
        let mut ids = self
            .packages
            .as_ref()
            .map(|tree| {
                tree.iter()
                    .map(|(id, ())| *id)
                    .skip(start)
                    .take(limit.saturating_add(1))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let has_more = ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    pub(crate) fn names_page<F>(
        &self,
        text: &str,
        start: usize,
        limit: usize,
        row_for_id: F,
        work: &WorkCounters,
    ) -> ArrangementPage
    where
        F: Fn(RowId) -> Option<Row>,
    {
        if text.is_empty() {
            let mut ids = self
                .names
                .as_ref()
                .map(|tree| {
                    tree.iter()
                        .map(|(key, ())| key.id)
                        .skip(start)
                        .take(limit.saturating_add(1))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let has_more = ids.len() > limit;
            ids.truncate(limit);
            return ArrangementPage { ids, has_more };
        }
        select_name_matches_page(
            text,
            start,
            limit,
            self.name_postings.as_ref(),
            row_for_id,
            work,
        )
    }

    /// Returns search matches in retained score/name order.
    pub(crate) fn search_page<F>(
        &self,
        text: &str,
        start: usize,
        limit: usize,
        row_for_id: F,
        work: &WorkCounters,
    ) -> ArrangementPage
    where
        F: Fn(RowId) -> Option<Row>,
    {
        select_search_matches_page(
            text,
            start,
            limit,
            self.search_postings.as_ref(),
            row_for_id,
            work,
        )
    }

    pub(crate) fn unscoped_symbols_page(&self, limit: usize) -> ArrangementPage {
        let mut ids = self
            .unscoped_symbols
            .as_ref()
            .map(|tree| {
                tree.iter()
                    .map(|(id, ())| *id)
                    .take(limit.saturating_add(1))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let has_more = ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    pub(crate) fn package_symbols_page(
        &self,
        package: PackageKey,
        limit: usize,
    ) -> ArrangementPage {
        self.package_symbols_page_from(package, 0, limit)
    }

    pub(crate) fn package_symbols_page_from(
        &self,
        package: PackageKey,
        start: usize,
        limit: usize,
    ) -> ArrangementPage {
        let Some(tree) = self.package_symbols.as_ref() else {
            return ArrangementPage {
                ids: Vec::new(),
                has_more: false,
            };
        };
        let lower = PackageRowKey { package, id: None };
        let mut ids = tree
            .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
            .take_while(|(key, ())| key.package == package)
            .filter_map(|(key, ())| key.id)
            .skip(start)
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let has_more = ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    /// Returns at most `limit` children from one package/parent range and
    /// reports whether another child follows. The range seek starts at the
    /// composite membership key, so unrelated packages and parents are never
    /// visited.
    pub(crate) fn children_page(
        &self,
        package: PackageKey,
        parent: Option<SymbolKey>,
        limit: usize,
    ) -> ArrangementPage {
        self.children_page_from(package, parent, 0, limit)
    }

    pub(crate) fn children_page_from(
        &self,
        package: PackageKey,
        parent: Option<SymbolKey>,
        start: usize,
        limit: usize,
    ) -> ArrangementPage {
        let Some(tree) = self.children.as_ref() else {
            return ArrangementPage {
                ids: Vec::new(),
                has_more: false,
            };
        };
        let lower = ChildRowKey {
            package,
            parent,
            id: None,
        };
        let mut ids = tree
            .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
            .take_while(|(key, ())| key.package == package && key.parent == parent)
            .filter_map(|(key, ())| key.id)
            .skip(start)
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let has_more = ids.len() > limit;
        ids.truncate(limit);
        ArrangementPage { ids, has_more }
    }

    pub(crate) fn package_count(&self) -> usize {
        self.packages.as_ref().map_or(0, MaterializedIndex::len)
    }
}
fn select_name_matches_page(
    text: &str,
    start: usize,
    limit: usize,
    postings: Option<&NamePostingTree>,
    row_for_id: impl Fn(RowId) -> Option<Row>,
    work: &WorkCounters,
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let grams = search_grams(&normalized);
    let Some(postings) = postings else {
        return ArrangementPage {
            ids: Vec::new(),
            has_more: false,
        };
    };
    let Some(gram) = grams.iter().next_back() else {
        return ArrangementPage {
            ids: Vec::new(),
            has_more: false,
        };
    };
    let lower = NamePostingKey {
        gram: gram.clone(),
        id: None,
    };
    let capacity = retained_page_capacity(start, limit);
    let mut rows = retain_best_names(
        postings
            .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
            .take_while(|(key, ())| key.gram.as_slice() == gram.as_slice())
            .filter_map(|(key, ())| key.id)
            .filter_map(row_for_id)
            .map(RankedRow::new)
            .filter(|row| row.normalized_label.contains(&normalized)),
        capacity,
    );
    work.record_sort(rows.len());
    sort_names(&mut rows);
    let mut ids = rows
        .into_iter()
        .map(|row| row.row.id)
        .skip(start)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    ArrangementPage { ids, has_more }
}

fn select_search_matches_page(
    text: &str,
    start: usize,
    limit: usize,
    postings: Option<&SearchPostingTree>,
    row_for_id: impl Fn(RowId) -> Option<Row>,
    work: &WorkCounters,
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let grams = search_grams(&normalized);
    let Some(postings) = postings else {
        return ArrangementPage {
            ids: Vec::new(),
            has_more: false,
        };
    };
    let Some(gram) = grams.iter().next_back() else {
        return ArrangementPage {
            ids: Vec::new(),
            has_more: false,
        };
    };
    let lower = SearchPostingKey {
        gram: gram.clone(),
        id: None,
    };
    let capacity = retained_page_capacity(start, limit);
    let mut rows = retain_best_search(
        postings
            .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
            .take_while(|(key, ())| key.gram.as_slice() == gram.as_slice())
            .filter_map(|(key, ())| key.id)
            .filter_map(row_for_id)
            .filter(|row| searchable_text(row).contains(&normalized))
            .map(RankedRow::new),
        capacity,
    );
    work.record_sort(rows.len());
    sort_search(&mut rows);
    let mut ids = rows
        .into_iter()
        .map(|row| row.row.id)
        .skip(start)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    ArrangementPage { ids, has_more }
}
