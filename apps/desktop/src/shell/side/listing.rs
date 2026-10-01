//! What the sidebar lists, as a pure function of what the store holds: the
//! header (the way out, the scope's title, the lens strip's counts) and the
//! rows of the lens on the scope. No entity, no window: the rig asserts on
//! the words and the ids, and the view only paints them.
//!
//! **The contextual rule.** The sidebar lists what the reader is not
//! already listing:
//!
//! - the Library (the reader draws a card per package): packages regrouped
//!   by how they relate to you;
//! - a package's intro (the reader draws every module as a region): NOT the
//!   modules. Contents is the API by what matters to you ([`Outline::intro_rows`]);
//! - a declaration's page (the reader draws one declaration): the outline,
//!   following the page;
//! - a hoisted module or type: its own children.

use super::hold::{Chip, Step};
use super::lens::{Counts, Lens};
use super::narrow::{Filter, Matched};
use super::outline::{Listed, Outline};
use super::row::{Do, Folds, Heading, Item, Mark, ReleaseMark, Row, RowId, Trailing};
use super::scope::{Crumbs, Located, Scope, locate};
use super::state::{Change, Reach, RowState, StateBook};
use crate::model::AppSnapshot;
use crate::model::pages::{
    DependencyScope, Known, OrbitModel, OutlineNode, OutlineTree, PackageDossier, PackageRef,
    PageKey, RecordSource, SearchQuery, Standing, SymbolRef,
};
use crate::navigation::{BrowseRoute, OrbitRoute, ReleaseId, Route, SettingsPage};
use crate::shell::kit::{gap_words, package_route};
use facet::data::release::Crate;
use facet::icons::{Icon, Kind};
use gpui::SharedString;
use std::rc::Rc;

/// The book's releases for the version comb (oldest first), the one you
/// pin, and the one being read.
#[derive(Clone)]
pub(super) struct Releases {
    /// The releases, oldest first.
    pub list: Rc<[facet::controls::Release]>,
    /// The pin's index in `list`.
    pub pinned: Option<usize>,
    /// The release being read's index in `list`.
    pub viewing: Option<usize>,
    /// What moving between releases changes, when the release data has this
    /// package.
    pub diffs: Option<std::sync::Arc<Crate>>,
}

/// What the way out does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum StepDoes {
    /// Shows the scope above (browsing: the reader does not move).
    Pop,
    /// Moves the reader (the settings overlay's way back).
    Go(Route),
}

/// The way out: "‹ Library".
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct StepOut {
    /// The scope above, by name.
    pub label: SharedString,
    /// What choosing it does.
    pub does: StepDoes,
}

/// The scope's own title.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Title {
    /// The Library.
    Library {
        /// "2 projects · 13 packages".
        detail: SharedString,
    },
    /// A package: its name and the release the page is about.
    Book {
        /// The package's name.
        name: SharedString,
        /// Its version.
        version: SharedString,
    },
    /// A hoisted module or type.
    Node {
        /// Its kind's mark.
        kind: Kind,
        /// Its name.
        name: SharedString,
        /// "enum · 34 members".
        detail: SharedString,
    },
}

/// Everything above the rows (and the trail below them).
#[derive(Clone, Default)]
pub(super) struct Head {
    /// The way out.
    pub step_out: Option<StepOut>,
    /// The scope's title.
    pub title: Option<Title>,
    /// The comb, under a package's title.
    pub releases: Option<Releases>,
    /// The lens strip's counts; `None` draws no strip.
    pub counts: Option<Counts>,
    /// What you hold, above the lenses.
    pub held: Vec<Chip>,
    /// Where you have been, at the foot.
    pub trail: Vec<Step>,
}

/// One list and what is above it.
pub(super) struct Listing {
    /// Above the rows.
    pub head: Head,
    /// The rows.
    pub rows: Vec<Row>,
    /// How much of the scope a narrowing matched, when one is on.
    pub matched: Option<Matched>,
}

/// What the store held when the listing was asked for.
pub(super) struct Inputs<'a> {
    /// The snapshot: workspace, session.
    pub snapshot: &'a AppSnapshot,
    /// Where the reader is.
    pub route: &'a Route,
    /// Where the sidebar is.
    pub crumbs: &'a Crumbs,
    /// The lens on it.
    pub lens: Lens,
    /// The groups opened or closed by hand.
    pub folds: &'a Folds,
    /// What narrows the list.
    pub filter: Filter<'a>,
    /// What each item of the package carries.
    pub book: &'a StateBook,
    /// The dossier of the package on the path down to the scope.
    pub dossier: Option<&'a PackageDossier>,
    /// The Orbit model.
    pub orbit: Option<&'a OrbitModel>,
    /// The declaration the reader is on.
    pub current: Option<SymbolRef>,
    /// The release data of this package.
    pub diffs: Option<std::sync::Arc<Crate>>,
    /// The settings page open over the reader.
    pub settings: Option<SettingsPage>,
    /// What you hold.
    pub held: Vec<Chip>,
    /// Where you have been.
    pub trail: Vec<Step>,
}

/// Builds the listing.
pub(super) fn build(inputs: &Inputs<'_>) -> Listing {
    if let Some(page) = inputs.settings {
        return settings(inputs, page);
    }
    let listing = match inputs.crumbs.shown() {
        Scope::Library => library(inputs),
        Scope::Package(package) => package_scope(inputs, package),
        Scope::Node(symbol) => node_scope(inputs, symbol),
    };
    let mut listing = narrowed_by_name(listing, inputs.filter.query);
    listing.head.held.clone_from(&inputs.held);
    listing.head.trail.clone_from(&inputs.trail);
    listing
}

/// Narrowing in place, for every list the outline did not already narrow
/// with its parents (releases, dependencies, dependents, the Library): the
/// rows whose names hold the typed words stay, each with the words
/// underlined, under the headings they belong to. Whatever the list, the
/// last row widens the same words to the whole library (Find).
fn narrowed_by_name(mut listing: Listing, query: &str) -> Listing {
    if query.is_empty() {
        return listing;
    }
    if listing.matched.is_none() {
        let of = listing
            .rows
            .iter()
            .filter(|row| row.item().is_some())
            .count();
        let mut kept: Vec<Row> = Vec::new();
        let mut heading: Option<Row> = None;
        let mut shown = 0;
        for row in std::mem::take(&mut listing.rows) {
            match row {
                Row::Heading(_) => heading = Some(row),
                Row::Note(_) => {}
                Row::Item(mut item) => {
                    if let Some(hit) = super::narrow::find_fold(&item.name, query) {
                        item.hit = Some(hit);
                        shown += 1;
                        kept.extend(heading.take());
                        kept.push(Row::Item(item));
                    }
                }
            }
        }
        if kept.is_empty() {
            kept.push(Row::Note("Nothing here matches.".into()));
        }
        listing.rows = kept;
        listing.matched = Some(Matched { shown, of });
    }
    if let Ok(search) = SearchQuery::new(query, SearchQuery::DEFAULT_LIMIT) {
        let mut widen = Item::new(
            RowId::Widen,
            0,
            Mark::Icon(Icon::Search),
            format!("{query} in the whole library"),
            Do::Widen(search),
        );
        widen.trailing = Trailing::Words("↵".into());
        listing.rows.push(Row::Item(widen));
    }
    listing
}

// ---------------------------------------------------------------- settings

fn settings(inputs: &Inputs<'_>, current: SettingsPage) -> Listing {
    let rows = SettingsPage::ALL.into_iter().map(|page| {
        let mut item = Item::new(
            RowId::Setting(page),
            0,
            Mark::Icon(settings_icon(page)),
            page.menu_label(),
            Do::Settings(page),
        );
        item.current = page == current;
        Row::Item(item)
    })
    .collect();
    Listing {
        head: Head {
            step_out: Some(StepOut {
                label: "Nudox".into(),
                does: StepDoes::Go(inputs.route.clone()),
            }),
            title: None,
            releases: None,
            counts: None,
            ..Head::default()
        },
        rows,
        matched: None,
    }
}

/// The icon belongs to the view; page identity and order come from the
/// closed `SettingsPage` vocabulary.
fn settings_icon(page: SettingsPage) -> Icon {
    match page {
        SettingsPage::Appearance => Icon::Eye,
        SettingsPage::Editor => Icon::File,
        SettingsPage::Agents => Icon::Users,
        SettingsPage::Connections => Icon::Link,
        SettingsPage::Privacy => Icon::Lock,
        SettingsPage::Diagnostics => Icon::Info,
        SettingsPage::Index => Icon::Server,
        SettingsPage::Registry => Icon::Globe,
        SettingsPage::Legend => Icon::Diamond,
        SettingsPage::Help => Icon::Key,
    }
}

// ---------------------------------------------------------------- the Library

/// Whether an indexed package is one of your projects: a local package that
/// stands at a project's folder or bears its name. The Library shows it
/// once, under Yours, not again among the packages.
fn is_a_project(
    package_root: &str,
    package_name: &str,
    local: bool,
    project_path: &str,
    project_label: &str,
) -> bool {
    local && (package_root == project_path || package_name == project_label)
}

/// In what order the library's packages are listed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LibraryOrder {
    /// By name, then version: a list a person looks things up in (the
    /// sidebar).
    Name,
    /// As the library lists them (the Library page's ring).
    Library,
}

/// The library's packages that are not one of your projects, in `order`.
/// The sidebar's "In the library" and the Library page's ring list the same.
pub(crate) fn beside_your_projects<'a>(
    indexed: &'a [crate::model::pages::IndexedPackage],
    workspace: &crate::model::WorkspaceState,
    order: LibraryOrder,
) -> Vec<&'a crate::model::pages::IndexedPackage> {
    let yours = |package: &crate::model::pages::IndexedPackage| {
        workspace.projects.iter().any(|project| {
            is_a_project(
                package.package.as_str(),
                &package.name,
                package.package.is_local(),
                &project.path,
                &project.label,
            )
        })
    };
    let mut list: Vec<_> = indexed.iter().filter(|package| !yours(package)).collect();
    if order == LibraryOrder::Name {
        list.sort_by(|a, b| {
            (
                a.name.as_ref(),
                a.package.release_version(),
                a.package.as_str(),
            )
                .cmp(&(
                    b.name.as_ref(),
                    b.package.release_version(),
                    b.package.as_str(),
                ))
        });
    }
    list
}

fn library(inputs: &Inputs<'_>) -> Listing {
    let workspace = inputs.snapshot.workspace();
    let library: Option<Vec<&crate::model::pages::IndexedPackage>> = inputs
        .orbit
        .and_then(|model| model.indexed.known())
        .map(|list| beside_your_projects(list, workspace, LibraryOrder::Name));
    let indexed = library.as_deref();
    let packages = indexed.map(<[_]>::len);
    let projects = workspace.projects.len();
    let detail = match packages {
        Some(packages) => format!(
            "{projects} project{} · {packages} package{}",
            if projects == 1 { "" } else { "s" },
            if packages == 1 { "" } else { "s" }
        ),
        None if inputs.orbit.is_none() => format!("{projects} project{} · packages reading", if projects == 1 { "" } else { "s" }),
        None => format!("{projects} project{} · packages unavailable", if projects == 1 { "" } else { "s" }),
    };
    // The count is what the list shows: your projects and the packages beside them.
    let counts = Counts {
        contents: packages.map(|packages| projects + packages),
        versions: None,
        rests_on: None,
        used_by: Some(projects),
    };
    let head = Head {
        title: Some(Title::Library {
            detail: detail.into(),
        }),
        counts: Some(counts),
        ..Head::default()
    };
    let project_rows = || {
        workspace.projects.iter().flat_map(|project| {
            let mut item = Item::new(
                RowId::Project(project.path.clone()),
                0,
                Mark::Kind(Kind::Module),
                project.label.to_string(),
                Do::Project(project.id.clone()),
            );
            item.current = workspace.active.as_ref() == Some(&project.id);
            if project.phase != crate::model::ProjectPhase::Ready {
                item.trailing = Trailing::Words(project.phase.label().into());
            }
            let mut rows = vec![Row::Item(item)];
            if project.phase != crate::model::ProjectPhase::Missing {
                let mut tree = Item::new(
                    RowId::ProjectTree(project.id.clone()),
                    1,
                    Mark::Icon(Icon::Layers),
                    "Dependency tree",
                    Do::ProjectTree(project.id.clone()),
                );
                tree.current = matches!(inputs.route, Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(id))) if *id == project.id);
                rows.push(Row::Item(tree));
            }
            rows
        })
    };
    let mut rows = Vec::new();
    match inputs.lens {
        Lens::Contents => {
            rows.push(Row::heading("Find", 1));
            rows.push(Row::Item(Item::new(
                RowId::Find,
                0,
                Mark::Icon(Icon::Search),
                "Find packages and declarations",
                Do::Go(Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome))),
            )));
            if projects > 0 {
                rows.push(Row::heading("Yours", projects));
                rows.extend(project_rows());
            }
            match indexed {
                Some(list) => {
                    if !list.is_empty() {
                        rows.push(Row::heading("In the library", list.len()));
                    }
                    let reading = crate::runtime::store::route_package(inputs.route);
                    let apart = told_apart(list);
                    for (package, apart) in list.iter().copied().zip(apart) {
                        let route = package_route(&package.package);
                        let mut item = Item::new(
                            RowId::Package(package.package.clone()),
                            0,
                            Mark::Kind(Kind::Package),
                            package.name.to_string(),
                            route.map_or(Do::Nothing, Do::Go),
                        );
                        item.current = reading.as_ref().is_some_and(|reading| {
                            super::scope::book_of(reading)
                                == super::scope::book_of(&package.package)
                        });
                        item.warm = Some(PageKey::Package(package.package.clone()));
                        item.hoists = Some(Scope::Package(package.package.clone()));
                        // The release, quiet beside the name: two releases
                        // of one crate otherwise read the same.
                        if let Some(version) = package.package.release_version() {
                            item.trailing = Trailing::Words(version.to_owned().into());
                        }
                        item.sub = apart;
                        rows.push(Row::Item(item));
                    }
                }
                None => rows.push(Row::Note(match inputs.orbit.and_then(|orbit| orbit.indexed.gap()) {
                    Some(gap) => format!("The library's packages are unavailable. {}", gap_words(gap)).into(),
                    None => "The library's packages are still being read.".into(),
                })),
            }
        }
        Lens::UsedBy => {
            if projects == 0 {
                rows.push(Row::Note(
                    "No project of yours uses the library yet.".into(),
                ));
            } else {
                rows.push(Row::heading("Yours", projects));
                rows.extend(project_rows());
            }
        }
        Lens::Versions => rows.push(Row::Note(
            "Newer releases across the library are not indexed yet.".into(),
        )),
        Lens::RestsOn => rows.push(Row::Note("The library rests on nothing.".into())),
    }
    Listing {
        head,
        rows,
        matched: None,
    }
}

/// What tells apart packages that share a name (two checkouts of one
/// project, one folder name in two trees): for each local root whose name
/// another row also has, the nearest folder above it that the others do not
/// share, as a quiet word (`backend/…` beside `tree/…`). A registry release
/// is told apart by its version, which its row already carries; a name no
/// other row has needs nothing.
pub(crate) fn told_apart(
    packages: &[&crate::model::pages::IndexedPackage],
) -> Vec<Option<SharedString>> {
    let folders = |package: &crate::model::pages::IndexedPackage| -> Vec<String> {
        let path = package.package.as_str().trim_end_matches(['/', '\\']);
        let mut parts: Vec<String> = path
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        parts.pop();
        parts
    };
    packages
        .iter()
        .enumerate()
        .map(|(index, package)| {
            // A release (a purl, or a registry tree in the cargo cache) is
            // told apart by its version, drawn beside it.
            let folder = |package: &crate::model::pages::IndexedPackage| {
                package.package.is_local() && package.package.release_version().is_none()
            };
            if !folder(package) {
                return None;
            }
            let twins: Vec<Vec<String>> = packages
                .iter()
                .enumerate()
                .filter(|(other, twin)| {
                    *other != index && folder(twin) && twin.name == package.name
                })
                .map(|(_, twin)| folders(twin))
                .collect();
            if twins.is_empty() {
                return None;
            }
            let own = folders(package);
            // The nearest folder, from the name up, where this root parts
            // from every twin.
            let depth = (1..=own.len()).find(|depth| {
                twins.iter().all(|twin| {
                    twin.len() < *depth || twin[twin.len() - depth] != own[own.len() - depth]
                })
            })?;
            let word = &own[own.len() - depth];
            Some(
                if depth == 1 {
                    format!("{word}/")
                } else {
                    format!("{word}/…")
                }
                .into(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------- a package

fn up_label(inputs: &Inputs<'_>, tree: Option<&OutlineTree>) -> Option<StepOut> {
    let label: SharedString = match inputs.crumbs.up()? {
        Scope::Library => "Library".into(),
        Scope::Package(package) => package.display_name().to_owned().into(),
        Scope::Node(symbol) => tree
            .and_then(|tree| locate(tree, symbol))
            .map_or_else(
                || symbol.identity().name().to_owned(),
                |found| super::outline::label(found.node).0,
            )
            .into(),
    };
    Some(StepOut {
        label,
        does: StepDoes::Pop,
    })
}

fn package_scope(inputs: &Inputs<'_>, package: &PackageRef) -> Listing {
    let dossier = inputs.dossier;
    let tree = dossier.and_then(|dossier| dossier.outline.known());
    let version = dossier
        .and_then(|dossier| dossier.record.known())
        .and_then(|record| record.version.known().map(ToString::to_string))
        .unwrap_or_default();
    let head = Head {
        step_out: up_label(inputs, tree),
        title: Some(Title::Book {
            name: package.display_name().to_owned().into(),
            version: version.into(),
        }),
        releases: dossier.and_then(|dossier| releases(inputs.route, dossier, inputs.diffs.clone())),
        counts: Some(package_counts(inputs, package, dossier)),
        ..Head::default()
    };
    let mut matched = None;
    let rows = match inputs.lens {
        Lens::Contents => {
            let (rows, narrowed) = contents(inputs, package, dossier);
            matched = narrowed;
            rows
        }
        Lens::Versions => dossier.map_or_else(Vec::new, |dossier| versions(inputs, dossier)),
        Lens::RestsOn => dossier.map_or_else(Vec::new, rests_on),
        Lens::UsedBy => dossier.map_or_else(Vec::new, |dossier| used_by(inputs, dossier)),
    };
    Listing {
        head,
        rows,
        matched,
    }
}

fn package_counts(
    inputs: &Inputs<'_>,
    package: &PackageRef,
    dossier: Option<&PackageDossier>,
) -> Counts {
    let Some(dossier) = dossier else {
        return Counts::default();
    };
    let crates = inputs.book.crates().len();
    let dependents = dossier.dependents.known().map(|list| list.len());
    // Contents counts what its list can show: on the intro, the items the
    // families hold (the page's "N public names"); anywhere else, every name
    // of the outline, groups closed or open.
    let intro = !inputs.filter.is_active() && reader_lists_modules(inputs, package);
    Counts {
        contents: dossier.outline.known().map(|tree| {
            if intro {
                super::outline::items(tree)
            } else {
                super::outline::tree_names(tree)
            }
        }),
        versions: dossier.versions.known().map(|list| list.len()),
        rests_on: dossier.dependencies.known().map(|list| list.len()),
        used_by: (crates > 0 || dependents.is_some()).then(|| crates + dependents.unwrap_or(0)),
    }
}

/// Whether the reader is on this package's own page, which draws every
/// module: the one place the sidebar must not list them.
fn reader_lists_modules(inputs: &Inputs<'_>, package: &PackageRef) -> bool {
    matches!(inputs.route, Route::Package(_))
        && crate::runtime::store::route_package(inputs.route).is_some_and(|reading| {
            super::scope::book_of(&reading) == super::scope::book_of(package)
        })
}

fn contents(
    inputs: &Inputs<'_>,
    package: &PackageRef,
    dossier: Option<&PackageDossier>,
) -> (Vec<Row>, Option<Matched>) {
    let Some(dossier) = dossier else {
        return (Vec::new(), None);
    };
    let tree = match &dossier.outline {
        Known::Known(tree) => tree,
        Known::Unknown(gap) => return (vec![Row::Note(gap_words(gap))], None),
    };
    let outline = Outline {
        package,
        current: inputs.current.as_ref(),
        folds: inputs.folds,
        book: inputs.book,
        filter: inputs.filter,
    };
    if !inputs.filter.is_active() && reader_lists_modules(inputs, package) {
        return (outline.intro_rows(tree), None);
    }
    let Listed { rows, matched } = outline.package_rows(tree);
    narrowed(rows, matched, inputs.filter)
}

/// A narrowed list's rows and its count; an empty one says so.
fn narrowed(
    mut rows: Vec<Row>,
    matched: Matched,
    filter: Filter<'_>,
) -> (Vec<Row>, Option<Matched>) {
    if !filter.is_active() {
        return (rows, None);
    }
    if rows.is_empty() {
        rows.push(Row::Note("Nothing here matches.".into()));
    }
    (rows, Some(matched))
}

/// Whether the reader is on this book (any release of it): what the pin and
/// the release being read mean for a dossier.
fn on_this_book(route: &Route, dossier: &PackageDossier) -> bool {
    crate::runtime::store::route_package(route).is_some_and(|reading| {
        super::scope::book_of(&reading) == super::scope::book_of(&dossier.package)
    })
}

/// The release the reader pins in this book: the route's own, when the reader is on the book.
fn pin_of(route: &Route, dossier: &PackageDossier) -> Option<String> {
    let routed = match route {
        Route::Package(route) => PackageRef::parse(route.package.as_str()).ok(),
        Route::Symbol(route) => PackageRef::parse(route.package.as_str()).ok(),
        Route::CargoSource(route) => PackageRef::parse(route.package.as_str()).ok(),
        Route::Orbit(_) | Route::World => None,
    };
    routed
        .filter(|_| on_this_book(route, dossier))
        .and_then(|package| package.version().map(str::to_owned))
}

/// The release being read (not the pin), when the reader is on this book at another release.
fn viewing_of<'a>(route: &'a Route, dossier: &PackageDossier) -> Option<&'a str> {
    route
        .at()
        .map(|at| at.as_str())
        .filter(|_| on_this_book(route, dossier))
}

/// The version comb's releases and what it needs to draw the pin and the
/// release being read.
fn releases(
    route: &Route,
    dossier: &PackageDossier,
    diffs: Option<std::sync::Arc<Crate>>,
) -> Option<Releases> {
    let versions = dossier.versions.known()?;
    let mut list: Vec<_> = versions.iter().collect();
    list.reverse();
    let viewing = viewing_of(route, dossier)
        .and_then(|at| list.iter().position(|entry| entry.version.as_ref() == at));
    let pinned = pin_of(route, dossier)
        .and_then(|version| {
            list.iter()
                .position(|entry| entry.version.as_ref() == version)
        })
        .or_else(|| list.iter().position(|entry| entry.current));
    Some(Releases {
        list: list
            .iter()
            .map(|entry| facet::controls::Release {
                id: facet::controls::ReleaseId(entry.version.to_string().into()),
                version: entry.version.to_string().into(),
                step: facet::controls::Step::of(&entry.version),
                age: SharedString::default(),
            })
            .collect(),
        pinned,
        viewing,
        diffs,
    })
}

// ---------------------------------------------------------------- Versions

/// Releases newest first: the pin in mint, the one being read in
/// periwinkle. Choosing one reads the book at it (the reader shows the
/// diff, and the amber state appears on the rows); choosing the pin comes
/// back.
fn versions(inputs: &Inputs<'_>, dossier: &PackageDossier) -> Vec<Row> {
    let list = match &dossier.versions {
        Known::Known(list) if list.is_empty() => {
            return vec![Row::Note(
                "No releases are recorded for this package.".into(),
            )];
        }
        Known::Known(list) => list,
        Known::Unknown(gap) => return vec![Row::Note(gap_words(gap))],
    };
    let pinned = pin_of(inputs.route, dossier).or_else(|| {
        list.iter()
            .find(|entry| entry.current)
            .map(|entry| entry.version.to_string())
    });
    let viewing = viewing_of(inputs.route, dossier).map(str::to_owned);
    let changed = inputs.book.compared().map(|compared| compared.changed);
    let mut rows = vec![Row::heading("Releases", list.len())];
    for entry in list.iter() {
        let version = entry.version.to_string();
        let is_pin = pinned.as_deref() == Some(version.as_str());
        let is_viewing = viewing.as_deref() == Some(version.as_str());
        let mark = if is_viewing {
            ReleaseMark::Reading
        } else if is_pin {
            ReleaseMark::Pin
        } else {
            ReleaseMark::Other
        };
        let at = (!is_pin).then(|| ReleaseId::new(&version).ok()).flatten();
        let mut item = Item::new(
            RowId::Release(version.as_str().into()),
            0,
            Mark::Release(mark),
            version.clone(),
            Do::Release(at),
        );
        item.current = is_viewing || (is_pin && viewing.is_none());
        item.dim = entry.standing == Standing::Yanked;
        item.sub = if is_pin {
            Some("your pin".into())
        } else if entry.standing == Standing::Yanked {
            Some("yanked".into())
        } else if is_viewing {
            Some("reading".into())
        } else {
            None
        };
        if is_viewing
            && let Some(changed) = changed
                .and_then(|changed| u32::try_from(changed).ok())
                .filter(|changed| *changed > 0)
        {
            item.trailing = Trailing::State(RowState {
                change: Change::Changes(changed),
                ..RowState::default()
            });
        }
        rows.push(Row::Item(item));
    }
    rows
}

// ---------------------------------------------------------------- Rests on

fn rests_on(dossier: &PackageDossier) -> Vec<Row> {
    let list = match &dossier.dependencies {
        Known::Known(list) if list.is_empty() => return vec![Row::Note("Rests on nothing".into())],
        Known::Known(list) => list,
        Known::Unknown(gap) => return vec![Row::Note(gap_words(gap))],
    };
    let mut rows = vec![Row::heading("Directly", list.len())];
    for dependency in list.iter() {
        let scope = match dependency.scope {
            DependencyScope::Development => " · dev",
            DependencyScope::Build => " · build",
            DependencyScope::Optional => " · optional",
            DependencyScope::Runtime | DependencyScope::Peer => "",
        };
        let route = dependency.resolved.as_ref().and_then(package_route);
        let mut item = Item::new(
            RowId::Dependency(dependency.name.clone()),
            0,
            Mark::Kind(Kind::Package),
            dependency.name.to_string(),
            route.map_or(Do::Nothing, Do::Go),
        );
        item.warm = dependency.resolved.clone().map(PageKey::Package);
        item.trailing = Trailing::Words(format!("{}{scope}", dependency.requirement).into());
        // Not resolved to an indexed release: nowhere to go yet.
        item.dim = dependency.resolved.is_none();
        rows.push(Row::Item(item));
    }
    rows
}

// ---------------------------------------------------------------- Used by

/// Your crates first, with how often each uses the package (choosing one
/// narrows Contents to what it uses), then the packages in the library that
/// depend on it.
fn used_by(inputs: &Inputs<'_>, dossier: &PackageDossier) -> Vec<Row> {
    let crates = inputs.book.crates();
    let gap = dossier.dependents.gap();
    let observed = if dossier.observed_dependents.is_empty() {
        dossier
            .dependents
            .known()
            .map_or(&[][..], AsRef::as_ref)
    } else {
        dossier.observed_dependents.as_ref()
    };
    if observed.is_empty() && crates.is_empty() {
        if let Some(gap) = gap {
            let words = if gap.reason == crate::model::pages::GapReason::Unknown {
                format!("Partial coverage: {}", gap.detail)
            } else {
                gap_words(gap).to_string()
            };
            return vec![Row::Note(words.into())];
        }
    }
    let dependent_row = |record: &crate::model::pages::PackageRecord| {
        let mut item = Item::new(
            RowId::Dependent(record.package.clone()),
            0,
            Mark::Kind(Kind::Package),
            record.name.to_string(),
            package_route(&record.package).map_or(Do::Nothing, Do::Go),
        );
        item.warm = Some(PageKey::Package(record.package.clone()));
        item.trailing = record.version.known().map_or(Trailing::Nothing, |version| {
            Trailing::Words(version.to_string().into())
        });
        Row::Item(item)
    };
    let (yours, others): (Vec<_>, Vec<_>) = observed
        .iter()
        .partition(|record| record.source == RecordSource::LocalManifest);
    let mut rows = Vec::new();
    if !crates.is_empty() || !yours.is_empty() {
        rows.push(Row::heading("Yours", crates.len() + yours.len()));
        for reach in &crates {
            rows.push(Row::Item(crate_row(inputs, reach)));
        }
        rows.extend(yours.iter().map(|record| dependent_row(record)));
    }
    if !others.is_empty() {
        rows.push(Row::heading("In the library", others.len()));
        rows.extend(others.iter().map(|record| dependent_row(record)));
    }
    if let Some(gap) = gap.filter(|gap| gap.reason == crate::model::pages::GapReason::Unknown) {
        rows.push(Row::Note(format!("Partial coverage: {}", gap.detail).into()));
    }
    if rows.is_empty() {
        rows.push(Row::Note("Nothing here uses it yet".into()));
    }
    rows
}

fn crate_row(inputs: &Inputs<'_>, reach: &Reach) -> Item {
    let mut item = Item::new(
        RowId::Crate(reach.by.clone()),
        0,
        Mark::Kind(Kind::Module),
        reach.by.shared(),
        Do::Via(reach.by.clone()),
    );
    item.sub = Some(
        format!(
            "{} item{}",
            reach.items,
            if reach.items == 1 { "" } else { "s" }
        )
        .into(),
    );
    item.trailing = Trailing::State(RowState {
        uses: Some(reach.uses),
        ..RowState::default()
    });
    item.current = inputs.filter.via == Some(&reach.by);
    item
}

// ---------------------------------------------------------------- a hoisted node

fn node_scope(inputs: &Inputs<'_>, symbol: &SymbolRef) -> Listing {
    let Some(package) = inputs.crumbs.package() else {
        return Listing {
            head: bare_head(inputs),
            rows: vec![Row::Note("No package is open.".into())],
            matched: None,
        };
    };
    let tree = inputs.dossier.and_then(|dossier| dossier.outline.known());
    let found = tree.and_then(|tree| locate(tree, symbol));
    let Some(Located { node, ancestors }) = found else {
        let rows = if inputs.dossier.is_some() {
            vec![Row::Note("It is not in this release's outline.".into())]
        } else {
            Vec::new()
        };
        return Listing {
            head: bare_head(inputs),
            rows,
            matched: None,
        };
    };
    let members = super::outline::kids(node).count();
    let module = node.decl.kind == Some(backend_library::DeclarationKind::Module);
    let noun = match (module, members) {
        (true, 1) => "item",
        (true, _) => "items",
        (false, 1) => "member",
        (false, _) => "members",
    };
    let head = Head {
        step_out: up_label(inputs, tree),
        title: Some(Title::Node {
            kind: crate::shell::kit::kind_of(node.decl.kind),
            name: super::outline::label(node).0.into(),
            detail: format!(
                "{}{} · {members} {noun}",
                super::outline::label(node)
                    .1
                    .map(|quiet| format!("{quiet} · "))
                    .unwrap_or_default(),
                node.decl.kind_name()
            )
            .into(),
        }),
        counts: Some(Counts {
            contents: Some(
                super::outline::kids(node)
                    .map(super::outline::names)
                    .sum::<usize>(),
            ),
            versions: None,
            rests_on: None,
            used_by: None,
        }),
        ..Head::default()
    };
    let mut matched = None;
    let rows = match inputs.lens {
        Lens::Contents => {
            let outline = Outline {
                package,
                current: inputs.current.as_ref(),
                folds: inputs.folds,
                book: inputs.book,
                filter: inputs.filter,
            };
            let Listed {
                rows,
                matched: count,
            } = outline.node_rows(node, &ancestors);
            let (rows, narrowed_count) = narrowed(rows, count, inputs.filter);
            matched = narrowed_count;
            rows
        }
        Lens::Versions => vec![Row::Note(
            "A declaration's history is not indexed yet.".into(),
        )],
        Lens::RestsOn => vec![Row::Note("What it is made of is not indexed yet.".into())],
        Lens::UsedBy => node_users(inputs, node, &ancestors),
    };
    Listing {
        head,
        rows,
        matched,
    }
}

/// The crates of yours that use a hoisted item, the busiest first.
fn node_users(inputs: &Inputs<'_>, node: &OutlineNode, ancestors: &[&OutlineNode]) -> Vec<Row> {
    let path = super::outline::path_of(inputs.book, ancestors, node);
    let users = inputs.book.users_of(&path);
    if users.is_empty() {
        return vec![Row::Note("No crate of yours is known to use it.".into())];
    }
    let mut rows = vec![Row::Heading(Heading {
        words: "Your code".into(),
        count: Some(users.len()),
    })];
    for usage in users {
        let mut item = Item::new(
            RowId::Crate(usage.by.clone()),
            0,
            Mark::Kind(Kind::Module),
            usage.by.shared(),
            Do::Via(usage.by.clone()),
        );
        item.trailing = Trailing::State(RowState {
            uses: Some(usage.uses),
            ..RowState::default()
        });
        item.current = inputs.filter.via == Some(&usage.by);
        rows.push(Row::Item(item));
    }
    rows
}

/// A header with only the way out, for a scope whose node is not there.
fn bare_head(inputs: &Inputs<'_>) -> Head {
    Head {
        step_out: up_label(inputs, None),
        counts: Some(Counts::default()),
        ..Head::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(name: &str) -> Row {
        Row::Item(Item::new(
            RowId::Dependency(name.into()),
            0,
            Mark::Kind(Kind::Package),
            name.to_owned(),
            Do::Nothing,
        ))
    }

    fn listing(rows: Vec<Row>) -> Listing {
        Listing {
            head: Head::default(),
            rows,
            matched: None,
        }
    }

    fn words(listing: &Listing) -> Vec<String> {
        listing
            .rows
            .iter()
            .map(|row| match row {
                Row::Item(item) => item.name.to_string(),
                Row::Heading(heading) => heading.words.to_uppercase(),
                Row::Note(words) => words.to_string(),
            })
            .collect()
    }

    fn indexed(root: &str) -> crate::model::pages::IndexedPackage {
        let package = PackageRef::parse(root).expect("a package");
        crate::model::pages::IndexedPackage {
            name: package.display_name().into(),
            package,
            readiness: crate::model::pages::Readiness::Ready,
        }
    }

    /// Two roots with one name (a checkout and a copy of it) are told apart
    /// by the nearest folder they do not share; a name no other row has, and
    /// a registry release (its version is on its row), need nothing.
    #[test]
    fn two_packages_with_one_name_are_told_apart_by_the_nearest_folder_they_do_not_share() {
        let list = [
            indexed("/work/backend/apps/fixtures/lang/go/pflag"),
            indexed("/work/backend/.local/tree/apps/fixtures/lang/go/pflag"),
            indexed("/work/backend/apps/fixtures/lang/ts/zod"),
            indexed("/work/one/present"),
            indexed("/work/two/present"),
            indexed("pkg:cargo/toml@0.8.23"),
            indexed("pkg:cargo/toml@1.1.6"),
        ];
        let refs: Vec<_> = list.iter().collect();
        let apart: Vec<Option<String>> = told_apart(&refs)
            .into_iter()
            .map(|word| word.map(|word| word.to_string()))
            .collect();
        assert_eq!(
            apart,
            vec![
                Some("backend/…".to_owned()),
                Some("tree/…".to_owned()),
                None,
                Some("one/".to_owned()),
                Some("two/".to_owned()),
                None,
                None
            ]
        );
    }

    #[test]
    fn a_local_package_at_a_projects_folder_or_with_its_name_is_the_project_and_appears_once() {
        assert!(
            is_a_project("/work/toml_pin", "toml_pin", true, "/work/toml_pin", "pin"),
            "the same folder"
        );
        assert!(
            is_a_project(
                "/index/copy",
                "toml_pin",
                true,
                "/work/toml_pin",
                "toml_pin"
            ),
            "the same name"
        );
        assert!(
            !is_a_project(
                "pkg:cargo/toml_pin@0.1.0",
                "toml_pin",
                false,
                "/work/other",
                "toml_pin"
            ),
            "a registry release is not your project, even with the name"
        );
        assert!(
            !is_a_project(
                "/work/present",
                "present",
                true,
                "/work/toml_pin",
                "toml_pin"
            ),
            "another local package stays a package"
        );
    }

    #[test]
    fn narrowing_a_flat_list_keeps_matches_under_their_headings_and_drops_a_heading_with_nothing_left()
     {
        let rows = vec![
            Row::heading("Yours", 1),
            dependency("desktop"),
            Row::heading("In the library", 2),
            dependency("toml_pin"),
            dependency("serde"),
        ];
        let narrowed = narrowed_by_name(listing(rows), "TOML");
        assert_eq!(
            words(&narrowed),
            ["IN THE LIBRARY", "toml_pin", "TOML in the whole library"]
        );
        assert_eq!(
            narrowed.matched,
            Some(Matched { shown: 1, of: 3 }),
            "one of the three rows"
        );
        let kept = narrowed
            .rows
            .iter()
            .filter_map(Row::item)
            .next()
            .expect("the match");
        assert_eq!(
            kept.hit,
            Some(0..4),
            "the typed words underlined, whatever their case"
        );
    }

    #[test]
    fn the_last_row_widens_the_words_to_find_and_is_there_even_when_nothing_matched() {
        let narrowed = narrowed_by_name(listing(vec![dependency("desktop")]), "zzz");
        assert_eq!(
            words(&narrowed),
            ["Nothing here matches.", "zzz in the whole library"]
        );
        assert_eq!(narrowed.matched, Some(Matched { shown: 0, of: 1 }));
        let widen = narrowed
            .rows
            .last()
            .and_then(Row::item)
            .expect("the widening row");
        assert_eq!(widen.id, RowId::Widen);
        assert_eq!(
            widen.does,
            Do::Widen(SearchQuery::new("zzz", SearchQuery::DEFAULT_LIMIT).expect("query")),
            "Find gets exactly the typed words"
        );
        assert_eq!(widen.trailing, Trailing::Words("↵".into()));
    }

    #[test]
    fn nothing_typed_leaves_the_list_alone() {
        let rows = vec![Row::heading("Yours", 1), dependency("desktop")];
        let narrowed = narrowed_by_name(listing(rows.clone()), "");
        assert_eq!(narrowed.rows, rows);
        assert_eq!(narrowed.matched, None);
    }

    #[test]
    fn an_outline_already_narrowed_with_its_parents_is_not_narrowed_again() {
        let mut already = listing(vec![dependency("glyph"), dependency("KindGlyph")]);
        already.matched = Some(Matched { shown: 1, of: 11 });
        let narrowed = narrowed_by_name(already, "kind");
        assert_eq!(
            words(&narrowed),
            ["glyph", "KindGlyph", "kind in the whole library"],
            "the parent stays as context: only the widening row is added"
        );
        assert_eq!(narrowed.matched, Some(Matched { shown: 1, of: 11 }));
    }
}
