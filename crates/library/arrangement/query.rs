//! Bounded seek adapters over shared materialized indexes.

use super::{
    ArrangementPage, ChildRowKey, NamePostingKey, NamePostingTree, PackageRowKey,
    ProjectionArrangement, SearchPostingKey, SearchPostingTree, WorkCounters, search_grams,
    searchable_text, update,
};
use crate::command::{AddressSelection, NameSelection};
use crate::{PackageKey, Row, RowId, SymbolKey};
use backend_flow::MaterializedIndex;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

struct NameRanked {
    id: RowId,
    normalized_label: String,
    placement: (bool, bool, bool, bool, bool, u8),
}

impl NameRanked {
    fn new(row: &Row, text: Option<&str>, qualified: Option<bool>) -> Option<Self> {
        let normalized_label = row.label.to_lowercase();
        if qualified.is_none() && text.is_some_and(|text| !normalized_label.contains(text)) {
            return None;
        }
        // Coordinates carry an opaque identity before the declaration name.
        // Compare a complete name segment (including relative qualification),
        // never a suffix within a name such as `MockEnvironment`.
        let exact = qualified == Some(true)
            || text.is_some_and(|text| {
                normalized_label == text
                    || normalized_label
                        .strip_suffix(text)
                        .is_some_and(|prefix| prefix.ends_with("::"))
            });
        // Among exact names a declaration comes before an import that only
        // brings it in, library code before examples, tests and benchmarks, a
        // runtime declaration before the type stub (.pyi) that declares it, and
        // a declaration with a captured site before one without, then by how
        // much its kind is the thing the name names.
        let path = row.source.file_path();
        let runtime = path.is_none_or(|path| !path.ends_with(".pyi"));
        Some(Self {
            id: row.id,
            normalized_label,
            placement: (
                exact,
                row.kind != Some(crate::DeclarationKind::Import),
                path.is_some_and(|path| !super::is_support_source_path(path)),
                runtime,
                row.source.captured().is_some(),
                row.kind.map_or(1, super::declaration_name_priority),
            ),
        })
    }

    fn ordering_key(&self) -> (Reverse<(bool, bool, bool, bool, bool, u8)>, &str, RowId) {
        (Reverse(self.placement), &self.normalized_label, self.id)
    }
}

impl PartialEq for NameRanked {
    fn eq(&self, other: &Self) -> bool {
        self.ordering_key() == other.ordering_key()
    }
}

impl Eq for NameRanked {}

impl Ord for NameRanked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_key().cmp(&other.ordering_key())
    }
}

impl PartialOrd for NameRanked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct SearchRanked {
    id: RowId,
    normalized_label: String,
    score: u32,
}

impl SearchRanked {
    fn new(row: &Row) -> Self {
        Self {
            id: row.id,
            normalized_label: row.label.to_lowercase(),
            score: row.score.unwrap_or_default(),
        }
    }

    fn ordering_key(&self) -> (Reverse<u32>, &str, RowId) {
        (Reverse(self.score), &self.normalized_label, self.id)
    }
}

impl PartialEq for SearchRanked {
    fn eq(&self, other: &Self) -> bool {
        self.ordering_key() == other.ordering_key()
    }
}

impl Eq for SearchRanked {}

impl Ord for SearchRanked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_key().cmp(&other.ordering_key())
    }
}

impl PartialOrd for SearchRanked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Only producer-retained sites influence this presentation order. Test/support
// paths are a navigation hint, never a declaration or language authority.
fn outline_order(row: &Row) -> (Reverse<(bool, bool, u8)>, &str, RowId) {
    let path = row.source.file_path();
    let support = path.is_some_and(super::is_support_source_path);
    (
        Reverse((
            path.is_some() && !support,
            path.is_some(),
            row.kind.map_or(1, super::declaration_name_priority),
        )),
        path.unwrap_or(""),
        row.id,
    )
}

// Retain the borrowed key once: heap comparisons never rescan a source path.
#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct OutlineRanked<'a>((Reverse<(bool, bool, u8)>, &'a str, RowId));

fn retained_page_capacity(start: usize, limit: usize) -> usize {
    start.saturating_add(limit).saturating_add(1)
}

fn retain_best_names(rows: impl Iterator<Item = NameRanked>, capacity: usize) -> Vec<NameRanked> {
    let mut retained = BinaryHeap::with_capacity(capacity.min(usize::from(crate::QueryLimit::MAX)));
    for row in rows {
        retained.push(row);
        if retained.len() > capacity {
            retained.pop();
        }
    }
    retained.into_iter().collect()
}

fn retain_best_search(
    rows: impl Iterator<Item = SearchRanked>,
    capacity: usize,
) -> Vec<SearchRanked> {
    let mut retained = BinaryHeap::with_capacity(capacity.min(usize::from(crate::QueryLimit::MAX)));
    for row in rows {
        retained.push(row);
        if retained.len() > capacity {
            retained.pop();
        }
    }
    retained.into_iter().collect()
}

fn sort_names(rows: &mut [NameRanked]) {
    rows.sort_unstable();
}

fn sort_search(rows: &mut [SearchRanked]) {
    rows.sort_unstable();
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

    pub(crate) fn names_page<'view, F, A>(
        &self,
        selection: NameSelection<'_>,
        start: usize,
        limit: usize,
        row_for_id: F,
        address_matches: A,
        relative_matches: impl Fn(RowId, &str) -> Option<bool>,
        work: &WorkCounters,
    ) -> ArrangementPage
    where
        F: Fn(RowId) -> Option<&'view Row>,
        A: Fn(RowId, AddressSelection<'_>) -> bool,
    {
        let text = selection.posting_text();
        if text.is_empty() {
            if !selection.is_global() {
                return ArrangementPage {
                    ids: Vec::new(),
                    has_more: false,
                };
            }
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
            selection,
            start,
            limit,
            self.name_postings.as_ref(),
            row_for_id,
            address_matches,
            relative_matches,
            work,
        )
    }

    /// Returns search matches in retained score/name order.
    pub(crate) fn search_page<'view, F>(
        &self,
        text: &str,
        start: usize,
        limit: usize,
        row_for_id: F,
        work: &WorkCounters,
    ) -> ArrangementPage
    where
        F: Fn(RowId) -> Option<&'view Row>,
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

    /// One shared bounded order for outline trees and their row projection.
    pub(crate) fn outline_page<'view>(
        &self,
        package: PackageKey,
        start: usize,
        limit: usize,
        row_for_id: impl Fn(RowId) -> Option<&'view Row>,
        work: &WorkCounters,
    ) -> ArrangementPage {
        let capacity = retained_page_capacity(start, limit);
        let mut retained =
            BinaryHeap::with_capacity(capacity.min(usize::from(crate::QueryLimit::MAX)));
        let mut visited = 0_usize;
        let mut visit = |id| {
            visited = visited.saturating_add(1);
            if let Some(row) = row_for_id(id) {
                retained.push(OutlineRanked(outline_order(row)));
                if retained.len() > capacity {
                    retained.pop();
                }
            }
        };
        let mut scoped = false;
        if let Some(tree) = self.package_symbols.as_ref() {
            let lower = PackageRowKey { package, id: None };
            for (key, ()) in tree
                .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
                .take_while(|(key, ())| key.package == package)
            {
                if let Some(id) = key.id {
                    scoped = true;
                    visit(id);
                }
            }
        }
        // Preserve the existing unambiguous legacy single-package relation.
        if !scoped && self.package_count() == 1 {
            if let Some(tree) = self.unscoped_symbols.as_ref() {
                for (id, ()) in tree.iter() {
                    visit(*id);
                }
            }
        }
        work.record_scan(visited);
        let mut retained = retained.into_vec();
        work.record_sort(retained.len());
        retained.sort_unstable();
        let has_more = retained.len() > start.saturating_add(limit);
        let ids = retained
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|row| row.0.2)
            .collect();
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
fn select_name_matches_page<'view>(
    selection: NameSelection<'_>,
    start: usize,
    limit: usize,
    postings: Option<&NamePostingTree>,
    row_for_id: impl Fn(RowId) -> Option<&'view Row>,
    address_matches: impl Fn(RowId, AddressSelection<'_>) -> bool,
    relative_matches: impl Fn(RowId, &str) -> Option<bool>,
    work: &WorkCounters,
) -> ArrangementPage {
    let normalized = selection.posting_text().to_lowercase();
    let name_text = match selection {
        NameSelection::Name { text, .. } => Some(text.to_lowercase()),
        NameSelection::Address(_) => None,
    };
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
            .filter(|&id| match selection {
                NameSelection::Name { .. } => true,
                NameSelection::Address(address) => address_matches(id, address),
            })
            .filter_map(row_for_id)
            .filter_map(|row| {
                let qualified = match selection {
                    NameSelection::Name { text, .. } => relative_matches(row.id, text),
                    NameSelection::Address(_) => None,
                };
                NameRanked::new(row, name_text.as_deref(), qualified)
            }),
        capacity,
    );
    work.record_sort(rows.len());
    sort_names(&mut rows);
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

fn select_search_matches_page<'view>(
    text: &str,
    start: usize,
    limit: usize,
    postings: Option<&SearchPostingTree>,
    row_for_id: impl Fn(RowId) -> Option<&'view Row>,
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
            .map(SearchRanked::new),
        capacity,
    );
    work.record_sort(rows.len());
    sort_search(&mut rows);
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

#[cfg(test)]
mod ordering_laws {
    use super::*;
    use crate::{
        Basis, DeclarationKind, Fragment, SourceLocation, object_version, symbol_key,
        view_state_root,
    };

    fn fixture(label: &str, kind: DeclarationKind, sourced: bool) -> Row {
        let basis = Basis::new(view_state_root(&[]), object_version(b"rank-law-fixture"));
        let row = Row::new(
            RowId::Symbol(symbol_key("same admitted identity")),
            basis,
            label,
        )
        .with_kind(kind);
        if sourced {
            row.with_source(SourceLocation::new("first.py", 7).expect("fixture source"))
        } else {
            row
        }
    }

    #[test]
    fn name_equality_and_order_ignore_payload_and_scores_but_keep_placement_tiers() {
        let original = fixture("scope::Environment", DeclarationKind::Class, true);
        let mut changed = original
            .clone()
            .with_source(
                SourceLocation::new("different.py", 90).expect("other captured fixture site"),
            )
            .with_signature("captured declaration signature")
            .with_document(vec![Fragment::Text(
                "different captured documentation".to_owned(),
            )]);
        changed.score = Some(987);
        assert_ne!(original, changed);
        let original =
            NameRanked::new(&original, Some("environment"), None).expect("name candidate");
        let changed = NameRanked::new(&changed, Some("environment"), None).expect("name candidate");
        assert!(original == changed);
        assert_eq!(original.cmp(&changed), Ordering::Equal);

        // A declaration of the exact name, sourced or not, outranks an import
        // that only brings the name in, however well sited the import is.
        let tiers = [
            fixture("z::Environment", DeclarationKind::Class, true),
            fixture("c::Environment", DeclarationKind::Field, true),
            fixture("a::Environment", DeclarationKind::Type, false),
            fixture("b::Environment", DeclarationKind::Import, true),
            fixture("a::MockEnvironment", DeclarationKind::Class, true),
        ]
        .into_iter()
        .map(|row| NameRanked::new(&row, Some("environment"), None).expect("name candidate"))
        .collect::<Vec<_>>();
        for (at, left) in tiers.iter().enumerate() {
            for (other_at, right) in tiers.iter().enumerate() {
                assert_eq!(left == right, left.cmp(right) == Ordering::Equal);
                assert_eq!(left.cmp(right), at.cmp(&other_at));
            }
        }
    }

    #[test]
    fn a_runtime_declaration_ranks_before_the_type_stub_that_declares_it() {
        let stub = fixture(
            "pkg::semantic::01::Environment",
            DeclarationKind::Class,
            false,
        )
        .with_source(SourceLocation::new("pkg/_attrs.pyi", 6).expect("stub site"));
        let runtime = fixture(
            "pkg::semantic::ff::Environment",
            DeclarationKind::Class,
            false,
        )
        .with_source(SourceLocation::new("pkg/_attrs.py", 20).expect("runtime site"));
        let mut ranked = [&stub, &runtime]
            .map(|row| NameRanked::new(row, Some("environment"), None).expect("name candidate"));
        // The stub's coordinate sorts first; only the stub tier moves it.
        assert!(stub.label < runtime.label);
        ranked.sort();
        assert_eq!(ranked[0].normalized_label, runtime.label.to_lowercase());
    }

    #[test]
    fn search_equality_and_order_ignore_payload_but_observe_relevance_score() {
        let original = fixture("scope::Environment", DeclarationKind::Class, false);
        let changed = original
            .clone()
            .with_source(SourceLocation::new("different.py", 90).expect("captured fixture site"))
            .with_signature("different signature")
            .with_document(vec![Fragment::Text("different documentation".to_owned())]);
        assert_ne!(original, changed);
        let mut zero = changed.clone();
        zero.score = Some(0);
        let mut higher = changed.clone();
        higher.score = Some(1);
        let rows = [original, changed, zero, higher]
            .into_iter()
            .map(|row| SearchRanked::new(&row))
            .collect::<Vec<_>>();
        for left in &rows {
            for right in &rows {
                assert_eq!(left == right, left.cmp(right) == Ordering::Equal);
            }
        }
        assert!(rows[0] == rows[1] && rows[1] == rows[2]);
        assert_eq!(rows[3].cmp(&rows[0]), Ordering::Less);
        assert!(rows[3] != rows[0]);
    }
}

#[cfg(test)]
mod outline_selection_profile {
    use super::*;

    #[test]
    #[ignore = "broker-only bounded outline rank versus original canonical seek experiment"]
    fn profile_outline_rank_and_original_canonical_prefix() {
        for rows in [300_usize, 10_000, 100_000] {
            let source = crate::view_state_root(&[]);
            let basis = crate::Basis::new(source, crate::object_version(b"outline-profile"));
            let package = crate::package_key("/outline-profile");
            let mut values = Vec::with_capacity(rows + 1);
            values.push(Row::new(RowId::Package(package), basis, "/outline-profile"));
            for i in 0..rows {
                let path = if i % 5 == 0 {
                    "src/app.spec.ts"
                } else {
                    "src/app.ts"
                };
                values.push(
                    Row::in_package(
                        RowId::Symbol(crate::symbol_key(&format!("profile::{i}"))),
                        basis,
                        package,
                        format!("/outline-profile::semantic::{i}::N{i}"),
                    )
                    .with_kind(crate::DeclarationKind::Class)
                    .with_source(
                        crate::SourceLocation::new(path, 1).expect("retained fixture site"),
                    ),
                );
            }
            // Index construction and canonical proof preparation are outside
            // measured selection. This partial fixture claims no native coverage.
            let view = crate::ViewRoot::new_incomplete(
                crate::view_key(b"outline-profile"),
                basis,
                crate::Frontier::new(basis.branch, basis.log, basis.schema, source, 0),
                values,
                vec![crate::Coverage::Unavailable {
                    lane: crate::Lane::Names,
                    reason: crate::Reason::Incomplete,
                }],
            )
            .expect("canonical fixture");
            let work = WorkCounters::default();
            let arrangement = ProjectionArrangement::build(&view, &work).expect("retained index");
            for observation in 0..3 {
                let tree = arrangement.package_symbols.as_ref().expect("package index");
                let lower = PackageRowKey { package, id: None };
                let started = std::time::Instant::now();
                let mut old_visits = 0_usize;
                let old = tree
                    .range((std::ops::Bound::Included(lower), std::ops::Bound::Unbounded))
                    .take_while(|(key, ())| key.package == package)
                    .filter_map(|(key, ())| key.id)
                    .inspect(|_| old_visits += 1)
                    .take(201)
                    .collect::<Vec<_>>();
                let old_elapsed = started.elapsed();
                let before = work.snapshot();
                let started = std::time::Instant::now();
                let ranked =
                    arrangement.outline_page(package, 0, 200, |id| view.row_ref(id), &work);
                let ranked_elapsed = started.elapsed();
                let after = work.snapshot();
                assert_eq!(old.len(), 201);
                assert_eq!(old_visits, 201);
                assert_eq!(ranked.ids.len(), 200);
                assert!(ranked.has_more);
                assert_eq!(after.scan_rows - before.scan_rows, rows as u64);
                eprintln!(
                    "outline_selection_profile {}",
                    serde_json::json!({
                        "rows": rows, "observation": observation, "limit": 200,
                        "canonical_visits": old_visits, "canonical_ns": old_elapsed.as_nanos(),
                        "ranked_visits": after.scan_rows - before.scan_rows,
                        "retained_rank_keys": after.sort_rows - before.sort_rows,
                        "ranked_ns": ranked_elapsed.as_nanos()
                    })
                );
                std::hint::black_box((old, ranked));
            }
        }
    }
}

#[cfg(test)]
mod library_before_support_ranking {
    use super::*;
    use crate::{
        Basis, DeclarationKind, SourceLocation, object_version, symbol_key, view_state_root,
    };

    fn named(label: &str, kind: DeclarationKind, path: &str) -> Row {
        let basis = Basis::new(view_state_root(&[]), object_version(b"support-rank"));
        Row::new(RowId::Symbol(symbol_key(label)), basis, label)
            .with_kind(kind)
            .with_source(SourceLocation::new(path, 3).expect("fixture site"))
    }

    #[test]
    fn library_code_ranks_above_examples_tests_and_benchmarks_for_one_exact_name() {
        for support in [
            "tests",
            "examples",
            "benches",
            "src/__tests__",
            "app.spec.ts",
        ] {
            assert!(crate::is_support_source_path(support), "{support}");
        }
        for library in [
            "src/alphabet.rs",
            "lib/examples_of.rs",
            "src/testing_helpers.rs",
        ] {
            assert!(!crate::is_support_source_path(library), "{library}");
        }
        // `examples/…` sorts before `src/…` by label, so only the placement
        // tier can put the library definition first.
        let mut ranked = [
            named(
                "/p::examples/base64.rs:11::Alphabet",
                DeclarationKind::Enum,
                "examples/base64.rs",
            ),
            named(
                "/p::tests/alphabet.rs:4::Alphabet",
                DeclarationKind::Struct,
                "tests/alphabet.rs",
            ),
            named(
                "/p::src/alphabet.rs:55::Alphabet",
                DeclarationKind::Struct,
                "src/alphabet.rs",
            ),
            named(
                "/p::examples/base64.rs:6::Alphabet",
                DeclarationKind::Import,
                "examples/base64.rs",
            ),
            named(
                "/p::src/lib.rs:9::Alphabet",
                DeclarationKind::Import,
                "src/lib.rs",
            ),
        ]
        .iter()
        .map(|row| NameRanked::new(row, Some("alphabet"), None).expect("candidate"))
        .collect::<Vec<_>>();
        sort_names(&mut ranked);
        let labels = ranked
            .iter()
            .map(|row| row.normalized_label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "/p::src/alphabet.rs:55::alphabet",
                "/p::examples/base64.rs:11::alphabet",
                "/p::tests/alphabet.rs:4::alphabet",
                "/p::src/lib.rs:9::alphabet",
                "/p::examples/base64.rs:6::alphabet",
            ],
            "library declaration, then support declarations, then imports"
        );
    }
}
