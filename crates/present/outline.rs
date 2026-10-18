//! A package outline as a tree of names.
//!
//! The engine answers an outline query with a tree of [`SymbolKey`] values and
//! nothing else — the shape is there, the words are not. Rendering that tree
//! directly is what produced today's unusable output: eighteen lines of bare
//! hexadecimal. So the model requires a resolver: an outline is only built
//! once every node has been matched to a row that carries its coordinate, and
//! a node that cannot be matched says so with its tag rather than pretending
//! to be a name.

use crate::identity::{Identity, IdentityKey, KeyTag};
use backend_library::{DeclarationKind, OutlineExtent, OutlineNode, SymbolKey};

use crate::page::Truncation;

/// One node of an outline, with the name it resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineEntry {
    identity: Option<Identity>,
    tag: KeyTag,
    kind: Option<DeclarationKind>,
    children: Box<[OutlineEntry]>,
}

impl OutlineEntry {
    /// Resolves one outline node and its children through a row resolver.
    #[must_use]
    pub fn resolve(
        node: &OutlineNode,
        resolve: &mut impl FnMut(SymbolKey) -> Option<(Identity, Option<DeclarationKind>)>,
    ) -> Self {
        let resolved = resolve(node.symbol);
        let children = node
            .children
            .iter()
            .map(|child| Self::resolve(child, resolve))
            .collect();
        Self {
            tag: KeyTag::from_key(node.symbol.as_bytes()),
            identity: resolved.as_ref().map(|(identity, _)| identity.clone()),
            kind: resolved.and_then(|(_, kind)| kind),
            children,
        }
    }

    /// Returns the resolved identity, when the node matched a row.
    #[must_use]
    pub const fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// Returns the display abbreviation of this node's key.
    #[must_use]
    pub const fn tag(&self) -> KeyTag {
        self.tag
    }

    /// Returns the node's declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the child nodes in outline order.
    #[must_use]
    pub fn children(&self) -> &[OutlineEntry] {
        &self.children
    }

    /// Returns the displayed name, falling back to the key tag.
    #[must_use]
    pub fn name(&self) -> String {
        self.identity.as_ref().map_or_else(
            || format!("‹{}›", self.tag),
            |identity| identity.name().to_owned(),
        )
    }

    /// Returns the number of nodes in this subtree, including itself.
    #[must_use]
    pub fn count(&self) -> usize {
        self.children
            .iter()
            .fold(1_usize, |total, child| total.saturating_add(child.count()))
    }

    /// Returns whether every node in this subtree resolved to a name.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.identity.is_some() && self.children.iter().all(Self::is_resolved)
    }
}

/// One package outline, resolved to names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineTree {
    package: Identity,
    roots: Box<[OutlineEntry]>,
    truncation: Truncation,
    page: Option<OutlinePageState>,
}

impl OutlineTree {
    /// Records one resolved outline.
    #[must_use]
    pub fn new(
        package: Identity,
        roots: impl Into<Box<[OutlineEntry]>>,
        extent: OutlineExtent,
    ) -> Self {
        Self {
            package,
            roots: roots.into(),
            page: None,
            truncation: match extent {
                OutlineExtent::Complete => Truncation::Complete,
                OutlineExtent::Truncated => Truncation::Truncated,
            },
        }
    }

    /// Retains a complete admitted native page's scope and continuation.
    #[must_use]
    pub fn with_page(mut self, snapshot: &backend_library::ViewSnapshot, limit: u16) -> Self {
        self.page = Some(OutlinePageState {
            basis: snapshot.root.basis(),
            sequence: snapshot.root.frontier().sequence,
            limit,
            continuation: snapshot
                .next
                .map(backend_library::PageContinuation::from_cursor),
            coverage: crate::CoverageLine::new(
                snapshot.root.coverage(),
                Some(snapshot.root.rows().len() as u64),
            ),
        });
        self
    }

    /// Exact source identity retained by a paged outline, for bounded re-reads.
    #[must_use]
    pub fn page_basis(&self) -> Option<(backend_library::Basis, u64)> {
        self.page.as_ref().map(|page| (page.basis, page.sequence))
    }

    /// Owner-issued continuation; never manufactured from a rendered prefix.
    #[must_use]
    pub fn continuation(&self) -> Option<backend_library::PageContinuation> {
        self.page.as_ref().and_then(|page| page.continuation)
    }

    pub(crate) fn page(&self) -> Option<&OutlinePageState> {
        self.page.as_ref()
    }

    /// Returns the package this outline describes.
    #[must_use]
    pub const fn package(&self) -> &Identity {
        &self.package
    }

    /// Returns every top-level node.
    #[must_use]
    pub fn roots(&self) -> &[OutlineEntry] {
        &self.roots
    }

    /// Returns whether the bounded response carried the complete outline.
    #[must_use]
    pub const fn truncation(&self) -> Truncation {
        self.truncation
    }

    /// Returns the total number of nodes.
    #[must_use]
    pub fn count(&self) -> usize {
        self.roots
            .iter()
            .fold(0_usize, |total, root| total.saturating_add(root.count()))
    }

    /// Returns the number of nodes that did not resolve to a name.
    #[must_use]
    pub fn unresolved(&self) -> usize {
        fn walk(entry: &OutlineEntry) -> usize {
            entry
                .children()
                .iter()
                .fold(usize::from(entry.identity().is_none()), |total, child| {
                    total.saturating_add(walk(child))
                })
        }
        self.roots
            .iter()
            .fold(0_usize, |total, root| total.saturating_add(walk(root)))
    }
}

/// Builds a resolver over a row slice keyed by its symbol identity.
pub fn row_resolver(
    rows: &[backend_library::Row],
) -> impl FnMut(SymbolKey) -> Option<(Identity, Option<DeclarationKind>)> + '_ {
    move |symbol| {
        rows.iter()
            .find(|row| row.id == backend_library::RowId::Symbol(symbol))
            .map(|row| {
                (
                    Identity::parse_with_key(&row.label, IdentityKey::Symbol(symbol))
                        .with_captured_source(
                            row.source
                                .captured()
                                .map(backend_library::SourceLocation::path),
                            row.source
                                .captured()
                                .map(backend_library::SourceLocation::start_line),
                        ),
                    row.kind,
                )
            })
    }
}

#[cfg(test)]
mod retained_site_tests {
    use super::*;

    #[test]
    fn named_outline_borrows_its_retained_site_without_rewriting_the_operand() {
        let label = "/gymbro::semantic::owner::CaféService";
        let symbol = backend_library::symbol_key(label);
        let basis = backend_library::Basis::new(
            backend_library::view_state_root(&[]),
            backend_library::object_version(b"outline"),
        );
        let source = backend_library::SourceLocation::new("src/café.service.ts", 17);
        let Ok(source) = source else {
            panic!("valid retained site");
        };
        let rows =
            [
                backend_library::Row::new(backend_library::RowId::Symbol(symbol), basis, label)
                    .with_source(source),
            ];
        let mut resolve = row_resolver(&rows);
        let Some((identity, _)) = resolve(symbol) else {
            panic!("retained identity");
        };
        assert_eq!(identity.coordinate().as_str(), label);
        assert_eq!(identity.name(), "CaféService");
        assert_eq!(identity.language(), crate::Language::TypeScript);
        assert_eq!(identity.key(), IdentityKey::Symbol(symbol));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutlinePageState {
    pub(crate) basis: backend_library::Basis,
    pub(crate) sequence: u64,
    pub(crate) limit: u16,
    pub(crate) continuation: Option<backend_library::PageContinuation>,
    pub(crate) coverage: crate::CoverageLine,
}

/// Builds a forest induced by every row of one admitted outline page. An
/// out-of-page parent is not invented; cycles are promoted as roots once.
/// No declaration is dropped to make the rendering fit.
pub(crate) fn page_tree(
    path: &str,
    snapshot: &backend_library::ViewSnapshot,
    limit: u16,
    order: &backend_library::OutlinePageOrder,
) -> Result<OutlineTree, backend_client::ClientError> {
    use backend_library::{RowId, package_key};
    use std::collections::{BTreeMap, BTreeSet};
    let rows = snapshot.root.rows();
    let package = package_key(path);
    if !(1..=backend_library::QueryLimit::MAX).contains(&limit)
        || rows.len() > usize::from(limit)
        || rows
            .iter()
            .any(|row| row.package != Some(package) || !matches!(row.id, RowId::Symbol(_)))
    {
        return Err(backend_client::ClientError::Protocol(
            "outline page contains an out-of-scope declaration".to_owned(),
        ));
    }
    let credit = backend_library::QueryLimit::new(limit).ok_or_else(|| {
        backend_client::ClientError::Protocol("outline page credit is invalid".to_owned())
    })?;
    order.validate(snapshot, package, credit).map_err(backend_client::ClientError::Protocol)?;
    let ordered = order.symbols().iter().map(|id| {
        rows.iter().find(|row| row.id == RowId::Symbol(*id)).expect("validated exact outline membership")
    }).collect::<Vec<_>>();
    let symbols = ordered
        .iter()
        .filter_map(|row| match row.id {
            RowId::Symbol(id) => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let selected = symbols.iter().copied().collect::<BTreeSet<_>>();
    let mut children = BTreeMap::<SymbolKey, Vec<SymbolKey>>::new();
    let mut roots = Vec::new();
    for row in ordered {
        let RowId::Symbol(id) = row.id else {
            unreachable!()
        };
        match row.parent.filter(|id| selected.contains(id)) {
            Some(parent) => children.entry(parent).or_default().push(id),
            None => roots.push(id),
        }
    }
    fn node(
        id: SymbolKey,
        children: &BTreeMap<SymbolKey, Vec<SymbolKey>>,
        visited: &mut BTreeSet<SymbolKey>,
        resolve: &mut impl FnMut(SymbolKey) -> Option<(Identity, Option<DeclarationKind>)>,
    ) -> OutlineEntry {
        visited.insert(id);
        let resolved = resolve(id);
        let mut entries = Vec::new();
        for child in children.get(&id).into_iter().flatten() {
            if !visited.contains(child) {
                entries.push(node(*child, children, visited, resolve));
            }
        }
        OutlineEntry {
            tag: KeyTag::from_key(id.as_bytes()),
            identity: resolved.as_ref().map(|(identity, _)| identity.clone()),
            kind: resolved.and_then(|(_, kind)| kind),
            children: entries.into_boxed_slice(),
        }
    }
    let mut resolve = row_resolver(rows);
    let mut visited = BTreeSet::new();
    let mut entries = Vec::new();
    for id in roots.into_iter().chain(symbols) {
        if !visited.contains(&id) {
            entries.push(node(id, &children, &mut visited, &mut resolve));
        }
    }
    Ok(OutlineTree::new(
        Identity::parse_with_key(path, IdentityKey::Package(package)),
        entries,
        if snapshot.next.is_some() {
            OutlineExtent::Truncated
        } else {
            OutlineExtent::Complete
        },
    )
    .with_page(snapshot, limit))
}

#[cfg(test)]
mod page_scope_tests {
    use super::*;
    use backend_library::{
        Basis, Coverage, Cursor, Frontier, Lane, Library, PageRequest, QueryLimit, Reason, Row,
        RowId, ViewRoot, object_version, package_key, symbol_key, view_key, view_state_root,
    };
    #[test]
    fn page_forest_keeps_every_native_id_once_across_cycles_and_out_of_page_parents() {
        let path = "/scope";
        let basis = Basis::new(view_state_root(&[]), object_version(b"page-scope"));
        let a = symbol_key("a");
        let b = symbol_key("b");
        let c = symbol_key("c");
        let rows = vec![
            Row::new(RowId::Package(package_key(path)), basis, path),
            Row::in_package(RowId::Symbol(a), basis, package_key(path), "/scope::A").with_parent(b),
            Row::in_package(RowId::Symbol(b), basis, package_key(path), "/scope::B").with_parent(a),
            Row::in_package(RowId::Symbol(c), basis, package_key(path), "/scope::C")
                .with_parent(symbol_key("outside")),
        ];
        let root = ViewRoot::new_incomplete(
            view_key(b"page-scope"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            rows,
            vec![Coverage::Unavailable {
                lane: Lane::Exact,
                reason: Reason::NoIndex,
            }],
        )
        .expect("incomplete source");
        let cursor = Cursor::for_view_root(&root);
        let library = Library::from_view(root, cursor).expect("real immutable catalog");
        let page = library
            .outline_page(
                package_key(path),
                PageRequest::new(library.revision_root(), QueryLimit::new(3).expect("limit")),
            )
            .expect("actual native page");
        let tree = page_tree(path, &page.snapshot, 3, page.outline_order.as_ref().expect("order")).expect("induced forest");
        assert_eq!(tree.count(), 3);
        fn keys(node: &OutlineEntry, output: &mut std::collections::BTreeSet<SymbolKey>) {
            let IdentityKey::Symbol(id) = node.identity().expect("retained identity").key() else {
                panic!("symbol");
            };
            assert!(output.insert(id));
            for child in node.children() {
                keys(child, output);
            }
        }
        let mut actual = std::collections::BTreeSet::new();
        for root in tree.roots() {
            keys(root, &mut actual);
        }
        assert_eq!(actual, [a, b, c].into_iter().collect());
        assert!(page_tree("/foreign", &page.snapshot, 3, page.outline_order.as_ref().expect("order")).is_err());
        assert!(page_tree(path, &page.snapshot, 0, page.outline_order.as_ref().expect("order")).is_err());
        assert_eq!(
            tree.continuation(),
            page.snapshot
                .next
                .map(backend_library::PageContinuation::from_cursor)
        );
        let dto = crate::OutlineDto::new(&tree);
        let decoded: crate::OutlineDto =
            serde_json::from_value(serde_json::to_value(&dto).expect("projection"))
                .expect("stable typed decode");
        assert_eq!(decoded, dto);
    }
    #[test]
    fn legacy_single_package_pages_certify_membership_without_admitting_ambiguous_unscoped_rows() {
        let path = "/legacy-outline";
        let package = package_key(path);
        let foreign = package_key("/foreign-outline");
        let basis = Basis::new(view_state_root(&[]), object_version(b"legacy-outline"));
        let parent = symbol_key("legacy-parent");
        let child = symbol_key("legacy-child");
        let sibling = symbol_key("legacy-sibling");
        let mut rows = vec![
            Row::new(RowId::Package(package), basis, path),
            Row::new(RowId::Symbol(parent), basis, "/legacy-outline::Parent")
                .with_kind(DeclarationKind::Class),
            Row::new(RowId::Symbol(child), basis, "/legacy-outline::Child").with_parent(parent),
            Row::new(RowId::Symbol(sibling), basis, "/legacy-outline::Sibling").with_parent(parent),
        ];
        let catalog = |rows: Vec<Row>| {
            let root = ViewRoot::new_incomplete(
                view_key(b"legacy-outline"), basis,
                Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0), rows,
                vec![Coverage::Unavailable { lane: Lane::Exact, reason: Reason::NoIndex }],
            ).expect("legacy source root");
            let cursor = Cursor::for_view_root(&root);
            Library::from_view(root, cursor).expect("actual immutable catalog")
        };
        let library = catalog(rows.clone());
        let full = library.outline_page(package, PageRequest::new(
            library.revision_root(), QueryLimit::new(200).expect("full credit"),
        )).expect("legacy singleton producer");
        assert_eq!(full.snapshot.root.row_count(), 3);
        assert!(full.snapshot.root.rows().iter().all(|row| row.package == Some(package)));
        assert!(library.view().rows().iter().filter(|row| matches!(row.id, RowId::Symbol(_)))
            .all(|row| row.package.is_none()), "projection must preserve the legacy source facts");
        let expected = full.outline_order.as_ref().expect("native order").symbols().to_vec();
        assert_eq!(page_tree(path, &full.snapshot, 200, full.outline_order.as_ref().expect("order"))
            .expect("same strict presenter").count(), 3);
        let credit = QueryLimit::new(1).expect("bounded credit");
        let request = PageRequest::new(library.revision_root(), credit);
        let first = library.outline_page(package, request).expect("first bounded legacy page");
        assert_eq!(page_tree(path, &first.snapshot, 1, first.outline_order.as_ref().expect("order"))
            .expect("bounded legacy presenter").count(), 1);
        assert!(page_tree("/foreign-outline", &first.snapshot, 1,
            first.outline_order.as_ref().expect("order")).is_err());
        let continuation = first.snapshot.next.expect("actual native predecessor");
        assert!(matches!(library.outline_page(package, PageRequest::new(library.revision_root(),
            QueryLimit::new(2).expect("changed credit")).with_continuation(
                backend_library::PageContinuation::from_cursor(continuation))),
            Err(backend_library::LibraryError::CursorMismatch)));
        let mut unscoped_rows = first.snapshot.root.rows().to_vec();
        unscoped_rows[0].package = None;
        let unscoped_root = ViewRoot::new_incomplete(first.snapshot.root.recipe(),
            first.snapshot.root.basis(), first.snapshot.root.frontier(), unscoped_rows,
            first.snapshot.root.coverage().to_vec()).expect("same IDs but unproved membership");
        let unscoped_snapshot = backend_library::ViewSnapshot {
            root: unscoped_root.clone(), freshness: first.snapshot.freshness, next: None,
        };
        assert!(page_tree(path, &unscoped_snapshot, 1, first.outline_order.as_ref().expect("order")).is_err(),
            "a presenter cannot infer singleton membership from a bounded page");
        let forged = Cursor::for_view_root(&unscoped_root).with_query_offset(1);
        assert!(matches!(library.outline_page(package, request.with_continuation(
            backend_library::PageContinuation::from_cursor(forged))),
            Err(backend_library::LibraryError::CursorMismatch)),
            "rehashed unscoped predecessor cannot replace the certified membership");
        let mut actual = Vec::new();
        let mut page_request = request;
        loop {
            let cold = Library::from_view(library.view().clone(), Cursor::for_view_root(library.view()))
                .expect("cold source catalog");
            let page = cold.outline_page(package, page_request).expect("exact legacy continuation");
            let order = page.outline_order.as_ref().expect("actual native witness");
            assert_eq!(page_tree(path, &page.snapshot, 1, order).expect("strict page tree").count(), 1);
            actual.extend_from_slice(order.symbols());
            assert!(actual.len() <= expected.len(), "bounded continuation must progress");
            match page.terminal {
                backend_library::PageTerminal::More(next) => page_request = request.with_continuation(next),
                backend_library::PageTerminal::Complete => break,
                other => panic!("unexpected terminal: {other:?}"),
            }
        }
        assert_eq!(actual, expected);
        assert!(matches!(library.outline_page(foreign, request), Err(backend_library::LibraryError::NotFound)));
        rows.push(Row::new(RowId::Package(foreign), basis, "/foreign-outline"));
        let foreign_symbol = symbol_key("foreign-scoped");
        rows.push(Row::in_package(RowId::Symbol(foreign_symbol), basis, foreign, "/foreign-outline::Scoped"));
        let ambiguous = catalog(rows);
        let request = PageRequest::new(ambiguous.revision_root(), QueryLimit::new(200).expect("credit"));
        let empty = ambiguous.outline_page(package, request).expect("multi-package outline");
        assert_eq!(empty.snapshot.root.row_count(), 0, "ambiguous unscoped symbols stay outside every package");
        let foreign_page = ambiguous.outline_page(foreign, request).expect("exact foreign package");
        assert_eq!(foreign_page.outline_order.as_ref().expect("order").symbols(), &[foreign_symbol]);
        assert!(foreign_page.snapshot.root.rows().iter().all(|row| row.package == Some(foreign)));
        assert!(page_tree(path, &foreign_page.snapshot, 200, foreign_page.outline_order.as_ref().expect("order")).is_err());
    }

    #[test]
    fn small_and_large_outline_pages_share_one_exact_preorder_after_cold_reopen() {
        let path = "/paging";
        let package = package_key(path);
        let basis = Basis::new(view_state_root(&[]), object_version(b"outline-preorder"));
        let mut symbols = (0..231).map(|n| symbol_key(&format!("outline-{n}"))).collect::<Vec<_>>();
        symbols.sort_unstable();
        let parents = [symbols[229], symbols[230]];
        let mut rows = vec![Row::new(RowId::Package(package), basis, path)];
        for (group, parent) in parents.into_iter().enumerate() {
            rows.push(Row::in_package(RowId::Symbol(parent), basis, package, format!("/paging::Parent{group}"))
                .with_kind(DeclarationKind::Class));
            for child in 0..112 {
                // Same names, same lines, different exact entities/parents.
                // Repeated type-literal members and anonymous callables must
                // not be hidden by a display-name or coordinate deduplication.
                let id = symbols[group * 112 + child];
                let name = if child < 2 { "<anonymous callable>" } else { "teacher_id" };
                rows.push(Row::in_package(RowId::Symbol(id), basis, package,
                    format!("/paging::semantic::{:064x}::{name}", group * 112 + child + 1))
                    .with_parent(parent)
                    .with_kind(if child < 2 { DeclarationKind::Function } else { DeclarationKind::Field })
                    .with_source(backend_library::SourceLocation::new("src/shared.ts", 1).expect("site")));
            }
        }
        for (index, parent) in [(224, symbols[226]), (225, symbols[224]), (226, symbols[225]), (227, symbols[227]), (228, symbol_key("outside-package"))] {
            rows.push(Row::in_package(RowId::Symbol(symbols[index]), basis, package, format!("/paging::Cycle{index}")).with_parent(parent));
        }
        rows.reverse();
        let root = ViewRoot::new_incomplete(
            view_key(b"outline-preorder"), basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0), rows,
            vec![Coverage::Unavailable { lane: Lane::Exact, reason: Reason::NoIndex }],
        ).expect("retained source");
        let cursor = Cursor::for_view_root(&root);
        let library = Library::from_view(root, cursor).expect("catalog");
        fn flatten(tree: &OutlineTree) -> Vec<(IdentityKey, String)> {
            fn visit(node: &OutlineEntry, output: &mut Vec<(IdentityKey, String)>) {
                let identity = node.identity().expect("resolved retained entity");
                output.push((identity.key(), identity.coordinate().as_str().to_owned()));
                for child in node.children() { visit(child, output); }
            }
            let mut output = Vec::new();
            for root in tree.roots() { visit(root, &mut output); }
            output
        }
        let large = library.outline_page(package, PageRequest::new(
            library.revision_root(), QueryLimit::new(200).expect("large limit"),
        )).expect("large page");
        let large_tree = page_tree(path, &large.snapshot, 200, large.outline_order.as_ref().expect("order")).expect("large forest");
        let prefix = flatten(&large_tree);
        assert_eq!(prefix.len(), 200);
        assert_eq!(large_tree.truncation(), crate::Truncation::Truncated);
        assert!(large_tree.continuation().is_some(), "the capped default has a real continuation");
        assert_eq!(crate::Answer::Outline(Box::new(large_tree.clone())).continuation(), large_tree.continuation());
        assert!(crate::render::text::outline(&large_tree, crate::render::Theme::plain()).contains("--limit 200"));
        assert!(crate::render::markdown::outline(&large_tree).contains("next cursor"));
        // The renderer must forward the exact portable token supplied by the
        // admitted CLI caller; it does not mint a token from native cursor data.
        let supplied_cursor = "exact-caller-issued-outline-token";
        let answer = crate::Answer::Outline(Box::new(large_tree.clone()));
        for detail in [crate::Detail::Standard, crate::Detail::Summary, crate::Detail::Full] {
            for rendered in [
                crate::render::text::answer_with_detail(&answer, crate::render::Theme::plain(), detail,
                    Some(supplied_cursor), crate::render::ContinuationTarget::CliOption),
                crate::render::markdown::answer_with_detail(&answer, detail,
                    Some(supplied_cursor), crate::render::ContinuationTarget::CliOption),
            ] {
                assert!(rendered.contains("--limit 200"));
                assert!(rendered.contains("`--cursor`"));
                assert!(rendered.contains(supplied_cursor), "the actual supplied CLI token must be visible");
                assert_eq!(rendered.matches(supplied_cursor).count(), 1);
            }
        }
        assert!(large.snapshot.root.rows().iter().all(|row| row.score.is_none()));
        let mut expected = prefix.clone();
        let tail = library.outline_page(package, PageRequest::new(library.revision_root(), QueryLimit::new(200).expect("credit"))
            .with_continuation(large_tree.continuation().expect("native continuation"))).expect("large tail");
        let tail_tree = page_tree(path, &tail.snapshot, 200, tail.outline_order.as_ref().expect("order")).expect("tail forest");
        let complete = crate::Answer::Outline(Box::new(tail_tree.clone()));
        for rendered in [
            crate::render::text::answer_with_cursor(&complete, crate::render::Theme::plain(),
                Some(supplied_cursor), crate::render::ContinuationTarget::CliOption),
            crate::render::markdown::answer_with_cursor(&complete,
                Some(supplied_cursor), crate::render::ContinuationTarget::CliOption),
            crate::render::text::answer_with_cursor(&answer, crate::render::Theme::plain(),
                None, crate::render::ContinuationTarget::CliOption),
            crate::render::markdown::answer_with_cursor(&answer,
                None, crate::render::ContinuationTarget::CliOption),
        ] {
            assert!(!rendered.contains(supplied_cursor), "no cursor is invented for complete pages or missing caller tokens");
        }
        expected.extend(flatten(&tail_tree));
        assert_eq!(expected.len(), symbols.len());
        assert_eq!(expected.iter().map(|(id, _)| *id).collect::<std::collections::BTreeSet<_>>().len(), symbols.len());
        for limit in [1, 2, 5, 200] {
            let request = PageRequest::new(library.revision_root(), QueryLimit::new(limit).expect("limit"));
            let mut continued = request;
            let mut actual = Vec::new();
            let mut pages = 0;
            loop {
                pages += 1;
                assert!(pages <= symbols.len() + 1, "continuation must make bounded progress");
                let cold = Library::from_view(library.view().clone(), Cursor::for_view_root(library.view())).expect("cold catalog");
                let page = cold.outline_page(package, continued).expect("exact continuation");
                let tree = page_tree(path, &page.snapshot, limit, page.outline_order.as_ref().expect("order")).expect("bounded forest");
                assert!(tree.count() <= usize::from(limit));
                actual.extend(flatten(&tree));
                match page.terminal {
                    backend_library::PageTerminal::More(next) => continued = request.with_continuation(next),
                    backend_library::PageTerminal::Complete => break,
                    other => panic!("unexpected page terminal: {other:?}"),
                }
            }
            assert_eq!(actual, expected, "limit {limit}: identity and order are independent of the page size");
        }
        let legacy = library.outline(backend_library::OutlineQuery::new(package, library.revision_root())).expect("legacy tree");
        let legacy = crate::assemble::outline_tree(path, &legacy, large.snapshot.root.rows());
        assert_eq!(flatten(&legacy), prefix, "default bounded tree uses the same preorder prefix");
    }

}
