//! A package outline as a tree of names.
//!
//! The engine answers an outline query with a tree of [`SymbolKey`] values and
//! nothing else — the shape is there, the words are not. Rendering that tree
//! directly is what produced today's unusable output: eighteen lines of bare
//! hexadecimal. So the model requires a resolver: an outline is only built
//! once every node has been matched to a row that carries its coordinate, and
//! a node that cannot be matched says so with its tag rather than pretending
//! to be a name.

use crate::identity::{Identity, IdentityKey, IdentityShape, KeyTag};
use backend_library::{DeclarationKind, OutlineExtent, OutlineNode, SymbolKey};
use std::collections::{BTreeMap, BTreeSet};

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

    /// The text a window fingerprints this entry by.
    fn sort_key(&self) -> &str {
        self.identity
            .as_ref()
            .map_or("", |identity| identity.coordinate().as_str())
    }

    /// The bytes this entry costs as one line of an indented outline.
    fn row_bytes(&self, depth: usize) -> usize {
        let trail = self
            .identity
            .as_ref()
            .map_or(0, |identity| identity.trail_within(None).len());
        depth
            .min(16)
            .saturating_mul(2)
            .saturating_add(self.name().len())
            .saturating_add(trail)
            .saturating_add(8)
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

/// The declaration an entry names: its project, file, line and name, which
/// both publication lanes agree on for one declaration.
fn site_of(entry: &OutlineEntry) -> Option<(String, String, u32, String)> {
    let identity = entry.identity.as_ref()?;
    if !matches!(
        identity.shape(),
        IdentityShape::Declaration | IdentityShape::Semantic
    ) {
        return None;
    }
    Some((
        identity.project()?.root().to_owned(),
        identity.path()?.as_str().to_owned(),
        identity.line()?.get(),
        identity.name().to_owned(),
    ))
}

/// Merges, among siblings, each structural/compiler-backed pair of entries
/// for one declaration into the structural entry, whose children are both
/// entries' children (merged the same way, since a declaration's members are
/// published by both lanes too).
///
/// The pairing is as strict as the owner's: the same site, exactly one entry
/// per lane, and an import only with an import. Anything else stays listed.
pub(crate) fn merge_lane_twins(entries: Vec<OutlineEntry>) -> Vec<OutlineEntry> {
    let mut sites = BTreeMap::<(String, String, u32, String), (Vec<usize>, Vec<usize>)>::new();
    for (at, entry) in entries.iter().enumerate() {
        let Some(site) = site_of(entry) else {
            continue;
        };
        let lanes = sites.entry(site).or_default();
        if entry
            .identity
            .as_ref()
            .is_some_and(|identity| identity.shape() == IdentityShape::Semantic)
        {
            lanes.1.push(at);
        } else {
            lanes.0.push(at);
        }
    }
    let mut twin_of = BTreeMap::<usize, usize>::new();
    let mut shadowed = BTreeSet::<usize>::new();
    for (structural, semantic) in sites.values() {
        let ([structural], [semantic]) = (structural.as_slice(), semantic.as_slice()) else {
            continue;
        };
        let imports = |at: usize| entries[at].kind == Some(DeclarationKind::Import);
        if imports(*structural) == imports(*semantic) {
            twin_of.insert(*structural, *semantic);
            shadowed.insert(*semantic);
        }
    }
    let mut slots = entries.into_iter().map(Some).collect::<Vec<_>>();
    let mut merged = Vec::with_capacity(slots.len());
    for at in 0..slots.len() {
        if shadowed.contains(&at) {
            continue;
        }
        let Some(mut entry) = slots[at].take() else {
            continue;
        };
        let mut children = entry.children.into_vec();
        if let Some(twin) = twin_of.get(&at).and_then(|twin| slots[*twin].take()) {
            entry.kind = entry.kind.or(twin.kind);
            children.extend(twin.children.into_vec());
        }
        entry.children = merge_lane_twins(children).into_boxed_slice();
        merged.push(entry);
    }
    merged
}

/// One package outline, resolved to names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineTree {
    package: Identity,
    roots: Box<[OutlineEntry]>,
    truncation: Truncation,
    page: Option<OutlinePageState>,
    window: Option<crate::window::Window>,
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
            window: None,
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

    /// Returns where the declarations were cut to a window, when they were.
    #[must_use]
    pub const fn window(&self) -> Option<crate::window::Window> {
        self.window
    }

    /// Cuts the outline to one window of its declarations, in outline order.
    ///
    /// A declaration whose parent falls outside the window is listed at the
    /// level of its nearest listed ancestor, the way an owner page lists a row
    /// whose parent is on another page: no declaration is invented, and none
    /// inside the window is dropped.
    ///
    /// # Errors
    ///
    /// Returns the stale-cursor fault when the caller resumes an outline that
    /// has changed, and a usage fault when its cursor is malformed.
    pub(crate) fn windowed(
        mut self,
        request: crate::window::WindowRequest,
    ) -> Result<Self, crate::fault::Fault> {
        use crate::window::{Window, WindowKind, fingerprint};
        // An owner-paged outline is already one page of the list; its own
        // cursor continues it.
        if self.page.is_some() {
            return Ok(self);
        }
        let mut entries = Vec::new();
        for root in self.roots.iter() {
            flatten(root, 0, &mut entries);
        }
        let print = fingerprint(entries.iter().map(|(_, entry)| entry.sort_key()));
        let start = request.start(WindowKind::Outline, print, entries.len())?;
        let text = entries
            .iter()
            .map(|(depth, entry)| entry.row_bytes(*depth))
            .collect::<Vec<_>>();
        // A typed node carries its whole coordinate, which the readable row
        // abbreviates to a trail: three times the text is a safe estimate.
        let typed = text.iter().map(|bytes| bytes.saturating_mul(3)).collect::<Vec<_>>();
        let window = Window::select(WindowKind::Outline, request, start, print, &text, &typed);
        if window.is_cut() {
            let mut seen = 0_usize;
            let mut kept = Vec::new();
            for root in self.roots.iter() {
                cut(root, &mut seen, &window.range(), &mut kept);
            }
            self.roots = kept.into_boxed_slice();
            self.window = Some(window);
        }
        Ok(self)
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

fn flatten<'a>(entry: &'a OutlineEntry, depth: usize, out: &mut Vec<(usize, &'a OutlineEntry)>) {
    out.push((depth, entry));
    for child in entry.children() {
        flatten(child, depth.saturating_add(1), out);
    }
}

/// Keeps the entries whose position in outline order is inside `range`,
/// promoting the listed descendants of an entry that is not.
fn cut(
    entry: &OutlineEntry,
    seen: &mut usize,
    range: &core::ops::Range<usize>,
    out: &mut Vec<OutlineEntry>,
) {
    let position = *seen;
    *seen += 1;
    let mut children = Vec::new();
    for child in entry.children() {
        cut(child, seen, range, &mut children);
    }
    if range.contains(&position) {
        out.push(OutlineEntry {
            identity: entry.identity.clone(),
            tag: entry.tag,
            kind: entry.kind,
            children: children.into_boxed_slice(),
        });
    } else {
        out.extend(children);
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
        merge_lane_twins(entries),
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

#[cfg(test)]
mod lane_twin_tests {
    use super::*;

    const PROJECT: &str = "/abs/p";

    fn entry(
        label: &str,
        line: u32,
        kind: DeclarationKind,
        children: Vec<OutlineEntry>,
    ) -> OutlineEntry {
        let identity = Identity::parse(label).with_captured_source(Some("src/a.rs"), Some(line));
        OutlineEntry {
            tag: KeyTag::from_key(&[7; 32]),
            identity: Some(identity),
            kind: Some(kind),
            children: children.into_boxed_slice(),
        }
    }

    fn structural(
        name: &str,
        line: u32,
        kind: DeclarationKind,
        children: Vec<OutlineEntry>,
    ) -> OutlineEntry {
        entry(
            &format!("{PROJECT}::src/a.rs:{line}::{name}"),
            line,
            kind,
            children,
        )
    }

    fn semantic(
        name: &str,
        line: u32,
        kind: DeclarationKind,
        children: Vec<OutlineEntry>,
    ) -> OutlineEntry {
        entry(
            &format!("{PROJECT}::semantic::{}::{name}", "ab".repeat(32)),
            line,
            kind,
            children,
        )
    }

    #[test]
    fn each_declaration_both_lanes_published_is_one_entry_with_both_lanes_members() {
        // `Opt` (line 19) is listed by both lanes; the compiler also lists a
        // field the structural baseline cannot see, and both list `parse`.
        let merged = merge_lane_twins(vec![
            semantic(
                "Opt",
                19,
                DeclarationKind::Struct,
                vec![
                    semantic("alphabet", 21, DeclarationKind::Field, vec![]),
                    semantic("parse", 20, DeclarationKind::Method, vec![]),
                ],
            ),
            structural(
                "Opt",
                19,
                DeclarationKind::Struct,
                vec![structural("parse", 20, DeclarationKind::Method, vec![])],
            ),
            structural("Other", 40, DeclarationKind::Struct, vec![]),
        ]);
        assert_eq!(
            merged.iter().map(OutlineEntry::name).collect::<Vec<_>>(),
            ["Opt", "Other"]
        );
        let opt = &merged[0];
        assert_eq!(
            opt.identity()
                .map(|identity| identity.coordinate().as_str()),
            Some("/abs/p::src/a.rs:19::Opt"),
            "the structural coordinate is the one entry's address"
        );
        let members = opt
            .children()
            .iter()
            .map(OutlineEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 2, "{members:?}");
        assert!(members.contains(&"parse".to_owned()) && members.contains(&"alphabet".to_owned()));
        assert_eq!(merged.iter().map(OutlineEntry::count).sum::<usize>(), 4);
    }

    #[test]
    fn entries_without_a_one_to_one_pair_stay_listed() {
        let two_compiler = merge_lane_twins(vec![
            structural("Opt", 19, DeclarationKind::Struct, vec![]),
            semantic("Opt", 19, DeclarationKind::Struct, vec![]),
            semantic("Opt", 19, DeclarationKind::Struct, vec![]),
        ]);
        assert_eq!(two_compiler.len(), 3);
        let import_against_declaration = merge_lane_twins(vec![
            structural("Opt", 19, DeclarationKind::Struct, vec![]),
            semantic("Opt", 19, DeclarationKind::Import, vec![]),
        ]);
        assert_eq!(import_against_declaration.len(), 2);
    }
}
