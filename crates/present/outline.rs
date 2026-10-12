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
    let symbols = rows
        .iter()
        .filter_map(|row| match row.id {
            RowId::Symbol(id) => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    let selected = symbols.iter().copied().collect::<BTreeSet<_>>();
    let mut children = BTreeMap::<SymbolKey, Vec<SymbolKey>>::new();
    let mut roots = Vec::new();
    for row in rows {
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
        let tree = page_tree(path, &page.snapshot, 3).expect("induced forest");
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
        assert!(page_tree("/foreign", &page.snapshot, 3).is_err());
        assert!(page_tree(path, &page.snapshot, 0).is_err());
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
}
