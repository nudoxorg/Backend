//! Bounded order witness for outline pages; never a public relevance score.
use crate::{PackageKey, QueryLimit, Row, RowId, SymbolKey, ViewSnapshot};
use std::collections::{BTreeMap, BTreeSet};

/// Exact preorder of one bounded native outline page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlinePageOrder {
    start: u64,
    symbols: Box<[SymbolKey]>,
}

impl OutlinePageOrder {
    /// Admits a bounded, duplicate-free identity sequence. Row membership and
    /// topology are checked separately against the admitted snapshot.
    /// # Errors
    /// Refuses oversized, duplicate or overflowing identity sequences.
    pub fn new(start: u64, symbols: impl Into<Box<[SymbolKey]>>) -> Result<Self, String> {
        let symbols = symbols.into();
        if symbols.len() > usize::from(QueryLimit::MAX)
            || symbols.iter().copied().collect::<BTreeSet<_>>().len() != symbols.len()
            || start.checked_add(symbols.len() as u64).is_none()
        {
            return Err("outline order exceeds its identity/count bound".to_owned());
        }
        Ok(Self { start, symbols })
    }

    /// Absolute start in the producer's immutable package preorder.
    #[must_use]
    pub const fn start(&self) -> u64 { self.start }

    /// Exact order; every returned symbol occurs once.
    #[must_use]
    pub fn symbols(&self) -> &[SymbolKey] { &self.symbols }

    pub(crate) fn digest(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        digest.update(b"outline-page-order-v1\0");
        digest.update(&(self.symbols.len() as u64).to_be_bytes());
        for id in &self.symbols { digest.update(id.as_bytes()); }
        *digest.finalize().as_bytes()
    }

    /// Checks exact membership and page-local containment against certified
    /// row facts. Out-of-page parents are promoted; a retained cycle may cut
    /// its closing edge once, but a closed subtree cannot be reopened.
    /// # Errors
    /// Refuses foreign/missing rows, changed package or inconsistent topology.
    pub fn validate(&self, snapshot: &ViewSnapshot, package: PackageKey, limit: QueryLimit) -> Result<(), String> {
        let rows = snapshot.root.rows();
        if self.symbols.len() != rows.len() || rows.len() > usize::from(limit.get()) {
            return Err("outline order does not cover its bounded row set".to_owned());
        }
        let mut by_id = BTreeMap::<SymbolKey, &Row>::new();
        for row in rows {
            let RowId::Symbol(id) = row.id else { return Err("outline order contains a non-symbol row".to_owned()); };
            if row.package.is_some_and(|owner| owner != package) { return Err("outline order crosses its package".to_owned()); }
            by_id.insert(id, row);
        }
        if self.symbols.iter().any(|id| !by_id.contains_key(id)) {
            return Err("outline order contains a foreign symbol".to_owned());
        }
        let mut visited = BTreeSet::new();
        let mut ancestors = Vec::new();
        for id in &self.symbols {
            let row = by_id[id];
            if let Some(parent) = row.parent.filter(|parent| by_id.contains_key(parent)) {
                if let Some(position) = ancestors.iter().position(|ancestor| *ancestor == parent) {
                    ancestors.truncate(position + 1);
                } else {
                    // A future parent can only close a retained cycle. Walk
                    // at most this page's row count, never an untrusted depth.
                    let mut current = Some(parent);
                    let mut closes_cycle = false;
                    for _ in 0..rows.len() {
                        let Some(next) = current else { break; };
                        if next == *id { closes_cycle = true; break; }
                        current = by_id.get(&next).and_then(|row| row.parent);
                    }
                    if visited.contains(&parent) || !closes_cycle {
                        return Err("outline order contradicts retained containment".to_owned());
                    }
                    ancestors.clear();
                }
            } else { ancestors.clear(); }
            visited.insert(*id);
            ancestors.push(*id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Basis, Coverage, Cursor, DeclarationKind, Frontier, Lane, Library, PageContinuation,
        PageRequest, QueryPageRecipe, Reason, ViewRoot, object_version, package_key, symbol_key,
        view_key, view_state_root};

    #[test]
    fn outline_order_refuses_membership_topology_and_rehashed_wrong_predecessors() {
        let package = package_key("/order");
        let basis = Basis::new(view_state_root(&[]), object_version(b"order"));
        let mut ids = (0..4).map(|n| symbol_key(&format!("ordered-{n}"))).collect::<Vec<_>>();
        ids.sort_unstable();
        let rows = vec![
            Row::new(RowId::Package(package), basis, "/order"),
            Row::in_package(RowId::Symbol(ids[0]), basis, package, "/order::Parent").with_kind(DeclarationKind::Class),
            Row::in_package(RowId::Symbol(ids[1]), basis, package, "/order::First").with_parent(ids[0]),
            Row::in_package(RowId::Symbol(ids[2]), basis, package, "/order::Second").with_parent(ids[0]),
            Row::in_package(RowId::Symbol(ids[3]), basis, package, "/order::Other").with_kind(DeclarationKind::Class),
        ];
        let root = ViewRoot::new_incomplete(view_key(b"order"), basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0), rows,
            vec![Coverage::Unavailable { lane: Lane::Exact, reason: Reason::NoIndex }]).expect("source");
        let cursor = Cursor::for_view_root(&root);
        let library = Library::from_view(root, cursor).expect("catalog");
        let credit = QueryLimit::new(200).expect("credit");
        let full = library.outline_page(package, PageRequest::new(library.revision_root(), credit)).expect("full page");
        full.outline_order.as_ref().expect("order").validate(&full.snapshot, package, credit).expect("native topology");
        assert!(OutlinePageOrder::new(0, vec![ids[0], ids[0]]).is_err());
        assert!(OutlinePageOrder::new(0, vec![ids[0]; 201]).is_err());
        assert!(OutlinePageOrder::new(0, vec![ids[0]]).expect("bounded").validate(&full.snapshot, package, credit).is_err());
        assert!(OutlinePageOrder::new(0, vec![ids[0], ids[1], ids[2], symbol_key("foreign")]).expect("bounded").validate(&full.snapshot, package, credit).is_err());
        assert!(OutlinePageOrder::new(0, vec![ids[0], ids[3], ids[1], ids[2]]).expect("permutation").validate(&full.snapshot, package, credit).is_err(),
            "a closed parent's subtree cannot reopen");
        let credit = QueryLimit::new(3).expect("small credit");
        let request = PageRequest::new(library.revision_root(), credit);
        let first = library.outline_page(package, request).expect("first page");
        let mut swapped = first.outline_order.as_ref().expect("order").symbols().to_vec();
        swapped.swap(1, 2); // A valid sibling permutation, but not this producer's order.
        let swapped = OutlinePageOrder::new(0, swapped).expect("bounded sibling order");
        swapped.validate(&first.snapshot, package, credit).expect("valid topology");
        let recipe = QueryPageRecipe::outline(library.revision_root(), package, credit, &swapped);
        let forged = ViewRoot::new_incomplete(recipe.identity(), first.snapshot.root.basis(),
            first.snapshot.root.frontier(), first.snapshot.root.rows().to_vec(),
            first.snapshot.root.coverage().to_vec()).expect("same rows, different order commitment");
        let forged = Cursor::for_view_root(&forged).with_query_offset(3);
        assert!(matches!(library.outline_page(package, request.with_continuation(PageContinuation::from_cursor(forged))),
            Err(crate::LibraryError::CursorMismatch)), "rehashing the same rows with a foreign predecessor order is refused");
    }
    #[test]
    fn outline_windows_preserve_legacy_recipe_and_authenticate_actual_predecessor_spans() {
        let package = package_key("/window-order");
        let basis = Basis::new(view_state_root(&[]), object_version(b"window-order"));
        let mut rows = (0..11).map(|n| Row::in_package(
            RowId::Symbol(symbol_key(&format!("window-{n}"))), basis, package,
            format!("/window-order::Item{n:02}"),
        )).collect::<Vec<_>>();
        rows.push(Row::new(RowId::Package(package), basis, "/window-order"));
        let root = ViewRoot::new_incomplete(view_key(b"window-order"), basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0), rows,
            vec![Coverage::Unavailable { lane: Lane::Exact, reason: Reason::NoIndex }]).expect("catalog");
        let library = Library::from_view(root.clone(), Cursor::for_view_root(&root)).expect("library");
        let credit = QueryLimit::new(4).expect("issued credit");
        let request = PageRequest::new(library.revision_root(), credit);
        let legacy = library.outline_page(package, request).expect("legacy page");
        let order = legacy.outline_order.as_ref().expect("order");
        // Independent historical preimage, not a comparison of two new wrappers.
        let mut recipe = b"outline-page-v4\0".to_vec();
        recipe.extend_from_slice(package.as_bytes());
        recipe.extend_from_slice(&credit.get().to_be_bytes());
        recipe.extend_from_slice(&0_u64.to_be_bytes());
        let historical = crate::canonical::view_identity_preimage(&[
            b"query", &recipe, library.revision_root().as_bytes(), &order.digest(),
        ]);
        assert_eq!(QueryPageRecipe::outline(library.revision_root(), package, credit, order).canonical_preimage(), historical.as_slice());
        assert_eq!(legacy.snapshot.root.recipe(), crate::ViewRecipeId::from_value(&historical));
        let legacy_cursor = legacy.snapshot.next.expect("legacy successor");
        assert!(legacy_cursor.query_position().predecessor().is_none());
        let narrow = crate::QueryWindow::new(credit, QueryLimit::new(1).expect("one")).expect("window");
        let request = request.with_window(narrow).expect("unchanged credit")
            .with_continuation(PageContinuation::from_cursor(legacy_cursor));
        let second = library.outline_page(package, request).expect("legacy predecessor, narrow successor");
        assert_eq!(second.outline_order.as_ref().expect("order").start(), 4);
        assert_eq!(second.snapshot.root.row_count(), 1);
        let next = second.snapshot.next.expect("next");
        assert_eq!(next.query_offset(), 5);
        assert_eq!(next.query_position().predecessor(), Some(narrow));
        assert!(library.outline_page(package, PageRequest::new(library.revision_root(), credit)
            .with_continuation(PageContinuation::from_cursor(next))).is_err(), "windowed cursor cannot downgrade to fixed");
        let mut seen = order.symbols().iter().copied().chain(second.outline_order.as_ref().expect("order").symbols().iter().copied())
            .collect::<BTreeSet<_>>();
        let mut cursor = next;
        for delivery in [2, 4] {
            let window = crate::QueryWindow::new(credit, QueryLimit::new(delivery).expect("positive")).expect("window");
            let request = PageRequest::new(library.revision_root(), credit).with_window(window).expect("credit")
                .with_continuation(PageContinuation::from_cursor(cursor));
            let page = library.outline_page(package, request).expect("exact prior span, new delivery");
            assert_eq!(page.outline_order.as_ref().expect("order").start(), cursor.query_offset());
            for id in page.outline_order.as_ref().expect("order").symbols() { assert!(seen.insert(*id)); }
            if let Some(next) = page.snapshot.next { cursor = next; }
            else { assert_eq!(delivery, 4); }
        }
        assert_eq!(seen.len(), 11);
        let wrong_credit = QueryLimit::new(3).expect("wrong credit");
        let wrong = PageRequest::new(library.revision_root(), wrong_credit)
            .with_window(crate::QueryWindow::new(wrong_credit, QueryLimit::new(1).expect("one")).expect("window")).expect("request")
            .with_continuation(PageContinuation::from_cursor(next));
        assert!(library.outline_page(package, wrong).is_err(), "issued ceiling is immutable");
        let forged_span = crate::QueryWindow::new(credit, QueryLimit::new(2).expect("wrong span")).expect("window");
        let forged = next.with_query_position(crate::QueryPosition::windowed(next.query_offset(), forged_span).expect("legal shape"));
        assert!(library.outline_page(package, request.with_continuation(PageContinuation::from_cursor(forged))).is_err(),
            "legal span metadata cannot replace the actual predecessor rows/order");
    }

}
