//! W-Open I1: the window never waits for the index owner.
//!
//! - `prepare` (everything `main` does before the window) returns while an
//!   owner that never answers is still "starting";
//! - a window whose owner has not answered holds its reads (none is asked
//!   at a root nobody served), shows the page as on its way, then says in
//!   the owner's own words why it could not start; "Try again" restarts the
//!   owner, and its answer fills the page at the owner's root;
//! - the owner's one-round-trip `revision()` names the same root the full
//!   subscription hydration used to (so the root the window adopts is the
//!   root the engine actor later confirms).

#![allow(clippy::expect_used, clippy::panic)]

use super::launch::prepare;
use crate::core::{ErrorValue, FaultCode, PackageId, VersionedRoot};
use crate::model::pages::{PackageRef, PageKey, PageValue, ReadFailure};
use crate::model::{
    AppSnapshot, PersistedDesktopState, PersistedPackageLane, PersistedRoute, ServiceMode,
    SessionState,
};
use crate::navigation::{PackageLane, PackageRoute, Route};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::wait;
use crate::runtime::{
    DesktopRuntime, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest, UiEntityGraph,
};
use crate::shell::tests::{Fixture, PACKAGE};
use backend_runtime::WorkspacePaths;
use gpui::{TestAppContext, VisualTestContext, point, px, size};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // `/tmp`, not `temp_dir()`: under nix the latter makes the socket path
    // longer than `sockaddr_un` allows.
    PathBuf::from("/tmp").join(format!("nx-w1-{tag}-{}-{nonce}", std::process::id()))
}

/// A project, its data directory holding a session left on a package page,
/// and an endpoint nobody will ever bind.
fn left_on_a_package_page(tag: &str) -> WorkspacePaths {
    let root = scratch(tag);
    let project = root.join("project");
    let data = root.join("data");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(
        project.join("Cargo.toml"),
        b"[package]\nname='window-first'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(&data).expect("data");
    let state = PersistedDesktopState {
        route: PersistedRoute::Package {
            project: None,
            package: "/fixture/restored".to_owned(),
            lane: PersistedPackageLane::Overview,
            at: None,
        },
        ..PersistedDesktopState::default()
    };
    std::fs::write(
        data.join("desktop-state.json"),
        serde_json::to_vec(&state).expect("state"),
    )
    .expect("write state");
    WorkspacePaths::discover(Some(project), Some(data), Some(root.with_extension("sock")))
        .expect("paths")
}

#[test]
fn the_window_is_prepared_without_waiting_for_the_owner() {
    let paths = left_on_a_package_page("prepare");
    let (sent, received) = mpsc::channel();
    // `prepare` runs beside this thread so a regression (a wait on the owner
    // before the window) fails with a measured bound instead of hanging.
    std::thread::spawn(move || {
        let started = Instant::now();
        // An owner that never answers: it is "started" and never publishes.
        let boot = prepare(Ok(paths), |_, _| None);
        let took = started.elapsed();
        sent.send((
            format!("{:?}", boot.snapshot.route()),
            boot.snapshot.key().is_unserved(),
            boot.gate.state(),
            boot.persistence.is_some(),
            took,
        ))
        .expect("send");
    });
    let (route, unserved, state, persisted, took) = received
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| {
            panic!("prepare waited on an owner that never answers: no window after 2 s")
        });
    assert!(
        route.contains("/fixture/restored"),
        "the window opens on the page it was left on, not on Home: {route}"
    );
    assert!(unserved, "no root is claimed before the owner answers");
    assert_eq!(
        state,
        OwnerState::Starting,
        "the owner is still starting when the window opens"
    );
    assert!(persisted, "the session file is kept for saving");
    assert!(took < Duration::from_millis(500), "prepare took {took:?}");
}

#[test]
fn a_window_with_no_workspace_still_opens_and_says_why() {
    let boot = prepare(
        Err("find the local workspace: HOME is not set".to_owned()),
        |_, _| panic!("no owner is started without a workspace"),
    );
    assert!(boot.snapshot.key().is_unserved());
    assert_eq!(
        boot.gate.state(),
        OwnerState::Failed("find the local workspace: HOME is not set".into())
    );
    assert!(boot.endpoint.is_none() && boot.persistence.is_none());
}

/// The engine actor's side of a window whose owner has not answered: waits
/// on the gate like `LocalEngineClient`, then confirms the basis as root.
struct GatedRoot(OwnerGate);

impl EngineClient for GatedRoot {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        self.0.wait().map_err(|message| {
            EngineFault::Failed(ErrorValue::new(FaultCode::Transport, message.to_string()))
        })?;
        if request.cancelled() {
            return Err(EngineFault::Cancelled);
        }
        match request {
            EngineRequest::Root { request, basis, .. } => Ok(EngineDto::Root {
                request: *request,
                basis: *basis,
                key: *basis,
                revision: basis.revision(),
                delta: None,
                project: None,
                catalog: None,
            }),
            _ => Err(EngineFault::Cancelled),
        }
    }
}

/// The read pool's side: waits on the gate like `SessionEngine`, then
/// answers from the shell's fixture pages.
struct GatedPages(OwnerGate);

impl PageReader for GatedPages {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        self.0.wait().map_err(|message| {
            ReadFailure::Fault(ErrorValue::new(FaultCode::Transport, message.to_string()))
        })?;
        Fixture.read(request, context)
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

/// Draws until `done` holds (reads land on real threads).
fn until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait::until(what, || {
        draw(cx);
        done(cx)
    });
}

#[gpui::test]
fn a_window_before_its_owner_holds_its_reads_then_says_why_the_owner_failed(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
        // The graph body's pinned test world: never the live world.json.
        crate::shell::bodies::graph::install_test_fixture(cx);
    });
    let gate = OwnerGate::starting();
    let package = PackageRef::parse(PACKAGE).expect("package");
    let route = Route::Package(PackageRoute {
        project: None,
        package: PackageId::new(PACKAGE).expect("package id"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    });
    let snapshot = AppSnapshot::empty(VersionedRoot::unserved()).with_session(SessionState {
        route,
        ..SessionState::default()
    });
    let actor = EngineActor::start(GatedRoot(gate.clone()), 8).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let pages = gate.clone();
    let pool = ReadPool::start(2, move |_| GatedPages(pages.clone())).expect("pool");
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(cx, runtime, None, Some(pool), Some(gate.clone()), None)
    });
    let window_graph = UiEntityGraph {
        root: graph.root.clone(),
        store: graph.store.clone(),
    };
    let window = cx.update(|cx| {
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(1440.0), px(900.0)),
                ))),
                ..gpui::WindowOptions::default()
            },
            |window, cx| crate::shell::open_shell(&window_graph, window, cx),
        )
        .expect("window")
    });
    let shell = window.root(cx).expect("shell");
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    let said = |cx: &mut VisualTestContext| -> Vec<String> {
        shell
            .read_with(cx, |shell, cx| shell.reader_text(cx))
            .into_iter()
            .map(|text| text.to_string())
            .collect()
    };
    let submitted = |cx: &mut VisualTestContext| {
        graph
            .store
            .read_with(cx, |store, _| store.stats().submitted)
    };

    // 1. The window is drawn while the owner starts: the page is on its way
    //    and no read has been asked of anyone.
    draw(cx);
    let first = said(cx);
    assert!(
        first.iter().any(|text| text.contains("on its way")),
        "the restored page is on its way while the owner starts: {first:?}"
    );
    assert_eq!(
        submitted(cx),
        0,
        "no read is asked before the owner answers"
    );

    // 2. The owner cannot start: the page says so, in the owner's words.
    gate.publish(OwnerState::Failed(
        "could not own /tmp/demo and nothing answered on /tmp/demo.sock".into(),
    ));
    until(cx, "the owner's failure is on the page", |cx| {
        said(cx)
            .iter()
            .any(|text| text.contains("could not own /tmp/demo"))
    });
    let failed = said(cx);
    assert!(
        failed.iter().any(|text| text == "READ-TRANSPORT")
            && !failed.iter().any(|text| text.contains("on its way")),
        "the page is a fault with its code, not a skeleton: {failed:?}"
    );
    assert!(
        failed
            .iter()
            .any(|text| text.contains("The index could not start. could not own /tmp/demo and nothing answered on /tmp/demo.sock")),
        "the owner's own words: {failed:?}"
    );
    assert_eq!(submitted(cx), 0, "a failed owner is never dialled");

    // 3. "Try again" restarts the owner; its answer fills the page at its root.
    graph.store.update(cx, |store, cx| {
        store.retry(PageKey::Package(package.clone()), cx)
    });
    assert_eq!(
        gate.state(),
        OwnerState::Starting,
        "Try again restarts the owner"
    );
    let key = VersionedRoot::from_revision(
        1,
        backend_library::Cursor::at(
            backend_library::view_state_root(&[("owner".to_owned(), "answered".to_owned())]),
            7,
        ),
        0,
    );
    gate.publish(OwnerState::Ready {
        key,
        mode: ServiceMode::Attached,
    });
    until(cx, "the dossier is in the store and drawn", |cx| {
        let landed = graph.store.read_with(cx, |store, _| {
            store.package(&package).loaded_value().is_some()
        });
        landed && !said(cx).iter().any(|text| text == "READ-TRANSPORT")
    });
    let served = said(cx);
    assert!(
        !served.iter().any(|text| {
            text == "READ-TRANSPORT"
                || text.contains("could not start")
                || text.contains("on its way")
        }),
        "the page is the dossier once the owner answers: {served:?}"
    );
    let (root, value_root, mode) = graph.store.read_with(cx, |store, _| {
        (
            store.snapshot().key(),
            store.package(&package).value_root(),
            store.snapshot().settings().service_mode,
        )
    });
    assert!(
        root.same_authority(key),
        "the window adopted the owner's root"
    );
    assert!(
        value_root.is_some_and(|value_root| value_root.same_authority(key)),
        "the page was asked at the owner's root, never at the unserved one"
    );
    assert_eq!(mode, ServiceMode::Attached);
    assert!(submitted(cx) >= 1);
}

#[test]
fn the_owners_revision_is_the_root_its_subscription_hydrates() {
    let root = scratch("revision");
    let project = root.join("project");
    let data = root.join("data");
    let endpoint = root.with_extension("sock");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(
        project.join("Cargo.toml"),
        b"[package]\nname='revision-proof'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("manifest");
    std::fs::write(project.join("src/lib.rs"), b"pub struct RevisionProof;\n").expect("source");
    // The owner keeps its private state under an owner-only parent.
    std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("owner-only root");
    let paths = WorkspacePaths::discover(Some(project.clone()), Some(data), Some(endpoint.clone()))
        .expect("paths");
    let host = super::lease::DesktopHost::start_with_paths(paths).expect("embedded owner");
    let mut session = backend_client::Session::connect(&endpoint).expect("session");
    // An indexed view, so the root is not the genesis one; the proof holds
    // for any root the owner serves.
    let indexed = session.index(project.to_str().expect("utf-8")).is_ok();
    let revision = |session: &mut backend_client::Session| {
        VersionedRoot::from_revision(1, session.revision().expect("revision").cursor(), 0)
    };
    let mut agreed = false;
    for _ in 0..20 {
        let before = revision(&mut session);
        let (_, cursor) = backend_client::LocalSubscriptionTransport::connect(&endpoint)
            .expect("subscription")
            .bootstrap_root()
            .expect("hydrate");
        let after = revision(&mut session);
        if before.same_authority(after) {
            assert!(
                VersionedRoot::from_revision(1, cursor, 0).same_authority(before),
                "revision() and the hydrated snapshot must name one authority (indexed: {indexed})"
            );
            eprintln!(
                "revision proof: indexed={indexed}, genesis={}",
                before.is_unserved()
            );
            agreed = true;
            break;
        }
    }
    assert!(agreed, "the owner never held still long enough to compare");
    drop(session);
    drop(host);
    let _ = std::fs::remove_dir_all(root);
}

// W-Open I2: the first frame is the page the window was left on, painted
// from the launch snapshot while the owner starts; the owner's answer
// confirms it, or redraws only what changed.

mod launch_snapshot {
    use super::*;
    use crate::model::pages::SeedEntry;
    use crate::model::pages::{DocFragment, SymbolPage};
    use crate::runtime::snapshot::{Keep, SnapshotFile, kept_keys};
    use crate::runtime::store::StoreEvent;
    use crate::shell::Shell;
    use crate::shell::tests::{dossier, page, page_route, symbol};
    use gpui::Entity;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    const NAME: &str = "RelationLabel";

    fn served(tag: &str) -> VersionedRoot {
        VersionedRoot::from_revision(
            1,
            backend_library::Cursor::at(
                backend_library::view_state_root(&[("owner".to_owned(), tag.to_owned())]),
                7,
            ),
            0,
        )
    }

    fn package() -> PackageRef {
        PackageRef::parse(PACKAGE).expect("package")
    }

    /// The page as the last launch saw it: its words differ from what the
    /// owner serves now.
    fn stale_page() -> SymbolPage {
        let mut page = page(NAME);
        page.docs = Arc::from([DocFragment::Text(Arc::from(
            "Stale words from the last launch.",
        ))]);
        page
    }

    /// The fixture's Orbit model (the shelf lists the index from it).
    fn orbit() -> crate::model::pages::OrbitModel {
        let cancel = crate::runtime::CancellationToken::new();
        let outlines = crate::runtime::reads::OutlineCache::default();
        let context = ReadContext {
            worker: 0,
            cancel: &cancel,
            outlines: &outlines,
        };
        match Fixture.read(&ReadRequest::Orbit, &context) {
            Ok(PageValue::Orbit(model)) => model,
            other => panic!("the fixture's orbit: {other:?}"),
        }
    }

    /// Saves `symbol_page`, the fixture dossier and Orbit as read at `root`.
    fn saved(tag: &str, root: VersionedRoot, symbol_page: SymbolPage) -> SnapshotFile {
        let data = scratch(tag);
        std::fs::create_dir_all(&data).expect("data");
        let file = SnapshotFile::in_data(&data);
        file.write(
            root,
            &[
                SeedEntry::Symbol(symbol(NAME), Arc::new(symbol_page)),
                SeedEntry::Package(package(), Arc::new(dossier())),
                SeedEntry::Orbit(Arc::new(orbit())),
            ],
        )
        .expect("save");
        file
    }

    /// Page reads behind the owner (like `GatedPages`) and behind a latch
    /// the test opens, recording every page they are asked for.
    #[derive(Clone, Default)]
    struct Latch(Arc<(Mutex<(bool, Vec<PageKey>)>, Condvar)>);

    impl Latch {
        fn open(&self) {
            let (state, opened) = &*self.0;
            state.lock().unwrap_or_else(PoisonError::into_inner).0 = true;
            opened.notify_all();
        }

        fn asked(&self) -> Vec<PageKey> {
            self.0
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .1
                .clone()
        }
    }

    struct Latched(OwnerGate, Latch);

    impl PageReader for Latched {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            self.0.wait().map_err(|message| {
                ReadFailure::Fault(ErrorValue::new(FaultCode::Transport, message.to_string()))
            })?;
            let (state, opened) = &*self.1.0;
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            while !state.0 {
                state = opened.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            state.1.push(match request {
                ReadRequest::Symbol(id) => PageKey::Symbol(id.clone()),
                ReadRequest::Source(id) => PageKey::Source(id.clone()),
                ReadRequest::Package(package) => PageKey::Package(package.clone()),
                ReadRequest::Orbit => PageKey::Orbit,
                _ => PageKey::Health,
            });
            drop(state);
            Fixture.read(request, context)
        }
    }

    fn symbol_key() -> PageKey {
        PageKey::Symbol(symbol(NAME))
    }

    fn package_key() -> PageKey {
        PageKey::Package(package())
    }

    struct Opened {
        latch: Latch,
        graph: UiEntityGraph,
        shell: Entity<Shell>,
        cx: &'static mut VisualTestContext,
        events: Rc<RefCell<Vec<StoreEvent>>>,
        _events: gpui::Subscription,
    }

    impl Opened {
        fn said(&mut self) -> Vec<String> {
            let shell = self.shell.clone();
            shell
                .read_with(self.cx, |shell, cx| shell.reader_text(cx))
                .into_iter()
                .map(|text| text.to_string())
                .collect()
        }

        fn says(&mut self, words: &str) -> bool {
            self.said().iter().any(|text| text.contains(words))
        }

        /// Every text the window paints, on a frame that repaints everything
        /// (the probe records what draws, and only what draws). It changes
        /// region render counts, so a test calls it outside the window it
        /// counts.
        fn painted(&mut self) -> Vec<String> {
            self.cx.update(|_, cx| facet::probe::enable(cx));
            self.cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            let ledger = self.cx.update(|_, cx| facet::probe::take(cx));
            self.cx.update(|_, cx| facet::probe::disable(cx));
            ledger.texts.into_iter().map(|text| text.content).collect()
        }

        /// Whether the page the store holds says `words` in its docs (what
        /// the reader will paint once it draws it).
        fn stored_docs_say(&mut self, words: &str) -> bool {
            self.graph.store.read_with(self.cx, |store, _| {
                store.symbol(&symbol(NAME)).loaded_value().is_some_and(|page| {
                    page.docs.iter().any(|fragment| matches!(fragment, DocFragment::Text(text) if text.contains(words)))
                })
            })
        }

        fn submitted(&mut self) -> u64 {
            self.graph
                .store
                .read_with(self.cx, |store, _| store.stats().submitted)
        }

        fn stamps(&mut self) -> (crate::model::pages::Stamp, crate::model::pages::Stamp) {
            self.graph.store.read_with(self.cx, |store, _| {
                (
                    store.stamp(&PageKey::Symbol(symbol(NAME))),
                    store.stamp(&PageKey::Package(package())),
                )
            })
        }

        fn events_for(&self, key: &PageKey) -> usize {
            self.events
                .borrow()
                .iter()
                .filter(|event| event.touches(key))
                .count()
        }
    }

    /// A window restored onto the `RelationLabel` page, whose owner has not
    /// answered, with the launch snapshot read as `main` reads it.
    fn open(
        cx: &mut TestAppContext,
        gate: &OwnerGate,
        file: &SnapshotFile,
        latched: bool,
    ) -> Opened {
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_component::init(cx);
            let _ = facet::fonts::install(cx);
            crate::shell::bodies::graph::install_test_fixture(cx);
        });
        let route = page_route(NAME);
        let keep = Keep {
            file: file.clone(),
            seed: file.read(&kept_keys(&route)),
        };
        let snapshot = AppSnapshot::empty(VersionedRoot::unserved()).with_session(SessionState {
            route,
            ..SessionState::default()
        });
        let actor = EngineActor::start(GatedRoot(gate.clone()), 8).expect("actor");
        let runtime = DesktopRuntime::new(snapshot, actor);
        let pages = gate.clone();
        let latch = Latch::default();
        if !latched {
            latch.open();
        }
        let reads = latch.clone();
        let pool =
            ReadPool::start(2, move |_| Latched(pages.clone(), reads.clone())).expect("pool");
        let graph = cx.update(|cx| {
            UiEntityGraph::install_with_owner(
                cx,
                runtime,
                None,
                Some(pool),
                Some(gate.clone()),
                Some(keep),
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let heard = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&graph.store, move |_, event: &StoreEvent, _| {
                heard.borrow_mut().push(event.clone())
            })
        });
        let window_graph = UiEntityGraph {
            root: graph.root.clone(),
            store: graph.store.clone(),
        };
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(1440.0), px(900.0)),
                    ))),
                    ..gpui::WindowOptions::default()
                },
                |window, cx| crate::shell::open_shell(&window_graph, window, cx),
            )
            .expect("window")
        });
        let shell = window.root(cx).expect("shell");
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        Opened {
            latch,
            graph,
            shell,
            cx,
            events,
            _events: subscription,
        }
    }

    /// Draws what changed, as the platform would (no forced refresh), so
    /// region render counts say which regions actually redrew.
    fn paint(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.run_until_parked();
    }

    fn inflight(opened: &mut Opened) -> bool {
        opened.graph.store.read_with(opened.cx, |store, _| {
            store.is_loading(&PageKey::Symbol(symbol(NAME)))
                || store.is_loading(&PageKey::Package(package()))
        })
    }

    #[gpui::test]
    fn the_first_frame_is_the_page_the_window_was_left_on_and_its_owner_confirms_it(
        cx: &mut TestAppContext,
    ) {
        let root = served("confirmed");
        let file = saved("first-frame", root, page(NAME));
        let gate = OwnerGate::starting();
        let mut opened = open(cx, &gate, &file, false);

        // The first frame: the page itself, not a skeleton, with no owner.
        draw(opened.cx);
        let first = opened.painted();
        assert!(
            first
                .iter()
                .any(|text| text.contains("names one relation group"))
                && !opened.says("on its way"),
            "the first frame is the page the window was left on: {first:?}"
        );
        assert_eq!(
            opened.submitted(),
            0,
            "painted from the snapshot, not from a read"
        );
        let before = opened.stamps();

        // The owner serves the root the pages were read at: they are current
        // as they are, and nothing is fetched or redrawn.
        gate.publish(OwnerState::Ready {
            key: root,
            mode: ServiceMode::Attached,
        });
        draw(opened.cx);
        let adopted = opened.graph.store.read_with(opened.cx, |store, _| {
            (
                store.snapshot().key(),
                store.symbol(&symbol(NAME)).value_root(),
                store.package(&package()).value_root(),
            )
        });
        assert!(
            adopted.0.same_authority(root),
            "the window adopted the owner's root"
        );
        assert!(
            adopted.1.is_some_and(|at| at.same_authority(root))
                && adopted.2.is_some_and(|at| at.same_authority(root)),
            "the snapshot's pages are current at the root they were read at: {adopted:?}"
        );
        let asked = opened.latch.asked();
        assert!(
            !asked.contains(&symbol_key()) && !asked.contains(&package_key()),
            "a confirmed snapshot is never fetched again: {asked:?}"
        );
        assert_eq!(opened.stamps(), before, "and nothing it drew moved");
        assert_eq!(
            (
                opened.events_for(&symbol_key()),
                opened.events_for(&package_key())
            ),
            (0, 0),
            "and nothing woke for it"
        );
    }

    /// Answers the owner at `now` and lets its root, project and mode be
    /// adopted (the chrome redraws for those) while the page reads wait
    /// behind the latch; then opens the latch and draws, as the platform
    /// would, until the route's pages are current and nothing is in flight.
    /// Returns the region render counts just before the latch opened and
    /// once everything landed. Asserts no frame ever showed a wait.
    fn revalidate(
        opened: &mut Opened,
        gate: &OwnerGate,
        now: VersionedRoot,
        landed: &str,
    ) -> (crate::shell::RenderCounts, crate::shell::RenderCounts) {
        gate.publish(OwnerState::Ready {
            key: now,
            mode: ServiceMode::Attached,
        });
        wait::until("the owner's root was adopted", || {
            let adopted = opened.graph.store.read_with(opened.cx, |store, _| {
                store.snapshot().key().same_authority(now)
            });
            if !adopted {
                paint(opened.cx);
            }
            adopted
        });
        for _ in 0..20 {
            paint(opened.cx);
            opened
                .cx
                .executor()
                .advance_clock(Duration::from_millis(50));
        }
        assert!(
            !opened.says("on its way"),
            "a revalidation never shows a wait"
        );
        let shell = opened.shell.clone();
        let before = shell.read_with(opened.cx, |shell, cx| shell.render_counts(cx));
        opened.events.borrow_mut().clear();
        opened.latch.open();
        assert!(
            wait::poll(wait::HUNG, || {
                paint(opened.cx);
                assert!(
                    !opened.says("on its way"),
                    "a revalidation never shows a wait"
                );
                let asked = opened.latch.asked();
                opened.stored_docs_say(landed)
                    && asked.contains(&symbol_key())
                    && asked.contains(&package_key())
                    && !inflight(opened)
            }),
            "never: the owner's pages landed ({:?})",
            opened.latch.asked()
        );
        for _ in 0..5 {
            paint(opened.cx);
            opened
                .cx
                .executor()
                .advance_clock(Duration::from_millis(50));
        }
        let after = shell.read_with(opened.cx, |shell, cx| shell.render_counts(cx));
        let roots = opened.graph.store.read_with(opened.cx, |store, _| {
            (
                store.symbol(&symbol(NAME)).value_root(),
                store.package(&package()).value_root(),
            )
        });
        assert!(
            roots.0.is_some_and(|at| at.same_authority(now))
                && roots.1.is_some_and(|at| at.same_authority(now)),
            "both are current at the owner's root: {roots:?}"
        );
        (before, after)
    }

    #[gpui::test]
    fn an_unchanged_snapshot_is_revalidated_without_a_single_redraw(cx: &mut TestAppContext) {
        let file = saved("unchanged", served("last-launch"), page(NAME));
        let gate = OwnerGate::starting();
        let mut opened = open(cx, &gate, &file, true);
        paint(opened.cx);
        assert!(
            opened
                .painted()
                .iter()
                .any(|text| text.contains("names one relation group")),
            "the snapshot's page is painted"
        );
        let stamps = opened.stamps();
        let (before, after) = revalidate(
            &mut opened,
            &gate,
            served("now"),
            "names one relation group",
        );
        assert_eq!(opened.stamps(), stamps, "no stamp moved");
        assert_eq!(
            (
                opened.events_for(&symbol_key()),
                opened.events_for(&package_key())
            ),
            (0, 0),
            "no event woke anything"
        );
        assert_eq!(
            (after.reader, after.shelf, after.titlebar, after.status),
            (before.reader, before.shelf, before.titlebar, before.status),
            "no region redrew when the owner confirmed the pages: {before:?} -> {after:?}; events {:?}; asked {:?}",
            opened.events.borrow(),
            opened.latch.asked()
        );
        assert!(
            opened
                .painted()
                .iter()
                .any(|text| text.contains("names one relation group")),
            "and the page is still what it painted"
        );
    }

    #[gpui::test]
    fn a_stale_row_becomes_ready_without_redrawing_unchanged_neighbours(cx: &mut TestAppContext) {
        let file = saved("stale", served("last-launch"), stale_page());
        let gate = OwnerGate::starting();
        let mut opened = open(cx, &gate, &file, true);
        paint(opened.cx);
        let stale = opened.painted();
        assert!(
            stale
                .iter()
                .any(|text| text.contains("Stale words from the last launch.")),
            "{stale:?}"
        );
        let (symbol_before, package_before) = opened.stamps();
        let now = served("now");
        let (before, after) = revalidate(&mut opened, &gate, now, "names one relation group");
        let (symbol_after, package_after) = opened.stamps();
        assert_ne!(symbol_after, symbol_before, "the changed page moved");
        assert_eq!(
            opened.events_for(&symbol_key()),
            1,
            "the changed page moved once, when its new value landed (never for 'working')"
        );
        assert_eq!(
            package_after, package_before,
            "the unchanged dossier's stamp did not move"
        );
        assert_eq!(
            opened.events_for(&package_key()),
            0,
            "and no event woke anything for it"
        );
        assert_eq!(
            after.reader - before.reader,
            1,
            "the reader redrew once, for the changed page: {before:?} -> {after:?}"
        );
        assert_eq!(
            (after.shelf, after.status),
            (before.shelf, before.status),
            "the shelf (drawn from the unchanged dossier and Orbit) and the status line did not redraw: {before:?} -> {after:?}"
        );
        eprintln!("stale row: region renders {before:?} -> {after:?}");
        let fresh = opened.painted();
        assert!(
            !fresh
                .iter()
                .any(|text| text.contains("Stale words from the last launch.")),
            "the stale words are gone: {fresh:?}"
        );
        assert!(
            fresh
                .iter()
                .any(|text| text.contains("names one relation group")),
            "the owner's words are painted: {fresh:?}"
        );

        // At rest, the route's pages are saved for the next launch, at the
        // owner's root, as they are now.
        opened
            .cx
            .executor()
            .advance_clock(Duration::from_millis(1_600));
        let seed = wait::until_some("the pages were saved at rest", || {
            opened.cx.run_until_parked();
            file.read(&kept_keys(&page_route(NAME)))
                .filter(|seed| seed.root.serves(now))
        });
        assert_eq!(
            seed.pages,
            [
                SeedEntry::Symbol(symbol(NAME), Arc::new(page(NAME))),
                SeedEntry::Package(package(), Arc::new(dossier())),
                SeedEntry::Orbit(Arc::new(orbit())),
            ],
            "the next launch paints what this one shows now"
        );
    }

    #[gpui::test]
    fn a_corrupt_snapshot_is_ignored_and_the_page_is_honestly_on_its_way(cx: &mut TestAppContext) {
        let file = saved("corrupt", served("corrupt"), page(NAME));
        // One letter of the page's own words: the JSON still parses, so only
        // the section hash stands between it and the first frame.
        let mut bytes = std::fs::read(file.path()).expect("bytes");
        let letter = bytes
            .windows(b"relation group".len())
            .position(|window| window == b"relation group")
            .expect("the page's words are in the file");
        bytes[letter] ^= 0x20;
        std::fs::write(file.path(), &bytes).expect("corrupt it");
        let gate = OwnerGate::starting();
        let mut opened = open(cx, &gate, &file, false);
        draw(opened.cx);
        let first = opened.painted();
        assert!(
            opened.says("on its way")
                && !first
                    .iter()
                    .any(|text| text.contains("names one relation group")),
            "a snapshot that does not check out is never painted: {first:?}"
        );
        assert!(
            file.path().with_extension("bad").exists(),
            "it is kept aside as .bad"
        );
    }

    #[gpui::test]
    fn an_owner_that_fails_leaves_the_page_as_it_was_left_and_says_so(cx: &mut TestAppContext) {
        let file = saved("failed", served("failed"), page(NAME));
        let gate = OwnerGate::starting();
        let mut opened = open(cx, &gate, &file, false);
        draw(opened.cx);
        gate.publish(OwnerState::Failed("could not own /tmp/demo".into()));
        let notice = wait::until_some("the failure was said", || {
            draw(opened.cx);
            opened.graph.store.read_with(opened.cx, |store, _| {
                store.notice().map(|notice| notice.message.to_string())
            })
        });
        assert!(
            notice.contains("as you left it") && notice.contains("could not own /tmp/demo"),
            "the window says the page is the one it was left on, and why: {notice}"
        );
        let painted = opened.painted();
        assert!(
            painted
                .iter()
                .any(|text| text.contains("names one relation group"))
                && !painted.iter().any(|text| text == "READ-TRANSPORT"),
            "the page stays, not a fault plate: {painted:?}"
        );
        assert_eq!(opened.submitted(), 0, "a failed owner is never dialled");
    }
}
