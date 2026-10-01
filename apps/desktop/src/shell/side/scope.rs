//! Scope (`v6/cohesion/COHESION.md`, "The sidebar: primitives", 1): the
//! sidebar shows ONE place at a time: the Library, a package, or one node of
//! a package's outline (a module, a type). The path lives in the jump bar;
//! the sidebar shows only the way out and the scope's own title.
//!
//! Two things move a scope, and only one of them moves the reader:
//!
//! - **Following.** The reader went somewhere: the sidebar re-scopes to
//!   where the reader is (a package page or one of its declarations: the
//!   package; anything else: the Library), unless the reader only moved
//!   inside what you hoisted.
//! - **Hoisting.** `→` or a double-click on a row with children scopes the
//!   sidebar into it; `←` or the step-out pops. Browsing is not
//!   navigating: the reader does not move.

use crate::model::pages::{OutlineNode, OutlineTree, PackageRef, SymbolRef};
use crate::navigation::Route;
use crate::runtime::store::route_package;
use crate::shell::jump::route_symbol;

/// A place the sidebar can show.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Scope {
    /// Your projects and every package in the library.
    Library,
    /// One package, at the release being read.
    Package(PackageRef),
    /// One node of the outline of the package above it: its children.
    Node(SymbolRef),
}

/// The path from the Library down to the place the sidebar shows, and how
/// far down it is showing: stepping out moves the showing up the path
/// without forgetting it, hoisting moves it down (and forgets what was
/// below).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Crumbs {
    path: Vec<Scope>,
    at: usize,
}

impl Crumbs {
    /// Where a reader on `route` is: the package it reads, or the Library.
    pub(crate) fn following(route: &Route) -> Self {
        let mut path = vec![Scope::Library];
        if let Some(package) = route_package(route) {
            path.push(Scope::Package(package));
        }
        let at = path.len() - 1;
        Self { path, at }
    }

    /// The scope on show.
    pub(crate) fn shown(&self) -> &Scope {
        &self.path[self.at]
    }

    /// The scope one step out, when there is one.
    pub(crate) fn up(&self) -> Option<&Scope> {
        self.at.checked_sub(1).map(|at| &self.path[at])
    }

    /// The package on the path down to what is shown (or being read).
    pub(crate) fn package(&self) -> Option<&PackageRef> {
        self.path[..=self.at]
            .iter()
            .rev()
            .find_map(|scope| match scope {
                Scope::Package(package) => Some(package),
                Scope::Library | Scope::Node(_) => None,
            })
    }

    /// Whether a node is what is shown.
    pub(crate) fn is_hoisted(&self) -> bool {
        matches!(self.shown(), Scope::Node(_))
    }

    /// Scopes into `scope`, below what is shown. What was below the shown
    /// scope on the old path is forgotten.
    pub(crate) fn hoist(&mut self, scope: Scope) {
        if self.shown() == &scope {
            return;
        }
        self.path.truncate(self.at + 1);
        self.path.push(scope);
        self.at += 1;
    }

    /// Steps out one scope. Returns whether there was one to step out to.
    pub(crate) fn step_out(&mut self) -> bool {
        match self.at.checked_sub(1) {
            Some(at) => {
                self.at = at;
                true
            }
            None => false,
        }
    }

    /// The reader moved to a new place: the sidebar follows it, keeping what
    /// is hoisted when the reader only went to a declaration inside it.
    pub(crate) fn follow(&mut self, route: &Route, tree: Option<&OutlineTree>) {
        let fresh = Self::following(route);
        let inside_hoist = match (
            self.shown(),
            self.path.get(1),
            fresh.path.get(1),
            route_symbol(route),
            tree,
        ) {
            (
                Scope::Node(node),
                Some(Scope::Package(old)),
                Some(Scope::Package(new)),
                Some(symbol),
                Some(tree),
            ) => book_of(old) == book_of(new) && inside(tree, node, &symbol),
            _ => false,
        };
        if inside_hoist {
            self.retarget(route);
        } else {
            *self = fresh;
        }
    }

    /// The reader is on another release of the same book: the package on
    /// the path is that release now (what is shown stays shown).
    pub(crate) fn retarget(&mut self, route: &Route) {
        if let (Some(slot @ Scope::Package(_)), Some(package)) =
            (self.path.get_mut(1), route_package(route))
        {
            *slot = Scope::Package(package);
        }
    }
}

/// The package without its release: reading another release of a book is
/// still the same book.
pub(super) fn book_of(package: &PackageRef) -> &str {
    let text = package.as_str();
    match text.rfind('@') {
        Some(at) if !package.is_local() => &text[..at],
        _ => text,
    }
}

/// Whether `symbol` is `node` or somewhere below it in `tree`.
fn inside(tree: &OutlineTree, node: &SymbolRef, symbol: &SymbolRef) -> bool {
    locate(tree, node).is_some_and(|found| contains(found.node, symbol))
}

/// Whether `node`'s subtree holds `symbol`.
pub(crate) fn contains(node: &OutlineNode, symbol: &SymbolRef) -> bool {
    &node.decl.coordinate == symbol || node.children.iter().any(|child| contains(child, symbol))
}

/// A node found in an outline, with the nodes above it.
pub(crate) struct Located<'a> {
    /// The node.
    pub node: &'a OutlineNode,
    /// Its ancestors, outermost first.
    pub ancestors: Vec<&'a OutlineNode>,
}

/// The node of `tree` that is `symbol`, and the nodes above it.
pub(crate) fn locate<'a>(tree: &'a OutlineTree, symbol: &SymbolRef) -> Option<Located<'a>> {
    fn walk<'a>(
        nodes: &'a [OutlineNode],
        symbol: &SymbolRef,
        above: &mut Vec<&'a OutlineNode>,
    ) -> Option<&'a OutlineNode> {
        for node in nodes {
            if &node.decl.coordinate == symbol {
                return Some(node);
            }
            above.push(node);
            if let Some(found) = walk(&node.children, symbol, above) {
                return Some(found);
            }
            above.pop();
        }
        None
    }
    let mut ancestors = Vec::new();
    let node = walk(&tree.roots, symbol, &mut ancestors)?;
    Some(Located { node, ancestors })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PackageId;
    use crate::navigation::{Coordinate, OrbitRoute, PackageLane, PackageRoute, SymbolRoute, View};

    const BOOK: &str = "pkg:cargo/toml@0.8.23";

    fn package(at: Option<&str>) -> Route {
        Route::Package(PackageRoute {
            project: None,
            package: PackageId::new(BOOK).expect("package"),
            lane: PackageLane::Overview,
            selected: None,
            at: at.map(|at| crate::navigation::ReleaseId::new(at).expect("release")),
        })
    }

    fn symbol_route(name: &str) -> Route {
        Route::Symbol(SymbolRoute {
            project: None,
            package: PackageId::new(BOOK).expect("package"),
            id: Coordinate::new(&format!("{BOOK}::value.rs:3::{name}")).expect("coordinate"),
            at: None,
            view: View::Page,
            line: None,
            selected: None,
        })
    }

    fn node(name: &str) -> SymbolRef {
        SymbolRef::new(&format!("{BOOK}::value.rs:3::{name}")).expect("symbol")
    }

    #[test]
    fn the_reader_on_a_package_or_its_declaration_scopes_the_sidebar_to_the_package_and_elsewhere_to_the_library()
     {
        let on_package = Crumbs::following(&package(None));
        assert!(matches!(on_package.shown(), Scope::Package(p) if p.display_name() == "toml"));
        assert_eq!(
            on_package.up(),
            Some(&Scope::Library),
            "one step out is the Library"
        );
        let on_symbol = Crumbs::following(&symbol_route("Value"));
        assert!(matches!(on_symbol.shown(), Scope::Package(_)));
        let library = Crumbs::following(&Route::Orbit(OrbitRoute::Home));
        assert_eq!(library.shown(), &Scope::Library);
        assert_eq!(library.up(), None, "nothing is above the Library");
    }

    #[test]
    fn stepping_out_shows_the_library_without_forgetting_the_package_and_hoisting_forgets_what_was_below()
     {
        let mut crumbs = Crumbs::following(&package(None));
        assert!(crumbs.step_out());
        assert_eq!(crumbs.shown(), &Scope::Library);
        assert!(!crumbs.step_out(), "the Library is the top");
        crumbs.hoist(Scope::Node(node("Value")));
        assert!(crumbs.is_hoisted());
        assert_eq!(
            crumbs.up(),
            Some(&Scope::Library),
            "hoisting from the Library forgot the package below it"
        );
        let mut crumbs = Crumbs::following(&package(None));
        crumbs.hoist(Scope::Node(node("value")));
        crumbs.hoist(Scope::Node(node("Value")));
        assert!(crumbs.step_out());
        assert_eq!(crumbs.shown(), &Scope::Node(node("value")));
        assert!(
            crumbs.package().is_some(),
            "the package is still on the path"
        );
    }

    #[test]
    fn a_hoist_survives_the_reader_moving_inside_it_and_is_dropped_when_it_leaves() {
        let tree = OutlineTree {
            roots: std::sync::Arc::from([OutlineNode {
                decl: crate::model::pages::DeclRef::from_label(
                    &format!("{BOOK}::value.rs:3::Value"),
                    None,
                    Some(backend_library::DeclarationKind::Enum),
                    None,
                )
                .expect("decl"),
                children: std::sync::Arc::from([OutlineNode {
                    decl: crate::model::pages::DeclRef::from_label(
                        &format!("{BOOK}::value.rs:3::as_str"),
                        None,
                        Some(backend_library::DeclarationKind::Method),
                        None,
                    )
                    .expect("decl"),
                    children: std::sync::Arc::from([]),
                }]),
            }]),
            complete: true,
        };
        let mut crumbs = Crumbs::following(&package(None));
        crumbs.hoist(Scope::Node(node("Value")));
        crumbs.follow(&symbol_route("as_str"), Some(&tree));
        assert_eq!(
            crumbs.shown(),
            &Scope::Node(node("Value")),
            "opening a member keeps you inside the type"
        );
        crumbs.follow(&symbol_route("Elsewhere"), Some(&tree));
        assert!(
            matches!(crumbs.shown(), Scope::Package(_)),
            "a declaration outside it re-scopes to the package"
        );
        crumbs.hoist(Scope::Node(node("Value")));
        crumbs.follow(&package(Some("1.1.6")), Some(&tree));
        assert!(
            !crumbs.is_hoisted(),
            "a package page has no declaration inside the hoist"
        );
    }

    #[test]
    fn reading_another_release_of_the_book_leaves_what_is_shown_and_points_the_package_at_it() {
        let mut crumbs = Crumbs::following(&package(None));
        crumbs.step_out();
        crumbs.retarget(&package(Some("1.1.6")));
        assert_eq!(crumbs.shown(), &Scope::Library, "the Library stays on show");
        assert!(
            matches!(crumbs.path[1], Scope::Package(ref p) if p.version() == Some("1.1.6")),
            "and the package below it is the release now read"
        );
        let mut crumbs = Crumbs::following(&package(None));
        crumbs.step_out();
        crumbs.follow(&symbol_route("Value"), None);
        assert!(
            matches!(crumbs.shown(), Scope::Package(_)),
            "a new place re-scopes to where the reader is"
        );
    }
}
