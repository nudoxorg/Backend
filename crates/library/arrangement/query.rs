//! Bounded seek adapters over shared materialized indexes.

use super::{
    ArrangementPage, ChildRowKey, NamePostingKey, NamePostingTree, PackageRowKey,
    ProjectionArrangement, searchable_text, trigrams, update,
};
use crate::{PackageKey, Row, RowId, SymbolKey};
use backend_flow::MaterializedIndex;

impl ProjectionArrangement {
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
        let mut ids = self
            .packages
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

    pub(crate) fn names_page<F>(
        &self,
        text: &str,
        start: usize,
        limit: usize,
        row_for_id: F,
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
        if text.chars().count() < 3 {
            return select_short_name_matches_page(
                text,
                start,
                limit,
                self.names.as_ref(),
                row_for_id,
            );
        }
        select_name_matches_page(text, start, limit, self.name_postings.as_ref(), row_for_id)
    }

    /// Returns search matches in retained score/name order.
    pub(crate) fn search_page<F>(
        &self,
        text: &str,
        start: usize,
        limit: usize,
        row_for_id: F,
    ) -> ArrangementPage
    where
        F: Fn(RowId) -> Option<Row>,
    {
        if text.chars().count() < 3 {
            return select_short_search_matches_page(
                text,
                start,
                limit,
                self.names.as_ref(),
                row_for_id,
            );
        }
        select_search_matches_page(text, start, limit, self.name_postings.as_ref(), row_for_id)
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
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let grams = trigrams(&normalized);
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
    let mut rows = postings
        .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
        .take_while(|(key, ())| key.gram.as_slice() == gram.as_slice())
        .filter_map(|(key, ())| key.id)
        .filter_map(row_for_id)
        .filter(|row| row.label.to_lowercase().contains(&normalized))
        .collect::<Vec<_>>();
    rows.sort_unstable_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut ids = rows
        .into_iter()
        .map(|row| row.id)
        .skip(start)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    ArrangementPage { ids, has_more }
}

fn select_short_name_matches_page(
    text: &str,
    start: usize,
    limit: usize,
    names: Option<&super::NameTree>,
    row_for_id: impl Fn(RowId) -> Option<Row>,
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let mut ids = names
        .into_iter()
        .flat_map(MaterializedIndex::iter)
        .filter_map(|(key, ())| row_for_id(key.id))
        .filter(|row| row.label.to_lowercase().contains(&normalized))
        .map(|row| row.id)
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
    postings: Option<&NamePostingTree>,
    row_for_id: impl Fn(RowId) -> Option<Row>,
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let grams = trigrams(&normalized);
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
    let mut rows = postings
        .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
        .take_while(|(key, ())| key.gram.as_slice() == gram.as_slice())
        .filter_map(|(key, ())| key.id)
        .filter_map(row_for_id)
        .filter(|row| searchable_text(row).contains(&normalized))
        .collect::<Vec<_>>();
    rows.sort_unstable_by(|left, right| {
        right
            .score
            .unwrap_or_default()
            .cmp(&left.score.unwrap_or_default())
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut ids = rows
        .into_iter()
        .map(|row| row.id)
        .skip(start)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    ArrangementPage { ids, has_more }
}

fn select_short_search_matches_page(
    text: &str,
    start: usize,
    limit: usize,
    names: Option<&super::NameTree>,
    row_for_id: impl Fn(RowId) -> Option<Row>,
) -> ArrangementPage {
    let normalized = text.to_lowercase();
    let mut rows = names
        .into_iter()
        .flat_map(MaterializedIndex::iter)
        .filter_map(|(key, ())| row_for_id(key.id))
        .filter(|row| searchable_text(row).contains(&normalized))
        .collect::<Vec<_>>();
    rows.sort_unstable_by(|left, right| {
        right
            .score
            .unwrap_or_default()
            .cmp(&left.score.unwrap_or_default())
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut ids = rows
        .into_iter()
        .map(|row| row.id)
        .skip(start)
        .take(limit.saturating_add(1))
        .collect::<Vec<_>>();
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    ArrangementPage { ids, has_more }
}
