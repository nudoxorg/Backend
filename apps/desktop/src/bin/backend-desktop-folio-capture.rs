//! Headless captures of the real shell showing the package folio, over
//! fixture pages built from real crates on this machine.
//!
//! The index is not needed to draw a package page: the shell's read pool is
//! given a reader that answers a package with a dossier built from the
//! crate's unpacked source (its manifest, its public names by module, its
//! recorded releases), and the folio's own source-facts reader is handed the
//! same crate. Everything else is the product's: the shell, the store, the
//! regions, the folio body and the facet components. (The harness over a
//! real index is the other way to see it; this one needs no owner.)
//!
//! ```text
//! NUDOX_FOLIO_OUT=<dir> .local/devenv/cargo run -p backend-desktop \
//!     --features visual-harness --bin backend-desktop-folio-capture
//! ```
//!
//! `NUDOX_FOLIO_ONLY=<substring>` limits the run to matching shots.

#![cfg(feature = "visual-harness")]
#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines, missing_docs)]

use backend_desktop::core::{ErrorValue, FaultCode, LocalProjectId, PackageId, VersionedRoot};
use backend_desktop::model::pages::{
    Dependency, DependencyScope, DeclRef, Gap, GapReason, HealthModel, IndexedPackage, IngestModel, Known, OrbitModel, OutlineNode, OutlineTree,
    PackageDossier, PackageRecord, PackageRef, PageValue, ReadFailure, Readiness, RecordSource, Standing, VersionEntry,
};
use backend_desktop::model::source_facts::{self, Reading, SourceFacts, docs, registry};
use backend_desktop::model::{AppSnapshot, AppearancePreference, DensityPreference, SessionState, SettingsState};
use backend_desktop::navigation::{Intent, OrbitRoute, PackageLane, PackageRoute, ReleaseId, Route};
use backend_desktop::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use backend_desktop::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use backend_desktop::runtime::store::DataStore;
use backend_desktop::runtime::{DesktopRuntime, UiEntityGraph};
use backend_desktop::shell::Shell;
use backend_gui_harness::{AnimationFrame, CaptureError, GpuiCaptureOptions, GuiState, Viewport, capture_gpui_state_with_adapters_result_and_semantics};
use backend_library::DeclarationKind;
use gpui::{App, AppContext as _, Entity, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, InputEvent, Window, point, px};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repository root")
}

/// The engine lane is not needed to read pages.
struct Idle;

impl EngineClient for Idle {
    fn execute(&mut self, _: &EngineRequest) -> Result<EngineDto, EngineFault> {
        Err(EngineFault::Cancelled)
    }
}

// ---------------------------------------------------------------- the fixture reader

fn directory(package: &PackageRef) -> Option<PathBuf> {
    if package.is_local() {
        return Some(PathBuf::from(package.as_str()));
    }
    let rest = package.as_str().strip_prefix("pkg:cargo/")?;
    let (name, version) = rest.split_once('@')?;
    registry::source_of(name, version)
}

/// The project whose lock file the weights are read against: this
/// repository, the way an open workspace would be.
fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn facts_of(package: &PackageRef) -> Option<Arc<SourceFacts>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<SourceFacts>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache.lock().ok()?.get(package.as_str()) {
        return hit.clone();
    }
    let facts = directory(package).and_then(|dir| source_facts::read(&dir, &HashMap::new(), Some(&project_root()))).map(Arc::new);
    if std::env::var_os("NUDOX_FOLIO_DEBUG").is_some()
        && let Some(facts) = &facts
    {
        let direct: Vec<_> = facts.manifest.compiled(source_facts::manifest::DefaultFeatures::On).iter().map(|d| d.key.clone()).collect();
        let layer1: Vec<_> = facts.berg.blocks.iter().filter(|b| b.layer == 0).map(|b| format!("{}@{}:{}", b.name, b.version, b.sloc)).collect();
        eprintln!("berg {}: own {} below {} blocks {} missing {}; direct {direct:?}; layer0 {layer1:?}", facts.manifest.name, facts.berg.own, facts.berg.below, facts.berg.blocks.len(), facts.berg.missing);
    }
    cache.lock().ok()?.insert(package.as_str().to_owned(), facts.clone());
    facts
}

fn kind_of(keyword: &str) -> DeclarationKind {
    match keyword {
        "fn" => DeclarationKind::Function,
        "struct" => DeclarationKind::Struct,
        "enum" => DeclarationKind::Enum,
        "trait" => DeclarationKind::Trait,
        "type" => DeclarationKind::Type,
        "union" => DeclarationKind::Union,
        "macro_rules!" => DeclarationKind::Macro,
        _ => DeclarationKind::Constant,
    }
}

fn file_of(module: &str) -> String {
    if module == "lib" { "src/lib.rs".to_owned() } else { format!("src/{}.rs", module.replace("::", "/")) }
}

fn outline(package: &PackageRef, modules: &[docs::Module]) -> OutlineTree {
    let roots: Vec<OutlineNode> = modules
        .iter()
        .filter(|module| module.access == docs::Access::Public)
        .filter_map(|module| {
            let file = file_of(&module.path);
            let stem = module.path.rsplit("::").next().unwrap_or(&module.path);
            let children: Vec<OutlineNode> = module
                .items
                .iter()
                .filter_map(|item| {
                    // A name re-exported here is defined in another file.
                    let file = item.from.as_deref().map_or_else(|| file.clone(), file_of);
                    let label = format!("{}::{file}:{}::{}", package.as_str(), item.line, item.name);
                    let decl = DeclRef::from_label(&label, None, Some(kind_of(item.keyword)), Some((&file, u32::try_from(item.line).ok()?)))?;
                    Some(OutlineNode { decl, children: Arc::from([]) })
                })
                .collect();
            let label = format!("{}::{file}:1::{stem}", package.as_str());
            let decl = DeclRef::from_label(&label, None, Some(DeclarationKind::Module), Some((&file, 1)))?;
            Some(OutlineNode { decl, children: children.into() })
        })
        .collect();
    OutlineTree { roots: roots.into(), complete: true }
}

fn unknown(reason: GapReason) -> Gap {
    Gap::new(reason, "")
}

fn dossier(package: &PackageRef) -> Option<PackageDossier> {
    let facts = facts_of(package)?;
    let manifest = &facts.manifest;
    let local = package.is_local();
    let versions: Vec<VersionEntry> = if local {
        Vec::new()
    } else {
        let pinned = package.version().unwrap_or_default().to_owned();
        registry::releases(&manifest.name)
            .into_iter()
            .filter(|release| registry::source_of(&manifest.name, &release.version).is_some())
            .filter_map(|release| {
                Some(VersionEntry {
                    package: package.at(&release.version)?,
                    version: Arc::from(release.version.as_str()),
                    standing: match release.standing {
                        facet::folio::state::Standing::Yanked => Standing::Yanked,
                        facet::folio::state::Standing::Available => Standing::Available,
                    },
                    current: release.version == pinned,
                })
            })
            .collect()
    };
    let dependencies: Vec<Dependency> = manifest
        .dependencies
        .iter()
        .filter(|dep| dep.stage == source_facts::manifest::Stage::Runtime)
        .map(|dep| Dependency {
            name: Arc::from(dep.package.as_str()),
            requirement: Arc::from(dep.req.as_str()),
            scope: if dep.need == source_facts::manifest::Need::Optional { DependencyScope::Optional } else { DependencyScope::Runtime },
            optional: dep.need == source_facts::manifest::Need::Optional,
            resolved: registry::pick(&dep.package, &dep.req, None).and_then(|v| PackageRef::parse(&format!("pkg:cargo/{}@{v}", dep.package)).ok()),
        })
        .collect();
    Some(PackageDossier {
        package: package.clone(),
        record: Known::Known(PackageRecord {
            package: package.clone(),
            source: if local { RecordSource::LocalManifest } else { RecordSource::Registry },
            name: Arc::from(manifest.name.as_str()),
            version: Known::Known(Arc::from(manifest.version.as_str())),
            ecosystem: Known::Known(Arc::from("cargo")),
            standing: if local { Known::Unknown(unknown(GapReason::LocalProject)) } else { Known::Known(Standing::Available) },
            downloads: Known::Unknown(unknown(GapReason::NotRecorded)),
            bytes: Known::Unknown(unknown(GapReason::NotRecorded)),
            advisory: Known::Unknown(unknown(if local { GapReason::LocalProject } else { GapReason::Unconfigured })),
            description: manifest.description.as_deref().map_or_else(|| Known::Unknown(unknown(GapReason::NotCaptured)), |d| Known::Known(Arc::from(d))),
            license: manifest.license.as_deref().map_or_else(|| Known::Unknown(unknown(GapReason::NotCaptured)), |l| Known::Known(Arc::from(l))),
        }),
        versions: if local { Known::Unknown(unknown(GapReason::LocalProject)) } else { Known::Known(versions.into()) },
        dependencies: Known::Known(dependencies.into()),
        dependents: Known::Unknown(unknown(GapReason::NotServed)),
        outline: Known::Known(outline(package, &facts.modules)),
        readme: Known::Unknown(unknown(GapReason::NotCaptured)),
    })
}

struct Fixtures;

impl PageReader for Fixtures {
    fn read(&mut self, request: &ReadRequest, _: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let fault = |words: &str| ReadFailure::Fault(ErrorValue::new(FaultCode::Transport, words));
        match request {
            ReadRequest::Package(package) => dossier(package).map(PageValue::Package).ok_or_else(|| fault("no such package on this machine")),
            ReadRequest::Orbit => Ok(PageValue::Orbit(OrbitModel {
                indexed: Known::Known(Arc::from(Vec::<IndexedPackage>::new())),
                projects: Known::Known(Arc::from([])),
                explore: Known::Unknown(unknown(GapReason::NotServed)),
                tree: Known::Unknown(unknown(GapReason::NotServed)),
            })),
            ReadRequest::Health => Ok(PageValue::Health(HealthModel {
                lanes: backend_present::CoverageLine::new(&[], Some(1)),
                rows: 1,
                ingest: IngestModel { files_discovered: 1, files_indexed: 1, files_unavailable: 0, declarations: 1, languages: Arc::from([]), faults: Arc::from([]) },
                ready_capabilities: Arc::from([]),
                missing_capabilities: Arc::from([]),
            })),
            _ => Err(fault("the folio fixture serves package pages")),
        }
    }
}

// ---------------------------------------------------------------- shots

#[derive(Clone)]
enum Act {
    /// Rest the pointer on the first painted text or box whose key contains this.
    Hover(&'static str),
    /// Press and release there.
    Click(&'static str),
    /// Rest the pointer this many pixels right of and below that centre.
    HoverAt(&'static str, f32, f32),
    /// Press and release there.
    ClickAt(&'static str, f32, f32),
    /// Dispatch an intent.
    Go(Intent),
    /// Move the pointer off the page.
    Away,
}

#[derive(Clone)]
struct Shot {
    name: String,
    width: u32,
    height: u32,
    percent: u16,
    appearance: AppearancePreference,
    route: Route,
    package: PackageRef,
    frames: Vec<u64>,
    /// What to do before frame `i` is taken.
    acts: Vec<Vec<Act>>,
}

fn route_of(package: &PackageRef, at: Option<&str>) -> Route {
    Route::Package(PackageRoute {
        project: None,
        package: PackageId::new(package.as_str()).expect("package"),
        lane: PackageLane::Overview,
        selected: None,
        at: at.map(|at| ReleaseId::new(at).expect("release")),
    })
}

fn land(store: &Entity<DataStore>, cx: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        store.update(cx, |store, cx| {
            store.drain(cx);
        });
        let (queued, running) = store.read(cx).pool_load();
        let loading = store.read(cx).focused().iter().any(|key| store.read(cx).is_loading(key));
        if queued == 0 && running == 0 && !loading {
            return;
        }
        assert!(Instant::now() < deadline, "reads never landed");
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// The centre of the first text (or box) painted under a key containing `part`.
fn find(part: &str, cx: &mut App) -> Option<(f32, f32)> {
    let ledger = facet::probe::take(cx);
    // `=word`: the text that says exactly `word`.
    let matches = |content: &str, key: &str| match part.strip_prefix('=') {
        Some(word) => content == word,
        None => key.contains(part),
    };
    ledger
        .texts
        .iter()
        .find(|t| matches(&t.content, &t.key))
        .map(|t| (t.bounds.x + t.bounds.width * 0.5, t.bounds.y + t.bounds.height * 0.5))
        .or_else(|| ledger.bounds.iter().find(|b| b.key.contains(part)).map(|b| (b.x + b.width * 0.5, b.y + b.height * 0.5)))
}

fn dispatch(window: &mut Window, cx: &mut App, event: impl InputEvent) {
    window.dispatch_event(event.to_platform_input(), cx);
}

fn capture(shot: &Shot, key: VersionedRoot, out: &Path) {
    let viewport = Viewport::new(shot.width, shot.height, 1).expect("viewport");
    let frames = shot.frames.iter().enumerate().map(|(index, time)| AnimationFrame { label: format!("f{index:02}-{time}ms"), time_ms: *time }).collect::<Vec<_>>();
    let slot: Rc<RefCell<Option<(UiEntityGraph, Entity<Shell>)>>> = Rc::new(RefCell::new(None));
    let (build_slot, hook_slot, last_slot) = (Rc::clone(&slot), Rc::clone(&slot), Rc::clone(&slot));
    let last_label = frames.last().map(|frame| frame.label.clone()).unwrap_or_default();
    let (shot_build, shot_hook) = (shot.clone(), shot.clone());
    let set = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new(shot.name.clone(), None, None),
        &[],
        &frames,
        GpuiCaptureOptions { asset_source: Arc::new(facet::icons::Assets), ..GpuiCaptureOptions::default() },
        move |frame: &AnimationFrame, window: &mut Window, cx: &mut App| -> Result<(), CaptureError> {
            let (graph, _shell) = {
                let borrowed = hook_slot.borrow();
                let (graph, shell) = borrowed.as_ref().expect("built");
                (UiEntityGraph { root: graph.root.clone(), store: graph.store.clone() }, shell.clone())
            };
            let index: usize = frame.label[1..3].parse().unwrap_or(0);
            land(&graph.store, cx);
            for act in shot_hook.acts.get(index).cloned().unwrap_or_default() {
                // The frame the last act left, so positions are current.
                window.refresh();
                window.draw(cx).clear(cx);
                match act.clone() {
                    Act::Go(intent) => {
                        if let Intent::SetRelease(Some(at)) = &intent
                            && let Some(older) = shot_hook.package.at(at.as_str())
                            && let Some(facts) = facts_of(&older)
                        {
                            source_facts::install(&older, Reading::Ready(facts), cx);
                        }
                        graph.root.update(cx, |root, cx| root.dispatch(intent, cx));
                        land(&graph.store, cx);
                    }
                    Act::Hover(part) | Act::HoverAt(part, _, _) => {
                        let (dx, dy) = if let Act::HoverAt(_, dx, dy) = act { (dx, dy) } else { (0.0, 0.0) };
                        let Some((x, y)) = find(part, cx) else {
                            eprintln!("  (no `{part}` on screen)");
                            continue;
                        };
                        dispatch(window, cx, MouseMoveEvent { position: point(px(x + dx), px(y + dy)), pressed_button: None, modifiers: Modifiers::default() });
                    }
                    Act::Click(part) | Act::ClickAt(part, _, _) => {
                        let (dx, dy) = if let Act::ClickAt(_, dx, dy) = act { (dx, dy) } else { (0.0, 0.0) };
                        let Some((x, y)) = find(part, cx) else {
                            eprintln!("  (no `{part}` on screen)");
                            continue;
                        };
                        let position = point(px(x + dx), px(y + dy));
                        dispatch(window, cx, MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() });
                        window.refresh();
                        window.draw(cx).clear(cx);
                        dispatch(window, cx, MouseDownEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1, first_mouse: false });
                        dispatch(window, cx, MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 });
                    }
                    Act::Away => dispatch(window, cx, MouseMoveEvent { position: point(px(2.0), px(2.0)), pressed_button: None, modifiers: Modifiers::default() }),
                }
            }
            land(&graph.store, cx);
            Ok(())
        },
        |_, _, _| {},
        move |frame: &AnimationFrame, _, _, _, _| {
            if frame.label == last_label {
                last_slot.borrow_mut().take();
            }
            Ok(None)
        },
        move |window: &mut Window, cx: &mut App| {
            gpui_component::init(cx);
            facet::fonts::install(cx).expect("fonts");
            facet::probe::enable(cx);
            let settings = SettingsState { appearance: shot_build.appearance, shelf_open: true, ..SettingsState::default() };
            let snapshot = AppSnapshot::empty(key).with_settings(settings).with_session(SessionState {
                route: shot_build.route.clone(),
                back: vec![Route::Orbit(OrbitRoute::Home)].into(),
                ..SessionState::default()
            });
            let mut workspace = snapshot.workspace().clone();
            workspace.host = LocalProjectId::from_path(&repo().join("crates/present")).ok();
            let snapshot = snapshot.with_workspace(workspace);
            let actor = EngineActor::start(Idle, 8).expect("actor");
            let runtime = DesktopRuntime::new(snapshot, actor);
            let pool = ReadPool::start(3, |_| Fixtures).expect("pool");
            let graph = UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool));
            // What the source on disk says, read here so the page draws it at once.
            for package in [&shot_build.package] {
                if let Some(facts) = facts_of(package) {
                    source_facts::install(package, Reading::Ready(facts), cx);
                }
            }
            let shell = backend_desktop::shell::open_shell(&graph, window, cx);
            let display = shell.read(cx).display_key();
            let percent = shot_build.percent;
            graph.root.update(cx, |root, cx| root.dispatch(Intent::ZoomTo { display, percent }, cx));
            land(&graph.store, cx);
            let root = cx.new(|cx| gpui_component::Root::new(shell.clone(), window, cx).bordered(false));
            *build_slot.borrow_mut() = Some((graph, shell));
            root
        },
    )
    .expect("capture");
    let dir = out.join(&shot.name);
    std::fs::create_dir_all(&dir).expect("out dir");
    for record in &set.frames {
        let path = if set.frames.len() == 1 { out.join(format!("{}.png", shot.name)) } else { dir.join(format!("{}.png", record.label)) };
        record.image.save(&path).expect("png");
    }
    eprintln!("captured {} ({} frames)", shot.name, set.frames.len());
}

fn crate_ref(spec: &str) -> PackageRef {
    PackageRef::parse(spec).expect("package ref")
}

fn main() {
    let Ok(out) = std::env::var("NUDOX_FOLIO_OUT") else {
        eprintln!("set NUDOX_FOLIO_OUT to capture");
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("out");
    let only = std::env::var("NUDOX_FOLIO_ONLY").ok();
    let key = VersionedRoot::from_revision(1, backend_library::Cursor::at(backend_library::view_state_root(&[("folio".to_owned(), "capture".to_owned())]), 4), 0);
    let toml = crate_ref("pkg:cargo/toml@0.8.23");
    let tokio = crate_ref("pkg:cargo/tokio@1.53.1");
    let serde_json = crate_ref("pkg:cargo/serde_json@1.0.151");
    let present = PackageRef::parse(repo().join("crates/present").to_str().expect("utf-8")).expect("local package");
    let still = |name: &str, package: &PackageRef, width, height| Shot {
        name: name.to_owned(),
        width,
        height,
        percent: 100,
        appearance: AppearancePreference::Abyss,
        route: route_of(package, None),
        package: package.clone(),
        frames: vec![0, 900],
        acts: vec![vec![], vec![]],
    };
    let mut shots = Vec::new();
    for (label, package) in [("toml", &toml), ("tokio", &tokio), ("serde_json", &serde_json), ("present", &present)] {
        // Phones are taller than the fold: the whole page is what is looked at.
        for (width, height) in [(320, 2000), (390, 2000), (430, 2000), (800, 900), (1024, 700), (1440, 900), (2560, 1440)] {
            shots.push(still(&format!("{label}-{width}"), package, width, height));
        }
    }
    let staged = |name: &str, package: &PackageRef, acts: Vec<Vec<Act>>| Shot {
        frames: (0..acts.len()).map(|i| if i == 0 { 0 } else { 700 + 300 * i as u64 }).collect(),
        acts,
        // Tall enough to see what opens below the fold.
        ..still(name, package, 1440, 1500)
    };
    // Motion is eased on the harness clock, which moves before an act, not
    // after: every act is followed by a frame with nothing to do.
    shots.push(staged("tokio-region", &tokio, vec![vec![], vec![Act::Hover("region-sync::mpsc")], vec![]]));
    // The shingles of a clicked module in the air: frames 40 ms apart from the click.
    shots.push(Shot {
        frames: vec![0, 1000, 1040, 1080, 1120, 1180, 1500],
        acts: vec![vec![], vec![Act::Click("region-sync::mpsc")], vec![], vec![], vec![], vec![], vec![]],
        ..still("tokio-flight", &tokio, 1440, 1100)
    });
    // The berg dropping in: frames 40 ms apart from the click.
    shots.push(Shot {
        frames: vec![0, 1000, 1040, 1080, 1120, 1180, 1500],
        acts: vec![vec![], vec![Act::Click("weight-label")], vec![], vec![], vec![], vec![], vec![]],
        ..still("tokio-berg-open", &tokio, 1440, 900)
    });
    shots.push(staged("tokio-module", &tokio, vec![vec![], vec![Act::Click("region-sync::mpsc")], vec![Act::Away], vec![]]));
    shots.push(staged("tokio-heads", &tokio, vec![vec![], vec![Act::HoverAt("heads-label", 0.0, 27.0)], vec![]]));
    shots.push(staged("tokio-heads-sheet", &tokio, vec![vec![], vec![Act::HoverAt("heads-label", 0.0, 27.0)], vec![Act::ClickAt("heads-label", 0.0, 27.0)], vec![]]));
    shots.push(staged("tokio-berg", &tokio, vec![vec![], vec![Act::Click("weight-label")], vec![Act::Hover("berg-block-2")], vec![]]));
    shots.push(staged("tokio-stamp", &tokio, vec![vec![], vec![Act::Hover("licence-verdict")], vec![]]));
    shots.push(staged("tokio-ticker", &tokio, vec![vec![], vec![Act::Hover("bar-1.28.0")], vec![]]));
    shots.push(staged("tokio-features", &tokio, vec![vec![], vec![Act::Click("=full")], vec![Act::Away], vec![]]));
    shots.push(staged("toml-past", &toml, vec![vec![], vec![Act::Go(Intent::SetRelease(Some(ReleaseId::new("0.5.11").expect("release"))))], vec![Act::Hover("region-de")], vec![]]));
    for shot in shots {
        if only.as_ref().is_some_and(|only| !shot.name.contains(only.as_str())) {
            continue;
        }
        capture(&shot, key, &out);
    }
}
