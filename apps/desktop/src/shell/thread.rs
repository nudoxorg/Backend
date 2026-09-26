//! The thread: history drawn as beads, the here capsule, and the mono
//! address — all derived from the session and whatever pages already landed.

use super::kit::kind_of;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::model::{AppSnapshot, SessionState};
use crate::navigation::{OrbitRoute, Overlay, Route, RouteDepth, SettingsPage};
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

/// One step of history.
#[derive(Clone, Debug)]
pub(crate) struct Bead {
    /// Its mark.
    pub mark: Mark,
    /// Its name (tooltip and hint label).
    pub label: SharedString,
    /// How many steps back (negative) or forward (positive) it is.
    pub steps: isize,
}

/// The thread around "here".
#[derive(Clone, Debug, Default)]
pub(crate) struct Thread {
    /// Beads behind, oldest first.
    pub behind: Vec<Bead>,
    /// Beads ahead, nearest first.
    pub ahead: Vec<Bead>,
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

/// How far back and ahead the thread reaches.
const BEHIND: usize = 5;
const AHEAD: usize = 2;

/// The thread for a session.
pub(crate) fn thread(session: &SessionState, store: &DataStore) -> Thread {
    let behind = session
        .back
        .to_vec()
        .into_iter()
        .take(BEHIND)
        .enumerate()
        .map(|(index, route)| bead(&route, -(index as isize) - 1, store))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let ahead = session
        .forward
        .to_vec()
        .into_iter()
        .take(AHEAD)
        .enumerate()
        .map(|(index, route)| bead(&route, index as isize + 1, store))
        .collect();
    Thread { behind, ahead }
}

fn bead(route: &Route, steps: isize, store: &DataStore) -> Bead {
    let here = route_here(route, store);
    Bead {
        mark: here.mark,
        label: here.name,
        steps,
    }
}

/// The page keys the thread's marks read (so a bead's gem appears when its
/// page lands).
pub(crate) fn thread_keys(session: &SessionState) -> Vec<PageKey> {
    session
        .back
        .to_vec()
        .into_iter()
        .take(BEHIND)
        .chain(session.forward.to_vec().into_iter().take(AHEAD))
        .chain(std::iter::once(session.route.clone()))
        .filter_map(|route| route_symbol(&route).map(PageKey::Symbol))
        .collect()
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
            let mut here = route_here(snapshot.route(), store);
            // Viewing another release: the capsule says which, and which you pin.
            if let Some(at) = snapshot.route().at() {
                let pinned = match snapshot.route() {
                    Route::Package(route) => PackageRef::parse(route.package.as_str()).ok(),
                    Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok(),
                    Route::Orbit(_) | Route::World => None,
                };
                let pinned = pinned
                    .as_ref()
                    .and_then(PackageRef::version)
                    .map_or_else(|| "your checkout".to_owned(), ToOwned::to_owned);
                here.path = format!("viewing {} · you pin {pinned}", at.as_str()).into();
            }
            here
        }
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

/// The mono address in the status bar: `nudox://present/glyph/RelationLabel`.
pub(crate) fn address(snapshot: &AppSnapshot) -> SharedString {
    if let Some(Overlay::Settings(page)) = snapshot.overlay() {
        return format!("nudox://settings/{}", page.as_str()).into();
    }
    if snapshot.overlay() == Some(Overlay::Inbox) {
        return "nudox://inbox".into();
    }
    match snapshot.route() {
        Route::Orbit(_) => "nudox://orbit".into(),
        Route::World => "nudox://graph".into(),
        Route::Package(route) => {
            let name = PackageRef::parse(route.package.as_str())
                .map_or_else(|_| route.package.as_str().to_owned(), |package| package.display_name().to_owned());
            format!("nudox://{name}").into()
        }
        Route::Symbol(route) => {
            let mut address = symbol_address(route.id.as_str(), None).to_string();
            match route.view {
                crate::navigation::View::Page => {}
                crate::navigation::View::Code => {
                    address.push_str("/code");
                    if let Some(line) = route.line {
                        address.push_str(&format!("#L{line}"));
                    }
                }
                crate::navigation::View::Graph => address.push_str("/graph"),
            }
            if let Some(at) = &route.at {
                address.push_str(&format!("@{}", at.as_str()));
            }
            address.into()
        }
    }
}

fn symbol_address(coordinate: &str, line: Option<u32>) -> SharedString {
    let identity = backend_present::Identity::parse(coordinate);
    let mut parts = crumbs(&identity);
    parts.push(identity.name().to_owned());
    let mut address = format!("nudox://{}", parts.join("/"));
    if let Some(line) = line {
        address.push_str(&format!(":{line}"));
    }
    address.into()
}

/// The depth a place shows on the altimeter.
pub(crate) fn depth(snapshot: &AppSnapshot) -> RouteDepth {
    snapshot.route().depth().unwrap_or(RouteDepth::Orbit)
}

/// A depth's name.
pub(crate) const fn depth_name(depth: RouteDepth) -> &'static str {
    match depth {
        RouteDepth::Orbit => "Orbit",
        RouteDepth::Package => "Package",
        RouteDepth::Page => "Page",
        RouteDepth::Source => "Source",
    }
}

/// Every depth, shallowest first.
pub(crate) const DEPTHS: [RouteDepth; 4] = [
    RouteDepth::Orbit,
    RouteDepth::Package,
    RouteDepth::Page,
    RouteDepth::Source,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_reads_as_package_module_name() {
        let address = symbol_address("/repo/crates/present::glyph.rs:138::RelationLabel", None);
        assert_eq!(address.as_ref(), "nudox://present/glyph/RelationLabel");
        let identity = backend_present::Identity::parse("/repo/crates/present::glyph.rs:138::RelationLabel");
        assert_eq!(crumbs(&identity).join(" › "), "present › glyph");
    }
}
