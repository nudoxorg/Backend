//! The jump bar's words (D-Hand): where you are as segments (package ›
//! module › declaration), each opening its siblings from the package
//! outline; the here line for places that are not declarations; and the
//! mono `nudox://` address (⌘⇧C copies it; hovering the bar shows it).
//! History never shows at rest: back and forward are ⌘[ / ⌘], and a long
//! press on back lists the last ten places.

use super::kit::kind_of;
use crate::model::pages::{OutlineNode, PackageRef, SymbolRef};
use crate::model::AppSnapshot;
use crate::navigation::{OrbitRoute, Overlay, Route, SettingsPage};
use crate::runtime::store::DataStore;
use facet::icons::Kind;
use gpui::SharedString;

/// What a bead or the capsule shows as its mark.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mark {
    /// A declaration or package kind.
    Kind(Kind),
    /// The orbit.
    Orbit,
    /// A settings or inbox place.
    Place,
}

/// The here capsule.
#[derive(Clone, Debug)]
pub(crate) struct Here {
    /// Its mark.
    pub mark: Mark,
    /// Its name.
    pub name: SharedString,
    /// The breadcrumb after the name (`present › glyph`).
    pub path: SharedString,
}

pub(crate) use crate::runtime::store::{route_package, route_symbol};

/// The capsule for the current place: a transient place (settings, inbox)
/// names itself; otherwise the route does.
pub(crate) fn here(snapshot: &AppSnapshot, store: &DataStore) -> Here {
    match snapshot.overlay() {
        Some(Overlay::Settings(page)) => Here {
            mark: Mark::Place,
            name: "Settings".into(),
            path: settings_name(page).into(),
        },
        Some(Overlay::Inbox) => Here {
            mark: Mark::Place,
            name: "Inbox".into(),
            path: "followed releases".into(),
        },
        _ => {
            if let Some(focus) = store.graph_focus() {
                return Here { mark: Mark::Kind(kind_of(Some(focus.kind))), name: focus.name.to_string().into(), path: focus.caption_path().into() };
            }
            let mut here = route_here(snapshot.route(), store);
            // Viewing another release: the capsule says which, and which you
            // pin, in the comb's words.
            if let Some(at) = snapshot.route().at() {
                let pinned = pinned_release(snapshot.route(), store);
                here.path = viewing(at.as_str(), pinned.as_deref()).into();
            }
            here
        }
    }
}

/// "viewing 0.3.0 · you pin 0.4.2", or, for a workspace crate read from
/// its checkout, "viewing 0.3.0 · yours is the working copy".
fn viewing(at: &str, pinned: Option<&str>) -> String {
    match pinned {
        Some(pinned) => format!("viewing {at} · you pin {pinned}"),
        None => format!("viewing {at} · yours is the working copy"),
    }
}

/// The release you pin for the route's package: the one its release list
/// marks current (what the comb marks mint), else the purl's own version;
/// `None` for a crate read from its working copy.
fn pinned_release(route: &Route, store: &DataStore) -> Option<String> {
    let package = match route {
        Route::Package(route) => PackageRef::parse(route.package.as_str()).ok()?,
        Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok()?,
        Route::Orbit(_) | Route::World => return None,
    };
    let current = store.package(&package).loaded_value().and_then(|dossier| {
        dossier
            .versions
            .known()
            .and_then(|versions| versions.iter().find(|entry| entry.current).map(|entry| entry.version.to_string()))
    });
    current.or_else(|| package.release_version().map(ToOwned::to_owned))
}

/// One segment of the jump bar: its words and where it leads.
#[derive(Clone, Debug)]
pub(crate) struct Segment {
    /// Its name (`present`, `glyph`, `RelationLabel`).
    pub name: SharedString,
    /// Where choosing it goes (its page), when it has one.
    pub route: Option<Route>,
    /// A name only: no page and no siblings to list (a graph focus the
    /// index has no row for). Drawn quiet, and inert.
    pub quiet: bool,
}

/// The place the jump bar describes: the graph's focus while it has one
/// (an indexed focus reads as its own page, so its segments open pages and
/// list siblings); otherwise the route.
pub(crate) fn bar_route(snapshot: &AppSnapshot, store: &DataStore) -> Route {
    store
        .graph_focus()
        .and_then(|focus| focus.indexed.as_ref())
        .and_then(|(package, symbol)| super::kit::symbol_route(package.as_str(), symbol))
        .unwrap_or_else(|| snapshot.route().clone())
}

/// The jump bar's segments: those of [`bar_route`]; a graph focus the index
/// has no row for reads package › module › name, names only (never a
/// guessed page).
pub(crate) fn bar_segments(snapshot: &AppSnapshot, store: &DataStore) -> Vec<Segment> {
    if let Some(focus) = store.graph_focus()
        && focus.indexed.is_none()
    {
        return [focus.package.as_ref(), focus.module.as_ref(), focus.name.as_ref()]
            .into_iter()
            .filter(|name| !name.is_empty())
            .map(|name| Segment { name: name.to_owned().into(), route: None, quiet: true })
            .collect();
    }
    segments(&bar_route(snapshot, store), store)
}

/// The segments for a declaration route: its package, then each module and
/// owner on the way down, then the declaration itself (the last).
pub(crate) fn segments(route: &Route, store: &DataStore) -> Vec<Segment> {
    match route {
        Route::Symbol(symbol) => {
            let Ok(package) = PackageRef::parse(symbol.package.as_str()) else { return Vec::new() };
            let identity = backend_present::Identity::parse(symbol.id.as_str());
            let mut out = vec![Segment {
                name: package.display_name().to_owned().into(),
                route: super::kit::package_route(&package),
                quiet: false,
            }];
            // Modules and owners, found in the outline by name so each
            // opens its own page.
            let dossier = store.package(&package);
            let tree = dossier.loaded_value().and_then(|dossier| dossier.outline.known().cloned());
            let mut level: Option<&[OutlineNode]> = tree.as_ref().map(|tree| &tree.roots[..]);
            for name in crumbs(&identity).into_iter().skip(usize::from(identity.project().is_some())) {
                let node = level.and_then(|nodes| nodes.iter().find(|node| super::shelf::shelf_name(node) == name));
                out.push(Segment {
                    name: name.into(),
                    route: node.and_then(|node| super::kit::symbol_route(package.as_str(), &node.decl.coordinate)),
                    quiet: false,
                });
                level = node.map(|node| &node.children[..]);
            }
            out.push(Segment { name: identity.name().to_owned().into(), route: Some(route.clone()), quiet: false });
            out
        }
        Route::Package(package) => PackageRef::parse(package.package.as_str())
            .map(|package| vec![Segment { name: package.display_name().to_owned().into(), route: Some(route.clone()), quiet: false }])
            .unwrap_or_default(),
        Route::Orbit(_) | Route::World => Vec::new(),
    }
}

/// What a segment's menu lists: the entries of the outline level it sits
/// at, the real ones first; test-only modules are folded into one trailing
/// "tests" row (the shelf's rule, [`super::shelf::is_test_module`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct Siblings {
    /// The level's real entries, in outline order.
    pub real: Vec<Segment>,
    /// Its test-only modules, in outline order.
    pub tests: Vec<Segment>,
}

impl Siblings {
    /// Nothing to list.
    pub(crate) fn is_empty(&self) -> bool {
        self.real.is_empty() && self.tests.is_empty()
    }
}

/// The siblings of segment `index` (what the menu under it lists), each
/// with its page.
pub(crate) fn siblings(route: &Route, index: usize, store: &DataStore) -> Siblings {
    let Route::Symbol(symbol) = route else { return Siblings::default() };
    let Ok(package) = PackageRef::parse(symbol.package.as_str()) else { return Siblings::default() };
    let identity = backend_present::Identity::parse(symbol.id.as_str());
    let dossier = store.package(&package);
    let Some(tree) = dossier.loaded_value().and_then(|dossier| dossier.outline.known().cloned()) else { return Siblings::default() };
    if index == 0 {
        return Siblings::default();
    }
    let path: Vec<String> = crumbs(&identity).into_iter().skip(usize::from(identity.project().is_some())).collect();
    let mut level: &[OutlineNode] = &tree.roots;
    for name in path.iter().take(index - 1) {
        match level.iter().find(|node| super::shelf::shelf_name(node) == name.as_str()) {
            Some(node) => level = &node.children,
            None => return Siblings::default(),
        }
    }
    let segment = |node: &OutlineNode| Segment {
        name: super::shelf::shelf_name(node).into(),
        route: super::kit::symbol_route(package.as_str(), &node.decl.coordinate),
        quiet: false,
    };
    let (tests, real): (Vec<&OutlineNode>, Vec<&OutlineNode>) = level.iter().partition(|node| super::shelf::is_test_module(node));
    Siblings {
        real: real.into_iter().map(segment).collect(),
        tests: tests.into_iter().map(segment).collect(),
    }
}

/// A settings page's name.
pub(crate) const fn settings_name(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::Appearance => "Appearance",
        SettingsPage::Editor => "Editor",
        SettingsPage::Agents => "Agents",
        SettingsPage::Connections => "Connections",
        SettingsPage::Privacy => "Privacy",
        SettingsPage::Diagnostics => "Diagnostics",
        SettingsPage::Index => "Index & registries",
        SettingsPage::Registry => "Registries",
        SettingsPage::Legend => "Legend",
        SettingsPage::Help => "Keys",
    }
}

fn route_here(route: &Route, store: &DataStore) -> Here {
    match route {
        Route::Orbit(OrbitRoute::Home) => Here {
            mark: Mark::Orbit,
            name: "Orbit".into(),
            path: "everything".into(),
        },
        Route::Orbit(OrbitRoute::Project(_)) => Here {
            mark: Mark::Orbit,
            name: "Project".into(),
            path: "orbit".into(),
        },
        Route::Orbit(OrbitRoute::Browse(browse)) => {
            let (name, path) = browse.here();
            Here { mark: Mark::Orbit, name: name.into(), path: path.into() }
        }
        Route::World => Here {
            mark: Mark::Orbit,
            name: "Graph".into(),
            path: "everything".into(),
        },
        Route::Package(route) => {
            let package = PackageRef::parse(route.package.as_str()).ok();
            Here {
                mark: Mark::Kind(Kind::Package),
                name: package
                    .as_ref()
                    .map_or_else(|| route.package.as_str().to_owned(), |package| package.display_name().to_owned())
                    .into(),
                path: package
                    .as_ref()
                    .map_or("package", |package| if package.is_local() { "local project" } else { "registry" })
                    .into(),
            }
        }
        Route::Symbol(route) => symbol_here(route.id.as_str(), store, route.view),
    }
}

fn symbol_here(coordinate: &str, store: &DataStore, view: crate::navigation::View) -> Here {
    let Ok(symbol) = SymbolRef::new(coordinate) else {
        return Here {
            mark: Mark::Kind(Kind::Unknown),
            name: coordinate.to_owned().into(),
            path: SharedString::default(),
        };
    };
    let identity = symbol.identity();
    let kind = store
        .symbol(&symbol)
        .loaded_value()
        .map(|page| kind_of(page.identity.kind))
        .unwrap_or(Kind::Unknown);
    let mut path = crumbs(&identity);
    match view {
        crate::navigation::View::Page => {}
        crate::navigation::View::Code => path.insert(0, "code".to_owned()),
        crate::navigation::View::Graph => path.insert(0, "graph".to_owned()),
    }
    Here {
        mark: Mark::Kind(kind),
        name: identity.name().to_owned().into(),
        path: path.join(" › ").into(),
    }
}

/// `present › glyph` for `…/present::glyph.rs:138::RelationLabel`.
fn crumbs(identity: &backend_present::Identity) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(project) = identity.project() {
        parts.push(project.name().to_owned());
    }
    if let Some(path) = identity.path() {
        let stem = path.stem();
        if !stem.is_empty() && stem != "lib" && stem != "mod" && stem != "main" {
            parts.push(stem.to_owned());
        }
    }
    let segments = identity.trail().segments();
    if segments.len() > 1 {
        for segment in &segments[..segments.len() - 1] {
            parts.push(segment.as_str().to_owned());
        }
    }
    parts
}

/// The mono address in the status bar (`nudox://present/glyph/RelationLabel`),
/// split where it may give way: the path (which can be cut from the left)
/// and the name with its view and release (which never is).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Address {
    /// Path segments after the scheme, outermost first.
    pub path: Vec<String>,
    /// The place's own name, then its view and release (`RelationLabel/code@0.3.0`).
    pub name: String,
}

impl Address {
    /// The whole address.
    pub(crate) fn full(&self) -> String {
        let mut full = "nudox://".to_owned();
        for segment in &self.path {
            full.push_str(segment);
            full.push('/');
        }
        full.push_str(&self.name);
        full
    }

    /// The address with all but the last `keep` path segments cut from the
    /// left (`…/glyph/RelationLabel`).
    pub(crate) fn keeping(&self, keep: usize) -> String {
        let keep = keep.min(self.path.len());
        let mut cut = "…/".to_owned();
        for segment in &self.path[self.path.len() - keep..] {
            cut.push_str(segment);
            cut.push('/');
        }
        cut.push_str(&self.name);
        cut
    }
}

/// The current place's [`Address`].
pub(crate) fn address_parts(snapshot: &AppSnapshot) -> Address {
    let place = |path: &[&str], name: String| Address {
        path: path.iter().map(|segment| (*segment).to_owned()).collect(),
        name,
    };
    if let Some(Overlay::Settings(page)) = snapshot.overlay() {
        return place(&["settings"], page.as_str().to_owned());
    }
    if snapshot.overlay() == Some(Overlay::Inbox) {
        return place(&[], "inbox".to_owned());
    }
    match snapshot.route() {
        Route::Orbit(OrbitRoute::Browse(browse)) => {
            let (path, name) = browse.address();
            Address { path, name }
        }
        Route::Orbit(_) => place(&[], "orbit".to_owned()),
        Route::World => place(&[], "graph".to_owned()),
        Route::Package(route) => place(
            &[],
            PackageRef::parse(route.package.as_str())
                .map_or_else(|_| route.package.as_str().to_owned(), |package| package.display_name().to_owned()),
        ),
        Route::Symbol(route) => {
            let mut address = symbol_parts(route.id.as_str());
            match route.view {
                crate::navigation::View::Page => {}
                crate::navigation::View::Code => {
                    address.name.push_str("/code");
                    if let Some(line) = route.line {
                        address.name.push_str(&format!("#L{line}"));
                    }
                }
                crate::navigation::View::Graph => address.name.push_str("/graph"),
            }
            if let Some(at) = &route.at {
                address.name.push_str(&format!("@{}", at.as_str()));
            }
            address
        }
    }
}

fn symbol_parts(coordinate: &str) -> Address {
    let identity = backend_present::Identity::parse(coordinate);
    Address {
        path: crumbs(&identity),
        name: identity.name().to_owned(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewing_another_release_names_the_one_you_pin_as_the_comb_does() {
        assert_eq!(viewing("0.3.0", Some("0.4.2")), "viewing 0.3.0 · you pin 0.4.2");
        assert_eq!(viewing("0.3.0", None), "viewing 0.3.0 · yours is the working copy");
    }

    #[test]
    fn a_declaration_reads_as_package_module_name() {
        let address = symbol_parts("/repo/crates/present::glyph.rs:138::RelationLabel");
        assert_eq!(address.full(), "nudox://present/glyph/RelationLabel");
        assert_eq!(address.keeping(1), "…/glyph/RelationLabel");
        assert_eq!(address.keeping(0), "…/RelationLabel");
        let identity = backend_present::Identity::parse("/repo/crates/present::glyph.rs:138::RelationLabel");
        assert_eq!(crumbs(&identity).join(" › "), "present › glyph");
    }
}
