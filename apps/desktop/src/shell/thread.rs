//! The thread: history drawn as beads, the here capsule, and the mono
//! address — all derived from the session and whatever pages already landed.

use super::kit::kind_of;
use crate::model::pages::{PackageRef, PageKey, SymbolRef};
use crate::model::{AppSnapshot, SessionState};
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
    current.or_else(|| package.version().map(ToOwned::to_owned))
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
