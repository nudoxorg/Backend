//! The desktop capture adapter: the gallery command line (`capture`, `film`,
//! `motion-report`, `storm`, `matrix`, `lint`, `perf`, `verify`) over the
//! real desktop shell, on a real local index of fixed fixture crates.
//!
//! Settings arrive as the product's own intents (text size as `ZoomTo` on
//! the window's display).
//!
//! Determinism. The fixture owner indexes `crates/present`,
//! `frontends/rust/fixtures/rich_project`, `crates/runtime`,
//! `frontends/rust/fixtures/toml_pin` and toml 0.8.23's source from the local
//! cargo registry cache once into `.local/harness/desktop/` and every later
//! boot reuses that index. Page
//! reads are real I/O on real threads, so each boot declares a quiescence
//! predicate: after the first frame and after every input instant the run
//! waits in real time (virtual time stands still) until the read pool is
//! empty, every result has landed, and the root has no engine work in
//! flight. I/O then takes zero virtual time and a frame at a virtual time is
//! a function of the script.
//!
//! The seams, each one line or one function: the window root is
//! [`ROOT`] (`crate::shell::open_shell`); a route description becomes a
//! product route in [`route::to_route`] (`Route::Symbol { id, at, view }`,
//! `Route::World`, `PackageRoute { at }`); settings acts become product
//! intents in [`adapt`].

use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::pages::{PackageRef, SearchQuery, SymbolRef};
use crate::model::{
    AppSnapshot, AppearancePreference, ContrastPreference, DensityPreference, MotionPreference,
};
use crate::navigation::Intent;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadPool, ReadRequest, SessionReader};
use crate::runtime::{CancellationToken, DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use backend_client::{LocalSubscriptionTransport, Session};
use backend_gui_harness::Act;
use facet::ActiveFacet;
use facet::gallery::{self, Scene};
use gpui::{AnyView, App, Global, Window};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub mod journey;

/// The window root the adapter mounts: the one line to change when the
/// shell's constructor moves.
const ROOT: fn(&UiEntityGraph, &mut Window, &mut App) -> gpui::Entity<crate::shell::Shell> =
    crate::shell::open_shell;

const INDEX_DEADLINE: Duration = Duration::from_mins(15);

/// How long to wait for another process's owner to answer.
const OWNER_DEADLINE: Duration = Duration::from_mins(1);

/// The fixture owner: a local index of fixed crates, shared by every boot in
/// this process.
pub struct Fixture {
    host: crate::DesktopHost,
    projects: Vec<PathBuf>,
}

impl Fixture {
    /// The owner's endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        self.host.endpoint()
    }

    /// The indexed fixture roots.
    #[must_use]
    pub fn projects(&self) -> &[PathBuf] {
        &self.projects
    }
}

thread_local! {
    static FIXTURE: RefCell<Option<&'static Fixture>> = const { RefCell::new(None) };
}

static STATE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Keeps this process's fixture index in `dir` instead of
/// `.local/harness/desktop` (same roots, its own owner), so a long run such
/// as a journey never contends with scene runs for the index's lock. Call
/// it before the first [`fixture`]; later calls are ignored.
pub fn keep_index_in(dir: PathBuf) {
    let _ = STATE.set(dir);
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A crate's unpacked source in the local cargo registry cache
/// (`$CARGO_HOME/registry/src/<index>/<name-version>`): indexed offline,
/// never fetched.
fn registry_source(release: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("CARGO_HOME").map_or_else(
        || std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")),
        |home| Some(PathBuf::from(home)),
    );
    let src = home
        .ok_or_else(|| "neither CARGO_HOME nor HOME is set".to_owned())?
        .join("registry/src");
    let mut found = std::fs::read_dir(&src)
        .map_err(|error| format!("{}: {error}", src.display()))?
        .filter_map(|index| Some(index.ok()?.path().join(release)))
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect::<Vec<_>>();
    found.sort();
    found.pop().map_or_else(
        || Err(format!("{release} is not in the cargo registry cache under {}; fetch it once with cargo", src.display())),
        |path| path.canonicalize().map_err(|error| format!("{}: {error}", path.display())),
    )
}

/// The owner's endpoint for the index in `data`: one per data directory,
/// not per process. Ownership is one lock on `data`, so a second process
/// cannot own it; with the same endpoint it reaches the running owner
/// (`DesktopHost::start_with_paths` attaches to a live one) instead of
/// dialling a socket nobody listens on. Under `/tmp`: a socket path under
/// the repository exceeds `sockaddr_un`.
fn endpoint_for(data: &Path) -> Result<PathBuf, String> {
    use sha2::Digest as _;
    let data = data
        .canonicalize()
        .map_err(|error| format!("{}: {error}", data.display()))?;
    let digest = sha2::Sha256::digest(data.as_os_str().as_encoded_bytes());
    let hex = digest.iter().take(6).map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(PathBuf::from(format!("/tmp/nx-harness-{hex}.sock")))
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Starts (or reuses) the fixture owner and waits until every fixture crate
/// is indexed and the row count is stable.
///
/// When another process already owns the index, this one attaches to that
/// owner (the endpoint is per data directory). It still asks for every root
/// it knows and waits for each to be Ready: an owner started by an older
/// binary with fewer roots indexes the missing ones (the other process then
/// sees them too), and nothing is served until they are ready or the
/// deadline passes. The attached process depends on the owner's process: if
/// that exits mid-run, this one's reads fail.
///
/// # Errors
/// The owner cannot start, or indexing does not settle in 15 minutes.
pub fn fixture() -> Result<&'static Fixture, String> {
    if let Some(fixture) = FIXTURE.with(|cell| *cell.borrow()) {
        return Ok(fixture);
    }
    let repo = repo()
        .canonicalize()
        .map_err(|error| format!("repository root: {error}"))?;
    let projects = vec![
        repo.join("crates/present"),
        repo.join("frontends/rust/fixtures/rich_project"),
        // The journeys' subjects (gui-plan §3 item 10, "Data"): J5 reads
        // `crates/runtime`; "your project" pins toml 0.8.23, whose source is
        // indexed offline from the local cargo registry cache.
        repo.join("crates/runtime"),
        repo.join("frontends/rust/fixtures/toml_pin"),
        registry_source("toml-0.8.23")?,
    ];
    let state = STATE
        .get()
        .cloned()
        .unwrap_or_else(|| repo.join(".local/harness/desktop"));
    std::fs::create_dir_all(state.join("data")).map_err(|error| format!("{}: {error}", state.display()))?;
    let endpoint = endpoint_for(&state.join("data"))?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(projects[0].clone()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .map_err(|error| format!("workspace paths: {error}"))?;
    // Another process may hold the index lock and still be opening it (the
    // host waits 2 s for its endpoint; a debug owner can take longer): keep
    // asking until it answers or the lock frees, for up to a minute.
    let attaching = Instant::now();
    let host = loop {
        match crate::DesktopHost::start_with_paths(paths.clone()) {
            Ok(host) => break host,
            Err(crate::HostError::Contended { .. }) if attaching.elapsed() < OWNER_DEADLINE => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(format!("fixture owner: {error}")),
        }
    };
    let mut session = Session::connect(&endpoint).map_err(|error| format!("session: {error}"))?;
    for project in &projects {
        session
            .index(utf8(project)?)
            .map_err(|error| format!("index {}: {error}", project.display()))?;
    }
    let started = Instant::now();
    let (mut last_rows, mut stable) = (0, 0);
    loop {
        let ready = match session.packages().map(|reply| reply.reply) {
            Ok(backend_library::CommandReply::Packages(snapshot)) => projects.iter().all(|project| {
                snapshot.root.rows().iter().any(|row| {
                    Some(row.label.as_str()) == project.to_str()
                        && row.state == backend_library::RowState::Ready
                })
            }),
            _ => false,
        };
        let rows = session.health().map_or(0, |health| health.row_count());
        stable = if ready && rows > 0 && rows == last_rows { stable + 1 } else { 0 };
        last_rows = rows;
        if stable >= 3 {
            break;
        }
        if started.elapsed() > INDEX_DEADLINE {
            return Err("the fixture index never settled".to_owned());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let fixture: &'static Fixture = Box::leak(Box::new(Fixture { host, projects }));
    FIXTURE.with(|cell| *cell.borrow_mut() = Some(fixture));
    Ok(fixture)
}

/// Route descriptions scripts and scenes use, and the one function that
/// turns them into product routes.
pub mod route {
    use super::{Fixture, resolve_package, resolve_symbol};
    use crate::core::PackageId;
    use crate::navigation::{
        Coordinate, OrbitRoute, PackageLane, PackageRoute, ReleaseId, Route, SymbolRoute,
    };
    pub use crate::navigation::View;

    /// A route, as a script names it.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Target {
        /// Home.
        Orbit,
        /// The world graph, no symbol.
        World,
        /// A package by name or path (`present`), optionally at a release.
        Package {
            /// Name or path.
            id: String,
            /// Release (`0.3.0`).
            at: Option<String>,
        },
        /// A symbol by path (`present::glyph::RelationLabel`), optionally
        /// at a release, in a view.
        Symbol {
            /// Path.
            id: String,
            /// Release (`0.3.0`).
            at: Option<String>,
            /// View.
            view: View,
        },
    }

    fn options(kind: &str, words: &[&str], view: &mut Option<View>) -> Result<Option<String>, String> {
        let mut at = None;
        for option in words {
            match option.split_once('=') {
                Some(("view", "page")) if view.is_some() => *view = Some(View::Page),
                Some(("view", "code")) if view.is_some() => *view = Some(View::Code),
                Some(("view", "graph")) if view.is_some() => *view = Some(View::Graph),
                Some(("at", release)) if !release.is_empty() => at = Some(release.to_owned()),
                _ => return Err(format!("route {kind}: unknown option `{option}`")),
            }
        }
        Ok(at)
    }

    /// Parses `orbit`, `world`, `package ID [at=RELEASE]`, or
    /// `symbol ID [view=page|code|graph] [at=RELEASE]`.
    ///
    /// # Errors
    /// Anything else, in words.
    pub fn parse(words: &str) -> Result<Target, String> {
        let parts = words.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["orbit"] => Ok(Target::Orbit),
            ["world"] => Ok(Target::World),
            ["package", id, rest @ ..] => Ok(Target::Package {
                id: (*id).to_owned(),
                at: options("package", rest, &mut None)?,
            }),
            ["symbol", id, rest @ ..] => {
                let mut view = Some(View::Page);
                let at = options("symbol", rest, &mut view)?;
                Ok(Target::Symbol {
                    id: (*id).to_owned(),
                    at,
                    view: view.unwrap_or_default(),
                })
            }
            _ => Err(format!(
                "route `{words}`: expected orbit, world, package ID [at=RELEASE], or symbol ID [view=page|code|graph] [at=RELEASE]"
            )),
        }
    }

    fn release(at: Option<&String>) -> Result<Option<ReleaseId>, String> {
        at.map(|release| {
            ReleaseId::new(release).map_err(|error| format!("release `{release}`: {error}"))
        })
        .transpose()
    }

    /// The product route for `target`: names resolve against the fixture
    /// index, everything else maps one to one onto the shell's route model.
    ///
    /// # Errors
    /// A name the index does not resolve to exactly one declaration.
    pub fn to_route(target: &Target, fixture: &Fixture) -> Result<Route, String> {
        match target {
            Target::Orbit => Ok(Route::Orbit(OrbitRoute::Home)),
            Target::World => Ok(Route::World),
            Target::Package { id, at } => {
                let package = resolve_package(id, fixture)?;
                Ok(Route::Package(PackageRoute {
                    project: None,
                    package: PackageId::new(package.as_str())
                        .map_err(|error| format!("route package {id}: {error}"))?,
                    lane: PackageLane::Overview,
                    selected: None,
                    at: release(at.as_ref())?,
                }))
            }
            Target::Symbol { id, at, view } => {
                let (symbol, _line) = resolve_symbol(id, fixture)?;
                let package = symbol
                    .package()
                    .ok_or_else(|| format!("route symbol {id}: its coordinate names no package"))?;
                Ok(Route::Symbol(SymbolRoute {
                    project: None,
                    package: PackageId::new(package.as_str())
                        .map_err(|error| format!("route symbol {id}: {error}"))?,
                    id: Coordinate::new(symbol.as_str())
                        .map_err(|error| format!("route symbol {id}: {error}"))?,
                    at: release(at.as_ref())?,
                    view: *view,
                    // The code view opens at the declaration's own line.
                    line: None,
                    selected: None,
                }))
            }
        }
    }
}

fn read(fixture: &Fixture, request: &ReadRequest) -> Result<crate::model::pages::PageValue, String> {
    let mut reader = SessionReader::connect(fixture.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext {
        worker: 0,
        cancel: &cancel,
        outlines: &outlines,
    };
    reader
        .read(request, &context)
        .map_err(|error| format!("read {request:?}: {error:?}"))
}

/// The fixture package named `id` (a crate name or a path).
///
/// # Errors
/// No fixture package matches.
pub fn resolve_package(id: &str, fixture: &Fixture) -> Result<PackageRef, String> {
    let root = fixture
        .projects()
        .iter()
        .find(|project| project.ends_with(id) || project.to_str() == Some(id))
        .ok_or_else(|| {
            format!(
                "no fixture package `{id}` (have: {})",
                fixture
                    .projects()
                    .iter()
                    .filter_map(|project| project.file_name()?.to_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    PackageRef::parse(utf8(root)?).map_err(|error| format!("package {id}: {error:?}"))
}

/// The coordinate (and line) of `present::glyph::RelationLabel`: the last
/// segment is the name, the first names the fixture package, the ones in
/// between must appear in the declaration's path or coordinate.
///
/// # Errors
/// No declaration or more than one matches.
pub fn resolve_symbol(id: &str, fixture: &Fixture) -> Result<(SymbolRef, u32), String> {
    let segments = id.split("::").collect::<Vec<_>>();
    let (Some(name), Some(package)) = (segments.last(), segments.first()) else {
        return Err(format!("symbol `{id}`: expected package::path::Name"));
    };
    let package = resolve_package(package, fixture)?;
    let query = SearchQuery::new(name, 200).map_err(|error| format!("search {name}: {error:?}"))?;
    let crate::model::pages::PageValue::Search(page) = read(fixture, &ReadRequest::Search(query))? else {
        return Err(format!("search {name}: not a search page"));
    };
    let middle = &segments[1..segments.len().saturating_sub(1)];
    let matches = page
        .rows
        .iter()
        .filter(|row| {
            row.decl.name.as_ref() == *name
                && row
                    .package
                    .as_deref()
                    .is_some_and(|spelling| spelling == package.as_str())
                && middle.iter().all(|segment| {
                    row.decl
                        .path
                        .as_deref()
                        .is_some_and(|path| path.contains(segment))
                        || row.decl.coordinate.as_str().contains(segment)
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [row] => Ok((row.decl.coordinate.clone(), row.decl.line.unwrap_or(1))),
        [] => Err(format!(
            "symbol `{id}`: no match among {} results for `{name}` ({})",
            page.rows.len(),
            page.rows
                .iter()
                .take(6)
                .map(|row| format!("{} in {:?}", row.decl.coordinate.as_str(), row.package))
                .collect::<Vec<_>>()
                .join("; ")
        )),
        many => Err(format!(
            "symbol `{id}` is ambiguous: {}",
            many.iter()
                .map(|row| row.decl.coordinate.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The booted graph of the window being captured.
struct Booted {
    graph: UiEntityGraph,
    fixture: &'static Fixture,
    shell: gpui::Entity<crate::shell::Shell>,
    /// Render counts and landed reads at the previous annotation.
    last: std::cell::Cell<(crate::shell::RenderCounts, u64)>,
}

impl Global for Booted {}

/// Boots the desktop on the fixture in `window` at `start`, with the
/// current facet's settings applied through the product's own intents.
///
/// # Errors
/// The fixture, the owner handshake, the actor, the read pool, or the
/// start route failing.
pub fn boot(start: &str, window: &mut Window, cx: &mut App) -> Result<AnyView, String> {
    let fixture = fixture()?;
    let endpoint = fixture.endpoint().to_path_buf();
    let mut subscription = LocalSubscriptionTransport::connect(&endpoint)
        .map_err(|error| format!("subscription: {error}"))?;
    let (view, revision) = subscription
        .bootstrap_root()
        .map_err(|error| format!("bootstrap root: {error}"))?;
    if view.root() != revision.root() {
        return Err("the owner returned mismatched startup identities".to_owned());
    }
    let snapshot = AppSnapshot::empty(VersionedRoot::from_revision(1, revision, 0));
    let project = LocalProjectId::from_path(&fixture.projects()[0])
        .map_err(|error| format!("project identity: {error}"))?;
    let actor = EngineActor::start(LocalEngineClient::new(&endpoint, project), 32)
        .map_err(|error| format!("engine actor: {error}"))?;
    let runtime = DesktopRuntime::new(snapshot, actor);
    let reads = ReadPool::start(3, move |_| SessionReader::connect(&endpoint))
        .map_err(|error| format!("read pool: {error}"))?;
    let graph = UiEntityGraph::install_with_reads(cx, runtime, None, Some(reads));
    // The shot's facet becomes the product's settings.
    let facet = cx.facet();
    for intent in settings_intents(&facet) {
        graph.root.update(cx, |root, cx| root.dispatch(intent, cx));
    }
    let target = route::parse(start)?;
    if target != route::Target::Orbit {
        let route = route::to_route(&target, fixture)?;
        graph
            .root
            .update(cx, |root, cx| root.dispatch(Intent::Navigate(route), cx));
    }
    gallery::declare_quiet(quiet, cx);
    gallery::declare_adapter(adapt, cx);
    gallery::declare_annotator(annotate, cx);
    gallery::declare_state(sample_state, cx);
    let shell = ROOT(&graph, window, cx);
    // Text size is a per-display zoom: set this window's display to the
    // shot's percent through the product's own intent.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = (facet.text_scale * 100.0).round() as u16;
    let display = shell.read(cx).display_key();
    graph
        .root
        .update(cx, |root, cx| root.dispatch(Intent::ZoomTo { display, percent }, cx));
    cx.set_global(Booted {
        graph,
        fixture,
        shell: shell.clone(),
        last: std::cell::Cell::new((crate::shell::RenderCounts::default(), 0)),
    });
    Ok(shell.into())
}

/// Which regions re-rendered since the previous frame, and how many reads
/// landed: what a slow frame did.
fn annotate(cx: &mut App) -> String {
    let Some(booted) = cx.try_global::<Booted>() else {
        return String::new();
    };
    let counts = booted.shell.read(cx).render_counts(cx);
    let landed = booted.graph.store.read(cx).stats().landed;
    let (before, landed_before) = booted.last.replace((counts, landed));
    let regions = [
        ("shell", counts.shell, before.shell),
        ("titlebar", counts.titlebar, before.titlebar),
        ("shelf", counts.shelf, before.shelf),
        ("reader", counts.reader, before.reader),
        ("status", counts.status, before.status),
        ("pins", counts.pins, before.pins),
        ("ask", counts.ask, before.ask),
    ]
    .into_iter()
    .filter(|(_, now, then)| now > then)
    .map(|(name, now, then)| format!("{name} x{}", now - then))
    .collect::<Vec<_>>();
    format!(
        "re-rendered [{}]{}; graph {}",
        regions.join(", "),
        if landed > landed_before {
            format!(", {} read(s) landed", landed - landed_before)
        } else {
            String::new()
        },
        booted.shell.read(cx).graph_report(cx)
    )
}

/// Samples the mounted product route and retained map after every probed draw.
fn sample_state(cx: &mut App, _: &facet::probe::Ledger) -> gallery::json::Json {
    use facet::gallery::json::Json;
    fn route_state(route: &crate::navigation::Route) -> Json {
        use crate::navigation::{OrbitRoute, Route};
        match route {
            Route::World => Json::obj([("kind", Json::str("world"))]),
            Route::Orbit(route) => Json::obj([
                ("kind", Json::str("orbit")),
                ("project", match route { OrbitRoute::Home => Json::Null, OrbitRoute::Project(project) => Json::num(project.get().get() as f64) }),
            ]),
            Route::Package(route) => Json::obj([
                ("kind", Json::str("package")), ("package", Json::str(route.package.as_str())),
                ("lane", Json::str(format!("{:?}", route.lane))),
                ("release", route.at.as_ref().map_or(Json::Null, |at| Json::str(at.as_str()))),
            ]),
            Route::Symbol(route) => Json::obj([
                ("kind", Json::str("symbol")), ("package", Json::str(route.package.as_str())),
                ("symbol", Json::str(route.id.as_str())), ("view", Json::str(route.view.as_str())),
                ("release", route.at.as_ref().map_or(Json::Null, |at| Json::str(at.as_str()))),
                ("line", Json::opt(route.line)),
            ]),
        }
    }
    let Some(booted) = cx.try_global::<Booted>() else { return Json::Null };
    let snapshot = booted.graph.store.read(cx).snapshot();
    Json::obj([
        ("route", route_state(snapshot.route())),
        ("root", Json::str(format!("{:?}", snapshot.key()))),
        ("back", Json::num(snapshot.session().back.len() as f64)),
        ("graph", booted.shell.read(cx).graph_state(cx)),
    ])
}

fn settings_intents(facet: &facet::Facet) -> Vec<Intent> {
    vec![
        Intent::SetAppearance(match facet.appearance {
            facet::Appearance::Abyss => AppearancePreference::Abyss,
            facet::Appearance::Glacier => AppearancePreference::Glacier,
        }),
        Intent::SetDensity(match facet.density {
            facet::Density::Comfortable => DensityPreference::Comfortable,
            facet::Density::Compact => DensityPreference::Compact,
            facet::Density::Dense => DensityPreference::Dense,
        }),
        Intent::SetContrast(match facet.contrast {
            facet::Contrast::Normal => ContrastPreference::Normal,
            facet::Contrast::High => ContrastPreference::High,
        }),
        Intent::SetMotion(if facet.reduced_motion {
            MotionPreference::Reduced
        } else {
            MotionPreference::Full
        }),
    ]
}

/// Nothing in flight: the read pool is empty, every finished read has
/// landed (drained here), and the root has no engine work pending.
fn quiet(cx: &mut App) -> bool {
    let Some(booted) = cx.try_global::<Booted>() else {
        return true;
    };
    let (store, root, shell) = (booted.graph.store.clone(), booted.graph.root.clone(), booted.shell.clone());
    store.update(cx, |store, cx| {
        store.drain(cx);
    });
    let idle_pool = store.read(cx).pool_load() == (0, 0);
    idle_pool && !root.read(cx).has_pending_work() && shell.read(cx).graph_ready(cx)
}

/// Script acts without a platform event, through the product: settings
/// become intents, `route` navigates. ⌘/⌥ holds arrive as real modifier
/// events and the shell reveals on its own.
fn adapt(act: &Act, _window: &mut Window, cx: &mut App) {
    let Some(booted) = cx.try_global::<Booted>() else {
        return;
    };
    let (root, fixture, shell) = (booted.graph.root.clone(), booted.fixture, booted.shell.clone());
    let intent = match act {
        Act::TextScale { percent } => Intent::ZoomTo {
            display: shell.read(cx).display_key(),
            percent: *percent,
        },
        Act::Density { name } => Intent::SetDensity(match name.as_str() {
            "compact" => DensityPreference::Compact,
            "dense" => DensityPreference::Dense,
            _ => DensityPreference::Comfortable,
        }),
        Act::Theme { name } => Intent::SetAppearance(if name == "glacier" {
            AppearancePreference::Glacier
        } else {
            AppearancePreference::Abyss
        }),
        Act::Contrast { name } => Intent::SetContrast(if name == "high" {
            ContrastPreference::High
        } else {
            ContrastPreference::Normal
        }),
        Act::Motion { on } => Intent::SetMotion(if *on {
            MotionPreference::Full
        } else {
            MotionPreference::Reduced
        }),
        Act::Route { target } => {
            match route::parse(target).and_then(|target| route::to_route(&target, fixture)) {
                Ok(route) => Intent::Navigate(route),
                Err(error) => panic!("route {target}: {error}"),
            }
        }
        _ => return,
    };
    root.update(cx, |root, cx| root.dispatch(intent, cx));
}

fn build(start: &'static str, window: &mut Window, cx: &mut App) -> AnyView {
    match boot(start, window, cx) {
        Ok(view) => view,
        Err(error) => panic!("desktop boot at `{start}`: {error}"),
    }
}

/// Every desktop scene: the real shell on the fixture, booted at a route.
#[must_use]
pub fn scenes() -> Vec<Scene> {
    vec![
        Scene {
            id: "desktop-orbit",
            title: "The desktop at home (Orbit) on the fixture index",
            size: (1440, 900),
            build: |window, cx| build("orbit", window, cx),
        },
        Scene {
            id: "desktop-package",
            title: "The `present` package dossier",
            size: (1440, 900),
            build: |window, cx| build("package present", window, cx),
        },
        Scene {
            id: "desktop-symbol",
            title: "present::glyph::RelationLabel, its page",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel", window, cx),
        },
        Scene {
            id: "desktop-code",
            title: "present::glyph::RelationLabel, its source",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel view=code", window, cx),
        },
        Scene {
            id: "desktop-graph",
            title: "present::glyph::RelationLabel, its graph",
            size: (1440, 900),
            build: |window, cx| build("symbol present::glyph::RelationLabel view=graph", window, cx),
        },
        Scene {
            id: "desktop-world",
            title: "The world graph, nothing selected",
            size: (1440, 900),
            build: |window, cx| build("world", window, cx),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::route::{Target, View, parse};

    #[test]
    fn route_words_parse_into_targets_and_reject_the_rest() {
        assert_eq!(parse("orbit"), Ok(Target::Orbit));
        assert_eq!(parse("world"), Ok(Target::World));
        assert_eq!(
            parse("package present at=0.1.0"),
            Ok(Target::Package {
                id: "present".to_owned(),
                at: Some("0.1.0".to_owned()),
            })
        );
        assert!(parse("package present view=graph").is_err());
        assert_eq!(
            parse("symbol present::glyph::RelationLabel view=graph at=0.3.0"),
            Ok(Target::Symbol {
                id: "present::glyph::RelationLabel".to_owned(),
                at: Some("0.3.0".to_owned()),
                view: View::Graph,
            })
        );
        assert!(parse("symbol X view=map").is_err());
        assert!(parse("package").is_err());
        assert!(parse("elsewhere").is_err());
    }
}
