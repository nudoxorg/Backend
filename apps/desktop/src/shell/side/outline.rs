//! The lists made from a package's outline: the outline itself (what a
//! symbol page's sidebar follows), the children of a hoisted node, the same
//! narrowed in place, and the package intro's cut of the API by what
//! matters to you (`listing`'s "contextual rule": on the intro the page
//! already draws every module, so the sidebar does not).

use super::narrow::{Filter, Matched, find_fold};
use super::row::{Do, Fold, Folds, Heading, Item, KindGroup, Mark, Row, RowId, Section, Trailing};
use super::scope::{Scope, contains};
use super::state::{Change, Gone, Rollup, RowState, StateBook};
use crate::model::pages::{Known, OutlineNode, OutlineTree, PackageRef, PageKey, SymbolRef};
use crate::shell::kit::{kind_of, symbol_route};
use backend_library::DeclarationKind;
use gpui::SharedString;
use std::cmp::Reverse;

/// How many members an open type lists before "N more" (hoist to see all).
const MEMBERS_SHOWN: usize = 12;
/// How many items a section of the package intro lists before "N more".
const LISTED_SHOWN: usize = 12;
/// How many items an open kind group lists before "N more".
const KIND_SHOWN: usize = 40;

/// Whether the outline lists a node at all. An import (`use`) is what a
/// module brings in, not what it holds: it is neither a row nor part of the
/// count.
pub(super) fn is_listed(node: &OutlineNode) -> bool {
    // A row that holds others is kept whatever its name (a `crate` root
    // with items in it); a leaf that is a type written out or a keyword is
    // not something the package declares.
    node.decl.kind != Some(DeclarationKind::Import)
        && (crate::shell::kit::names_a_declaration(&node.decl.name) || !node.children.is_empty())
}

/// The children of a node that are listed. A function's are its own (its
/// parameters and locals, which a compiler's rows also carry): not part of
/// the package's outline.
pub(super) fn kids(node: &OutlineNode) -> impl Iterator<Item = &OutlineNode> {
    let own = matches!(
        node.decl.kind,
        Some(DeclarationKind::Function | DeclarationKind::Method | DeclarationKind::Constructor)
    );
    node.children
        .iter()
        .filter(move |child| !own && is_listed(child))
}

/// How many names a subtree holds, itself included (imports left out): what
/// you would count if every group were open.
pub(super) fn names(node: &OutlineNode) -> usize {
    usize::from(is_listed(node)) + kids(node).map(names).sum::<usize>()
}

/// How many names an outline holds.
pub(super) fn tree_names(tree: &OutlineTree) -> usize {
    tree.roots
        .iter()
        .filter(|node| is_listed(node))
        .map(names)
        .sum()
}

/// What entering a node does to the path of what is inside it.
enum Descent {
    /// Nothing: `lib`, `main` and `mod` are the crate or the directory itself.
    Same,
    /// One more name.
    Push(String),
    /// The path is now this one: a file module's own path from the crate root.
    Reset(Vec<String>),
}

/// How to undo [`enter`].
enum Restore {
    Nothing,
    Pop,
    Whole(Vec<String>),
}

fn descent(node: &OutlineNode) -> Descent {
    if let Some(path) = file_module(node) {
        return Descent::Reset(path);
    }
    let name = shelf_name(node);
    if node.decl.kind == Some(DeclarationKind::Module)
        && matches!(name.as_str(), "lib" | "main" | "mod")
    {
        Descent::Same
    } else {
        Descent::Push(name)
    }
}

/// Goes into `node`: `chain` is then the path of what it holds.
fn enter(chain: &mut Vec<String>, node: &OutlineNode) -> Restore {
    match descent(node) {
        Descent::Same => Restore::Nothing,
        Descent::Push(name) => {
            chain.push(name);
            Restore::Pop
        }
        Descent::Reset(path) => Restore::Whole(std::mem::replace(chain, path)),
    }
}

fn leave(chain: &mut Vec<String>, restore: Restore) {
    match restore {
        Restore::Nothing => {}
        Restore::Pop => {
            chain.pop();
        }
        Restore::Whole(old) => *chain = old,
    }
}

/// A module the index names by its file (`map.rs`, in `src/de/`): its path
/// from the crate root (`de::map`; `src/de/mod.rs` is `de`; `src/lib.rs`
/// is `lib`). One file name repeats across directories (`map` in `de`, in
/// `ser` and in `edit`), so the path is what tells them apart.
fn file_module(node: &OutlineNode) -> Option<Vec<String>> {
    if node.decl.kind != Some(DeclarationKind::Module)
        || node.decl.name.as_ref() == shelf_name(node)
    {
        return None;
    }
    Some(module_of_file(
        node.decl.path.as_deref().unwrap_or_default(),
        shelf_name(node),
    ))
}

/// The module a source file is: its directories below the source root, then
/// its stem (`mod.rs`, `index.ts` and `__init__.py` are their directory).
/// Everything up to the source root (`crates/toml/src/`) is where the
/// package is, not its modules.
fn module_of_file(path: &str, stem: String) -> Vec<String> {
    let mut dirs: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    dirs.pop();
    if let Some(root) = dirs.iter().position(|part| part == "src") {
        dirs.drain(..=root);
    }
    if !(matches!(stem.as_str(), "mod" | "index" | "__init__") && !dirs.is_empty()) {
        dirs.push(stem);
    }
    dirs
}

/// The module a name says it is declared in, for a name the index places at
/// the top level of the outline on its own (a compiler-backed outline may):
/// each such name carries the path of the file it is in.
fn declared_in(node: &OutlineNode) -> Vec<String> {
    let Some(path) = node.decl.path.as_deref() else {
        return Vec::new();
    };
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    module_of_file(path, stem.to_owned())
}

/// The path in the release data of `node`, found under `ancestors`.
pub(super) fn path_of(
    book: &StateBook,
    ancestors: &[&OutlineNode],
    node: &OutlineNode,
) -> SharedString {
    let mut chain: Vec<String> = Vec::new();
    for above in ancestors.iter().copied() {
        let _ = enter(&mut chain, above);
    }
    book.path_of(
        chain
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(node.decl.name.as_ref())),
    )
}

/// A node's name as its row says it, and the quiet words that tell it from
/// the others of the same name: a file module is its last path segment,
/// with the rest of the path after it (`map`, `de`).
pub(super) fn label(node: &OutlineNode) -> (String, Option<String>) {
    match file_module(node) {
        Some(mut path) => {
            let name = path.pop().unwrap_or_else(|| shelf_name(node));
            (name, (!path.is_empty()).then(|| path.join("::")))
        }
        None => (shelf_name(node), None),
    }
}

/// How many items the package intro lists (the top-level names of its
/// modules, imports and members left out): the page's own "N public names".
pub(super) fn items(tree: &OutlineTree) -> usize {
    let mut found = Vec::new();
    collect(&tree.roots, &mut Vec::new(), &mut found);
    found.len()
}

/// What lists an outline need to know.
pub(super) struct Outline<'a> {
    /// The package the outline belongs to (routes are built from it).
    pub package: &'a PackageRef,
    /// The declaration the reader is on.
    pub current: Option<&'a SymbolRef>,
    /// The groups opened or closed by hand.
    pub folds: &'a Folds,
    /// What each item carries.
    pub book: &'a StateBook,
    /// What narrows the list.
    pub filter: Filter<'a>,
}

/// A list, and how much of its scope it shows.
pub(super) struct Listed {
    /// The rows.
    pub rows: Vec<Row>,
    /// How much of the scope matched (all of it when nothing narrows).
    pub matched: Matched,
}

impl Outline<'_> {
    /// The whole outline: modules first, the one holding the page you are
    /// on open, test-only modules folded into one trailing row.
    pub(super) fn package_rows(&self, tree: &OutlineTree) -> Listed {
        let total = tree_names(tree);
        let mut roots: Vec<&OutlineNode> =
            tree.roots.iter().filter(|node| is_listed(node)).collect();
        // Modules named by their files come in the index's order; by path, `de` and
        // everything in it sit together.
        if roots.iter().any(|node| file_module(node).is_some()) {
            roots.sort_by_cached_key(|node| {
                file_module(node).unwrap_or_else(|| vec![shelf_name(node)])
            });
        }
        if self.filter.is_active() {
            return self.narrowed(&roots, &[], total);
        }
        let mut rows = Vec::new();
        let mut tests: Vec<&OutlineNode> = Vec::new();
        let mut chain = Vec::new();
        for root in roots {
            if is_test_module(root) {
                tests.push(root);
            } else {
                let before = rows.len();
                self.push(root, 0, &mut chain, &mut rows, &mut tests);
                // A name the index puts at the top level on its own says its module.
                if root.decl.kind != Some(DeclarationKind::Module)
                    && let Some(Row::Item(item)) = rows.get_mut(before)
                    && item.sub.is_none()
                {
                    let module = declared_in(root).join("::");
                    item.sub = (!module.is_empty()).then(|| SharedString::from(module));
                }
            }
        }
        self.push_tests(&tests, &mut chain, &mut rows);
        Listed {
            rows,
            matched: Matched {
                shown: total,
                of: total,
            },
        }
    }

    /// The children of a hoisted node, the ones your code uses first.
    pub(super) fn node_rows(&self, node: &OutlineNode, above: &[&OutlineNode]) -> Listed {
        let children: Vec<&OutlineNode> = kids(node).collect();
        let total: usize = children.iter().map(|child| names(child)).sum();
        let mut chain: Vec<String> = Vec::new();
        for node in above.iter().copied().chain(std::iter::once(node)) {
            let _ = enter(&mut chain, node);
        }
        if self.filter.is_active() {
            let up: Vec<String> = chain;
            return self.narrowed(&children, &up, total);
        }
        let mut rows = Vec::new();
        let mut tests: Vec<&OutlineNode> = Vec::new();
        for child in self.by_use(&children, &mut chain) {
            if is_test_module(child) {
                tests.push(child);
            } else {
                self.push(child, 0, &mut chain, &mut rows, &mut tests);
            }
        }
        self.push_tests(&tests, &mut chain, &mut rows);
        Listed {
            rows,
            matched: Matched {
                shown: total,
                of: total,
            },
        }
    }

    /// The nodes of a level with the ones your code uses first (stable: the
    /// outline's own order breaks ties).
    fn by_use<'n>(
        &self,
        nodes: &[&'n OutlineNode],
        chain: &mut Vec<String>,
    ) -> Vec<&'n OutlineNode> {
        let mut list: Vec<(&'n OutlineNode, u32)> = nodes
            .iter()
            .map(|node| (*node, self.state(node, chain).uses.unwrap_or(0)))
            .collect();
        list.sort_by_key(|(_, uses)| Reverse(*uses));
        list.into_iter().map(|(node, _)| node).collect()
    }

    fn push<'n>(
        &self,
        node: &'n OutlineNode,
        depth: u8,
        chain: &mut Vec<String>,
        rows: &mut Vec<Row>,
        tests: &mut Vec<&'n OutlineNode>,
    ) {
        let group = kids(node).next().is_some();
        let id = RowId::Node(node.decl.coordinate.clone());
        let holds_current = self.current.is_some_and(|current| contains(node, current));
        let open = group && self.folds.is_open(&id, holds_current);
        let state = self.state(node, chain);
        let Some(item) = self.item(node, depth, group.then(|| Fold::of(open)), state, chain) else {
            return;
        };
        rows.push(Row::Item(item));
        if !open {
            return;
        }
        let module = node.decl.kind == Some(DeclarationKind::Module);
        let restore = enter(chain, node);
        let mut hidden = 0usize;
        let mut listed = 0usize;
        for child in kids(node) {
            // Test modules never sit among the real ones, wherever in the top two levels they are.
            if depth == 0 && is_test_module(child) {
                tests.push(child);
                continue;
            }
            if !module && listed >= MEMBERS_SHOWN {
                hidden += 1;
                continue;
            }
            listed += 1;
            self.push(child, depth.saturating_add(1), chain, rows, tests);
        }
        leave(chain, restore);
        if hidden > 0 {
            rows.push(Row::Note(format!("{hidden} more · → to see them").into()));
        }
    }

    /// The trailing "tests" row, and, opened, the modules under it.
    fn push_tests(&self, tests: &[&OutlineNode], chain: &mut Vec<String>, rows: &mut Vec<Row>) {
        if tests.is_empty() {
            return;
        }
        let holds_current = self
            .current
            .is_some_and(|current| tests.iter().any(|node| contains(node, current)));
        let open = self.folds.is_open(&RowId::Tests, holds_current);
        let mut fold = Item::new(
            RowId::Tests,
            0,
            Mark::Kind(facet::icons::Kind::Module),
            "tests",
            Do::Fold(RowId::Tests),
        );
        fold.fold = Some(Fold::of(open));
        rows.push(Row::Item(fold));
        if open {
            for node in tests {
                self.push(node, 1, chain, rows, &mut Vec::new());
            }
        }
    }

    /// The outline narrowed in place: every node is searched (collapsed
    /// ones too), each match keeps its parents as context and its own
    /// matching children open beneath it.
    fn narrowed(&self, nodes: &[&OutlineNode], above: &[String], total: usize) -> Listed {
        let mut rows = Vec::new();
        let mut chain = above.to_vec();
        let shown = self.filtered(nodes, 0, &mut chain, &mut rows);
        Listed {
            rows,
            matched: Matched { shown, of: total },
        }
    }

    /// Emits the matching rows of `nodes` (and their context) into `out`;
    /// returns how many nodes matched.
    fn filtered(
        &self,
        nodes: &[&OutlineNode],
        depth: u8,
        chain: &mut Vec<String>,
        out: &mut Vec<Row>,
    ) -> usize {
        let mut matched = 0;
        for node in nodes.iter().copied() {
            let name = label(node).0;
            let hit = find_fold(&name, self.filter.query);
            let restore = enter(chain, node);
            let mut below = Vec::new();
            let children: Vec<&OutlineNode> = kids(node).collect();
            let inner = self.filtered(&children, depth.saturating_add(1), chain, &mut below);
            leave(chain, restore);
            let own = self.matches(node, hit.is_some(), chain);
            if !own && inner == 0 {
                continue;
            }
            matched += inner + usize::from(own);
            let group = !children.is_empty();
            let state = self.state(node, chain);
            if let Some(mut item) = self.item(
                node,
                depth,
                group.then(|| Fold::of(inner > 0)),
                state,
                chain,
            ) {
                item.hit = hit.filter(|_| !self.filter.query.is_empty());
                out.push(Row::Item(item));
                out.append(&mut below);
            }
        }
        matched
    }

    /// Whether a node itself matches: the typed words are in its name, and
    /// with a crate chosen, that crate uses it (a module is only ever kept
    /// as the parent of what matches).
    fn matches(&self, node: &OutlineNode, named: bool, chain: &[String]) -> bool {
        let word_ok = self.filter.query.is_empty() || named;
        let via_ok = self.filter.via.is_none_or(|via| {
            node.decl.kind != Some(DeclarationKind::Module) && {
                let path = self.book.path_of(
                    chain
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(node.decl.name.as_ref())),
                );
                self.book.uses_of(&path, via) > 0
            }
        });
        word_ok && via_ok
    }

    /// The row for one node.
    fn item(
        &self,
        node: &OutlineNode,
        depth: u8,
        fold: Option<Fold>,
        state: RowState,
        chain: &[String],
    ) -> Option<Item> {
        let symbol = node.decl.coordinate.clone();
        let group = kids(node).next().is_some();
        let module_group = node.decl.kind == Some(DeclarationKind::Module) && group;
        let does = if module_group {
            Do::Fold(RowId::Node(symbol.clone()))
        } else {
            Do::Go(symbol_route(self.package.as_str(), &symbol)?)
        };
        let (name, quiet) = label(node);
        let mut item = Item::new(
            RowId::Node(symbol.clone()),
            depth,
            Mark::Kind(kind_of(node.decl.kind)),
            name,
            does,
        );
        item.sub = quiet.map(SharedString::from);
        item.current = self.current == Some(&symbol);
        item.fold = fold;
        item.hoists = group.then(|| Scope::Node(symbol.clone()));
        if !module_group {
            item.warm = Some(PageKey::Symbol(symbol.clone()));
            item.source = Some(symbol);
            if !self.book.is_empty() {
                item.path = Some(
                    self.book.path_of(
                        chain
                            .iter()
                            .map(String::as_str)
                            .chain(std::iter::once(node.decl.name.as_ref())),
                    ),
                );
            }
        }
        item.trailing = if state.is_quiet() {
            Trailing::Nothing
        } else {
            Trailing::State(state)
        };
        Some(item)
    }

    /// What one node carries: its own uses and changes, its members' rolled
    /// in, a coral word for a deprecation the index recorded.
    fn state(&self, node: &OutlineNode, chain: &mut Vec<String>) -> RowState {
        let module = node.decl.kind == Some(DeclarationKind::Module);
        let deprecated = matches!(node.decl.facts.deprecation, Known::Known(Some(_)));
        let mut own = RowState::default();
        if !module && !self.book.is_empty() {
            let path = self.book.path_of(
                chain
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(node.decl.name.as_ref())),
            );
            own = self.book.state_of(&path);
        }
        if deprecated && own.change == Change::Still {
            own.change = Change::Gone(Gone::Deprecated);
        }
        if kids(node).next().is_none() {
            return own;
        }
        let restore = enter(chain, node);
        let mut rollup = Rollup::default();
        rollup.add(&own);
        for child in kids(node) {
            rollup.add(&self.state(child, chain));
        }
        leave(chain, restore);
        let mut state = rollup.finish(kids(node).count());
        if let Change::Gone(gone) = own.change {
            state.change = Change::Gone(gone);
        }
        state
    }

    /// The package intro's Contents: the API by what matters to you, since
    /// the page beside it already draws every module. Items your code uses,
    /// items that change in the release being read, then every kind family
    /// (each a group that opens in place to a flat list, its module a quiet
    /// word after each name).
    pub(super) fn intro_rows(&self, tree: &OutlineTree) -> Vec<Row> {
        let mut items = Vec::new();
        collect(&tree.roots, &mut Vec::new(), &mut items);
        let states: Vec<(usize, RowState)> = items
            .iter()
            .enumerate()
            .map(|(index, found)| (index, self.state(found.node, &mut found.modules.clone())))
            .collect();
        let mut rows = Vec::new();
        let section = |rows: &mut Vec<Row>,
                       title: &str,
                       which: Section,
                       mut picks: Vec<(usize, RowState)>,
                       order: fn(&(usize, RowState)) -> Reverse<u32>| {
            if picks.is_empty() {
                return;
            }
            picks.sort_by_key(order);
            rows.push(Row::heading(title, picks.len()));
            for (index, state) in picks.iter().take(LISTED_SHOWN) {
                rows.extend(self.listed(&items[*index], which, 0, *state).map(Row::Item));
            }
            if picks.len() > LISTED_SHOWN {
                rows.push(Row::Note(
                    format!("{} more · type to narrow", picks.len() - LISTED_SHOWN).into(),
                ));
            }
        };
        let yours: Vec<_> = states
            .iter()
            .filter(|(_, state)| state.uses.is_some_and(|uses| uses > 0))
            .copied()
            .collect();
        section(&mut rows, "Yours", Section::Yours, yours, |(_, state)| {
            Reverse(state.uses.unwrap_or(0))
        });
        let changes: Vec<_> = states
            .iter()
            .filter(|(_, state)| state.change != Change::Still)
            .copied()
            .collect();
        section(
            &mut rows,
            "Changes",
            Section::Changes,
            changes,
            |(_, state)| Reverse(u32::from(matches!(state.change, Change::Gone(_)))),
        );
        let mut any = false;
        for group in KindGroup::ALL {
            let members: Vec<&(usize, RowState)> = states
                .iter()
                .filter(|(index, _)| KindGroup::of(items[*index].node.decl.kind) == Some(group))
                .collect();
            if members.is_empty() {
                continue;
            }
            if !any {
                rows.push(Row::Heading(Heading {
                    words: "By kind".into(),
                    count: None,
                }));
                any = true;
            }
            let id = RowId::Group(group);
            let open = self.folds.is_open(&id, false);
            let mut head = Item::new(
                id.clone(),
                0,
                Mark::Kind(group.mark()),
                group.label(),
                Do::Fold(id),
            );
            head.fold = Some(Fold::of(open));
            head.trailing = Trailing::State(RowState {
                members: u32::try_from(members.len()).ok(),
                ..RowState::default()
            });
            rows.push(Row::Item(head));
            if open {
                for (index, state) in members.iter().take(KIND_SHOWN).map(|pair| **pair) {
                    rows.extend(
                        self.listed(&items[index], Section::Kind(group), 1, state)
                            .map(Row::Item),
                    );
                }
                if members.len() > KIND_SHOWN {
                    rows.push(Row::Note(
                        format!("{} more · type to narrow", members.len() - KIND_SHOWN).into(),
                    ));
                }
            }
        }
        if rows.is_empty() {
            rows.push(Row::Note(
                "No public names were listed for this package.".into(),
            ));
        }
        rows
    }

    /// One item listed away from its module.
    fn listed(
        &self,
        found: &Found<'_>,
        section: Section,
        depth: u8,
        state: RowState,
    ) -> Option<Item> {
        let symbol = found.node.decl.coordinate.clone();
        let route = symbol_route(self.package.as_str(), &symbol)?;
        let mut item = Item::new(
            RowId::Listed(section, symbol.clone()),
            depth,
            Mark::Kind(kind_of(found.node.decl.kind)),
            shelf_name(found.node),
            Do::Go(route),
        );
        item.sub =
            (!found.modules.is_empty()).then(|| SharedString::from(found.modules.join("::")));
        item.current = self.current == Some(&symbol);
        item.warm = Some(PageKey::Symbol(symbol.clone()));
        item.source = Some(symbol);
        if !self.book.is_empty() {
            item.path = Some(
                self.book.path_of(
                    found
                        .modules
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(found.node.decl.name.as_ref())),
                ),
            );
        }
        item.trailing = if state.is_quiet() {
            Trailing::Nothing
        } else {
            Trailing::State(state)
        };
        Some(item)
    }
}

/// An item of the API and the modules it is in.
struct Found<'a> {
    node: &'a OutlineNode,
    modules: Vec<String>,
}

/// Every item under `nodes` (a module's children are its items; nothing
/// inside an item is one: those are its members), test-only modules left
/// out.
fn collect<'a>(nodes: &'a [OutlineNode], modules: &mut Vec<String>, out: &mut Vec<Found<'a>>) {
    for node in nodes {
        if node.decl.kind == Some(DeclarationKind::Module) {
            if is_test_module(node) {
                continue;
            }
            let restore = enter(modules, node);
            collect(&node.children, modules, out);
            leave(modules, restore);
        } else if KindGroup::of(node.decl.kind).is_some() {
            // Outside any module of the walk, the name says which it is in.
            let modules = if modules.is_empty() {
                declared_in(node)
            } else {
                modules.clone()
            };
            out.push(Found { node, modules });
        }
    }
}

/// Whether `node` is a test-only module (`#[cfg(test)] mod browse_tests;`,
/// an inline `mod tests`). The index carries no `cfg` attributes, so this
/// is the tour's file rule (`facet::semantics::tour`'s `is_test`: `tests/`,
/// `benches/`, `examples/`, `*_test(s).rs`, `tests.rs`) read from the
/// module's own name and path.
pub(crate) fn is_test_module(node: &OutlineNode) -> bool {
    if node.decl.kind != Some(DeclarationKind::Module) {
        return false;
    }
    let stem = shelf_name(node);
    let stem = stem.rsplit("::").next().unwrap_or(&stem);
    let named =
        matches!(stem, "tests" | "test") || stem.ends_with("_tests") || stem.ends_with("_test");
    let placed = node.decl.path.as_deref().is_some_and(|path| {
        ["test/", "tests/", "benches/", "examples/"]
            .iter()
            .any(|dir| path.starts_with(dir) || path.contains(&format!("/{dir}")))
    });
    named || placed
}

/// A module row reads as its module (`glyph`), not its file (`glyph.rs`).
pub(crate) fn shelf_name(node: &OutlineNode) -> String {
    let name = node.decl.name.as_ref();
    if node.decl.kind == Some(DeclarationKind::Module)
        && let Some((stem, extension)) = name.rsplit_once('.')
        && matches!(
            extension,
            "rs" | "ts"
                | "tsx"
                | "js"
                | "py"
                | "go"
                | "java"
                | "cs"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
        )
        && !stem.is_empty()
    {
        return stem.to_owned();
    }
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pages::PackageDossier;
    use crate::shell::tests::{PACKAGE, dossier, symbol};

    fn package() -> PackageRef {
        PackageRef::parse(PACKAGE).expect("package")
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Item(item) => item.name.to_string(),
                Row::Heading(heading) => heading.words.to_uppercase(),
                Row::Note(words) => words.to_string(),
            })
            .collect()
    }

    fn tree(dossier: &PackageDossier) -> &OutlineTree {
        dossier.outline.known().expect("the fixture outline")
    }

    /// A compiler's rows carry more than the package declares: a function's
    /// parameters and locals, a type written out (`&str`), the `crate`
    /// keyword. The outline lists what a person would look up (seen on
    /// toml_pin's `read_settings` page: `read_settings` held `text`, and
    /// `&str` and `crate` stood beside it).
    #[test]
    fn a_functions_locals_a_written_type_and_a_path_keyword_are_not_in_the_outline() {
        use backend_library::DeclarationKind as K;
        let node = |name: &str, kind: K, children: Vec<OutlineNode>| OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(
                &format!("{PACKAGE}::lib.rs:16::{name}"),
                None,
                Some(kind),
                None,
            )
            .expect("decl"),
            children: std::sync::Arc::from(children),
        };
        let roots = vec![
            node("Settings", K::Struct, vec![node("table", K::Field, vec![])]),
            node(
                "read_settings",
                K::Function,
                vec![
                    node("text", K::Variable, vec![]),
                    node("read_settings", K::Variable, vec![]),
                ],
            ),
            node("&str", K::Type, vec![]),
            node("crate", K::Module, vec![]),
        ];
        let tree = OutlineTree {
            roots: std::sync::Arc::from(roots),
            complete: true,
        };
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let current = symbol("read_settings");
        let outline = Outline {
            package: &package,
            current: Some(&current),
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let listed = outline.package_rows(&tree);
        let shown = names(&listed.rows);
        assert!(
            shown.contains(&"Settings".to_owned()) && shown.contains(&"read_settings".to_owned()),
            "{shown:?}"
        );
        for gone in ["text", "&str", "crate"] {
            assert!(
                !shown.contains(&gone.to_owned()),
                "`{gone}` is not the package's to list: {shown:?}"
            );
        }
        assert_eq!(
            shown.iter().filter(|name| *name == "read_settings").count(),
            1,
            "the function once, not its own name inside it: {shown:?}"
        );
        let read = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.name.as_ref() == "read_settings")
            .expect("the function");
        assert_eq!(
            read.fold, None,
            "a function is a leaf of the outline: {shown:?}"
        );
    }

    #[test]
    fn an_import_is_what_a_module_brings_in_not_what_it_holds() {
        use backend_library::DeclarationKind as K;
        let node = |name: &str, kind: K, children: Vec<OutlineNode>| OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(
                &format!("{PACKAGE}::glyph.rs:138::{name}"),
                None,
                Some(kind),
                None,
            )
            .expect("decl"),
            children: std::sync::Arc::from(children),
        };
        let value = node(
            "value",
            K::Module,
            vec![
                node("BTreeMap", K::Import, vec![]),
                node("HashMap", K::Import, vec![]),
                node("Value", K::Enum, vec![node("fmt", K::Import, vec![])]),
                node("parse", K::Function, vec![]),
            ],
        );
        let tree = OutlineTree {
            roots: std::sync::Arc::from([value]),
            complete: true,
        };
        assert_eq!(
            tree_names(&tree),
            3,
            "the module, its enum and its function: an import is not a name"
        );
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let current = symbol("parse");
        let outline = Outline {
            package: &package,
            current: Some(&current),
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let listed = outline.package_rows(&tree);
        assert_eq!(
            names(&listed.rows),
            ["value", "Value", "parse"],
            "no `use` row under the module, and an enum holding only an import is not a group"
        );
        assert_eq!(
            listed.matched,
            Matched { shown: 3, of: 3 },
            "the count is what the list shows"
        );
        let value = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.name.as_ref() == "Value")
            .expect("Value");
        assert_eq!(value.fold, None);
        let outline = Outline {
            filter: Filter {
                query: "map",
                via: None,
            },
            ..outline
        };
        assert_eq!(
            names(&outline.package_rows(&tree).rows),
            Vec::<String>::new(),
            "an import is not searched either"
        );
    }

    #[test]
    fn one_file_name_in_three_directories_is_three_modules_told_apart_by_their_paths() {
        use backend_library::DeclarationKind as K;
        let file = |path: &str| OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(
                &format!(
                    "{PACKAGE}::{path}:1::{}",
                    path.rsplit('/').next().unwrap_or(path)
                ),
                None,
                Some(K::Module),
                None,
            )
            .expect("decl"),
            children: std::sync::Arc::from([]),
        };
        assert_eq!(
            label(&file("src/de/map.rs")),
            ("map".to_owned(), Some("de".to_owned()))
        );
        assert_eq!(
            label(&file("src/ser/map.rs")),
            ("map".to_owned(), Some("ser".to_owned()))
        );
        assert_eq!(
            label(&file("src/edit/de/map.rs")),
            ("map".to_owned(), Some("edit::de".to_owned())),
            "the whole path, quietly"
        );
        assert_eq!(
            label(&file("src/de/mod.rs")),
            ("de".to_owned(), None),
            "a mod.rs is its directory"
        );
        assert_eq!(
            label(&file("crates/toml/src/edit/mod.rs")),
            ("edit".to_owned(), None),
            "where the package sits is not a module"
        );
        assert_eq!(label(&file("src/lib.rs")), ("lib".to_owned(), None));
        assert_eq!(
            file_module(&file("src/de/map.rs")),
            Some(vec!["de".to_owned(), "map".to_owned()]),
            "and the path its items are found under in the release data"
        );
        let inline = OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(
                &format!("{PACKAGE}::glyph.rs:1::inner"),
                None,
                Some(K::Module),
                None,
            )
            .expect("decl"),
            children: std::sync::Arc::from([]),
        };
        assert_eq!(
            label(&inline),
            ("inner".to_owned(), None),
            "a module that is not a file says only its name"
        );
    }

    #[test]
    fn a_name_the_index_places_on_its_own_says_the_module_its_file_is() {
        use backend_library::DeclarationKind as K;
        let name = |path: &str| OutlineNode {
            decl: crate::model::pages::DeclRef::from_label(
                &format!("{PACKAGE}::{path}:9::Deserializer"),
                None,
                Some(K::Struct),
                None,
            )
            .expect("decl"),
            children: std::sync::Arc::from([]),
        };
        assert_eq!(declared_in(&name("src/de/mod.rs")), ["de"]);
        assert_eq!(declared_in(&name("src/de/map.rs")), ["de", "map"]);
        assert_eq!(declared_in(&name("crates/toml/src/lib.rs")), ["lib"]);
        assert_eq!(
            declared_in(&name("glyph.rs")),
            ["glyph"],
            "a file at the root is its own module"
        );
        let tree = OutlineTree {
            roots: std::sync::Arc::from([name("src/de/map.rs")]),
            complete: true,
        };
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let outline = Outline {
            package: &package,
            current: None,
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let listed = outline.package_rows(&tree);
        let row = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .next()
            .expect("the name");
        assert_eq!(
            row.sub.as_deref(),
            Some("de::map"),
            "in the outline it says where it is"
        );
    }

    #[test]
    fn the_intro_lists_kind_families_and_never_a_module() {
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let about = dossier();
        let outline = Outline {
            package: &package,
            current: None,
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let rows = outline.intro_rows(tree(&about));
        assert_eq!(
            names(&rows),
            ["BY KIND", "Types", "Functions"],
            "five types and one function, no module, no test module: {rows:#?}"
        );
        let types = rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.name.as_ref() == "Types")
            .expect("the Types group");
        assert_eq!(types.fold, Some(Fold::Shut), "a family opens on demand");
        assert_eq!(types.does, Do::Fold(RowId::Group(KindGroup::Types)));
        assert_eq!(
            types.trailing,
            Trailing::State(RowState {
                members: Some(5),
                ..RowState::default()
            }),
            "how many names it holds, in quiet ink"
        );
    }

    #[test]
    fn an_open_family_lists_each_name_with_its_module_and_a_route_to_its_page() {
        let (package, book) = (package(), StateBook::none());
        let mut folds = Folds::default();
        folds.flip(RowId::Group(KindGroup::Functions));
        let about = dossier();
        let outline = Outline {
            package: &package,
            current: None,
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let rows = outline.intro_rows(tree(&about));
        let listed = rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.name.as_ref() == "relation_label")
            .expect("the function under its family");
        assert_eq!(
            listed.id,
            RowId::Listed(
                Section::Kind(KindGroup::Functions),
                symbol("relation_label")
            )
        );
        assert_eq!(listed.sub.as_deref(), Some("glyph"), "its module, quietly");
        assert_eq!(listed.depth, 1);
        assert_eq!(
            listed.does,
            Do::Go(symbol_route(PACKAGE, &symbol("relation_label")).expect("route"))
        );
        assert_eq!(
            listed.source,
            Some(symbol("relation_label")),
            "S peels it, and a click hands its name to its title"
        );
    }

    #[test]
    fn the_outline_opens_the_module_of_the_page_and_folds_test_modules_into_one_row() {
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let about = dossier();
        let current = symbol("RelationLabel");
        let outline = Outline {
            package: &package,
            current: Some(&current),
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let listed = outline.package_rows(tree(&about));
        assert_eq!(
            names(&listed.rows),
            [
                "identity",
                "glyph",
                "RelationLabel",
                "RelationDirection",
                "KindGlyph",
                "relation_label",
                "outline",
                "tests"
            ]
        );
        let glyph = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.name.as_ref() == "glyph")
            .expect("glyph");
        assert_eq!(glyph.fold, Some(Fold::Open));
        assert!(
            matches!(glyph.hoists, Some(Scope::Node(_))),
            "→ scopes into it"
        );
        assert!(
            matches!(glyph.does, Do::Fold(_)),
            "a module row folds in place; it is not a page"
        );
        let here = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .find(|item| item.current)
            .expect("the page's own row");
        assert_eq!(here.name.as_ref(), "RelationLabel");
        assert_eq!(
            listed.matched,
            Matched { shown: 11, of: 11 },
            "nothing narrows"
        );
    }

    #[test]
    fn narrowing_searches_folded_modules_keeps_each_matchs_parents_and_counts_what_matched() {
        let (package, folds, book) = (package(), Folds::default(), StateBook::none());
        let about = dossier();
        let outline = Outline {
            package: &package,
            current: None,
            folds: &folds,
            book: &book,
            filter: Filter {
                query: "outl",
                via: None,
            },
        };
        let listed = outline.package_rows(tree(&about));
        assert_eq!(
            names(&listed.rows),
            ["outline", "Outline"],
            "the module (a match itself) and the struct inside it, though nothing was open"
        );
        assert_eq!(listed.matched, Matched { shown: 2, of: 11 });
        let item = listed
            .rows
            .iter()
            .filter_map(Row::item)
            .nth(1)
            .expect("the struct");
        assert_eq!(item.hit, Some(0..4), "the words to underline");
        let outline = Outline {
            filter: Filter {
                query: "folds",
                via: None,
            },
            ..outline
        };
        let listed = outline.package_rows(tree(&about));
        assert_eq!(
            names(&listed.rows),
            ["glyph_tests", "folds_rows"],
            "a test module is searched too, as its own parent"
        );
    }

    #[test]
    fn a_hoisted_node_lists_its_children_and_a_type_lists_its_members_used_ones_first() {
        let (package, folds) = (package(), Folds::default());
        let about = dossier();
        let glyph = tree(&about)
            .roots
            .iter()
            .find(|node| shelf_name(node) == "glyph")
            .expect("glyph");
        let book = StateBook::none();
        let outline = Outline {
            package: &package,
            current: None,
            folds: &folds,
            book: &book,
            filter: Filter::default(),
        };
        let listed = outline.node_rows(glyph, &[]);
        assert_eq!(
            names(&listed.rows),
            [
                "RelationLabel",
                "RelationDirection",
                "KindGlyph",
                "relation_label"
            ],
            "outline order when nothing is used"
        );
        assert_eq!(listed.matched, Matched { shown: 4, of: 4 });
        assert!(
            listed
                .rows
                .iter()
                .filter_map(Row::item)
                .all(|item| item.depth == 0),
            "the hoisted scope's children start at the left edge"
        );
    }
}
