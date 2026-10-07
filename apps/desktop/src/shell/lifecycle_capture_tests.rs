//! Renderer-backed controlled fixture evidence, never a live owner claim.

use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::pages::{
    DeclRef, Gap, GapReason, Known, MatchReason, PageValue, ReadFailure, SearchPage, SearchRow,
};
use crate::model::{AppSnapshot, ProjectPhase, SessionState, WorkspaceProject};
use crate::navigation::{BrowseRoute, OrbitRoute, Route};
use crate::runtime::owner::{OwnerFault, OwnerGate, OwnerState};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::{DesktopRuntime, EngineActor, UiEntityGraph};
use backend_gui_harness::{Act, Quiet, Session, SessionOptions, Viewport};
use gpui::{AppContext as _, Entity};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

fn words(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct CaptureReads {
    no_place: bool,
}

impl PageReader for CaptureReads {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if self.no_place
            && let ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } = request
        {
            let decl = DeclRef::from_label(
                "::Mystery",
                None,
                Some(backend_library::DeclarationKind::Struct),
                None,
            )
            .ok_or_else(|| {
                ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    "invalid controlled no-destination declaration",
                ))
            })?;
            return Ok(PageValue::Search(SearchPage {
                query: Arc::clone(&query.text),
                rows: Arc::from([SearchRow {
                    rank: 0,
                    decl,
                    package: None,
                    score: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                    signature: Known::Unknown(Gap::new(GapReason::NotServed, "")),
                    snippet: None,
                    reason: MatchReason::ExactName,
                }]),
                coverage: backend_present::CoverageLine::new(&[], None),
                next: None,
            }));
        }
        super::tests::Fixture.read(request, context)
    }
}

struct Mounted {
    session: Session,
    graph: UiEntityGraph,
    shell: Entity<super::Shell>,
}

fn open_fixture(project: Option<WorkspaceProject>, no_place: bool) -> Result<Mounted, String> {
    facet::fonts::verify().map_err(words)?;
    let root = if no_place {
        VersionedRoot::synthetic(
            backend_library::view_state_root(&[("ask".to_owned(), "native fixture".to_owned())]),
            4,
        )
    } else {
        VersionedRoot::unserved()
    };
    let mut snapshot = AppSnapshot::empty(root);
    let mut state = SessionState::default();
    if no_place {
        state.route = super::tests::page_route("RelationLabel");
    }
    snapshot = snapshot.with_session(state);
    let mut workspace = snapshot.workspace().clone();
    if let Some(project) = project {
        workspace.active = Some(project.id.clone());
        workspace.projects = Arc::from([project]);
    }
    snapshot = snapshot.with_workspace(workspace);
    let actor = EngineActor::start(super::tests::RootOnly, 8).map_err(words)?;
    let pool = ReadPool::start(2, move |_| CaptureReads { no_place }).map_err(words)?;
    let gate = (!no_place).then(|| {
        let gate = OwnerGate::starting();
        gate.publish(OwnerState::Failed(OwnerFault::Host(Arc::from(
            "controlled fixture owner unavailable",
        ))));
        gate
    });
    let mounted = Rc::new(RefCell::new(None));
    let built = Rc::clone(&mounted);
    let failure = Rc::new(RefCell::new(None));
    let font_failure = Rc::clone(&failure);
    let session = Session::open(
        Viewport::new(1440, 900, 1).map_err(words)?,
        SessionOptions {
            asset_source: Arc::new(facet::icons::Assets),
            frame_ms: 16,
            capture_native_accessibility: true,
        },
        move |window, cx| {
            gpui_component::init(cx);
            if let Err(error) = facet::fonts::install(cx) {
                *font_failure.borrow_mut() = Some(words(error));
            }
            cx.bind_keys(super::keys::bindings());
            cx.set_global(gpui::TextTrace);
            let graph = UiEntityGraph::install_with_owner(
                cx,
                DesktopRuntime::new(snapshot, actor),
                None,
                Some(pool),
                gate,
                None,
            );
            let shell = super::open_shell(&graph, window, cx);
            let closing = crate::host::close::GracefulClose::install(&graph, cx);
            crate::host::close::GracefulClose::attach(&closing, window, cx);
            let view = cx.new(|cx| crate::host::close::CloseView::new(shell.clone(), closing, cx));
            *built.borrow_mut() = Some((graph, shell));
            cx.new(|cx| gpui_component::Root::new(view, window, cx).bordered(false))
        },
    )
    .map_err(words)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(format!("native font installation: {error}"));
    }
    let (graph, shell) = mounted
        .borrow_mut()
        .take()
        .ok_or("native shell was not mounted")?;
    let mut mounted = Mounted {
        session,
        graph,
        shell,
    };
    let quiet_root = mounted.graph.root.clone();
    let quiet_store = mounted.graph.store.clone();
    mounted.session.set_quiet(Some(Quiet {
        check: Box::new(move |cx| {
            quiet_root.update(cx, |root, cx| root.drain_now(cx));
            quiet_store.update(cx, |store, cx| {
                store.drain(cx);
            });
            let store = quiet_store.read(cx);
            store.pool_activity().is_idle()
                && store.focused().iter().all(|key| !store.is_loading(key))
                && !quiet_root.read(cx).has_pending_work()
        }),
        deadline: std::time::Duration::from_secs(5),
    }));
    advance(&mut mounted.session, 400)?;
    if no_place {
        mounted
            .session
            .update(|_, cx| {
                let root = mounted.graph.store.read(cx).snapshot().key();
                super::bodies::graph::install_test_fixture(root, cx);
            })
            .map_err(words)?;
    }
    Ok(mounted)
}

fn advance(session: &mut Session, duration: u64) -> Result<(), String> {
    let until = session
        .now_ms()
        .checked_add(duration)
        .ok_or("capture clock overflow")?;
    while session.now_ms() < until {
        session.advance_to(session.now_ms().saturating_add(16).min(until));
        session.frame(false).map_err(words)?;
    }
    session.quiesce().map_err(words)?;
    Ok(())
}

fn key(session: &mut Session, chord: &str) -> Result<(), String> {
    session
        .apply(
            &Act::Key {
                chord: chord.to_owned(),
            },
            &mut |_, _, _| {},
        )
        .map_err(words)?;
    advance(session, 240)
}

fn focus_reader(mounted: &mut Mounted, target: &str) -> Result<(), String> {
    let focused = mounted
        .session
        .update(|window, cx| {
            mounted
                .shell
                .read(cx)
                .reader_targets(cx)
                .focus_native(target, window, cx)
        })
        .map_err(words)?;
    if !focused {
        return Err(format!("native target is not mounted: {target}"));
    }
    Ok(())
}

fn capture(
    mounted: &mut Mounted,
    out: &Path,
    revision: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    // PNG, AccessKit and typed route/state are read inside the same App update.
    let (image, native, meta) = mounted.session.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
        let image = window.render_to_image().map_err(words)?;
        let native = window.debug_a11y_tree_json().ok_or("forced native AccessKit tree missing")?;
        let runtime = mounted.graph.root.read(cx).snapshot();
        let store = mounted.graph.store.read(cx).snapshot();
        let meta = serde_json::json!({
            "source": revision, "scope": "controlled admitted fixture; no live producer acceptance",
            "renderer": "actual platform offscreen renderer", "frame": name,
            "route": format!("{:?}", runtime.route()), "overlay": format!("{:?}", runtime.overlay()), "runtime_store_workspace_equal": runtime.workspace() == store.workspace(),
            "projects": runtime.workspace().projects.iter().map(|row| serde_json::json!({
                "id": row.id.as_str(), "phase": format!("{:?}", row.phase), "operation": row.operation,
            })).collect::<Vec<_>>(),
            "painted": window.painted_texts().iter().map(|text| text.text.to_string()).collect::<Vec<_>>(),
            "width": image.width(), "height": image.height(),
        });
        Ok::<_, String>((image, native, meta))
    }).map_err(words)??;
    image.save(out.join(format!("{name}.png"))).map_err(words)?;
    image::imageops::crop_imm(&image, 0, 0, image.width(), image.height().min(420))
        .to_image()
        .save(out.join(format!("{name}-top.png")))
        .map_err(words)?;
    std::fs::write(out.join(format!("{name}.accesskit.json")), native).map_err(words)?;
    std::fs::write(
        out.join(format!("{name}.route.json")),
        serde_json::to_vec_pretty(&meta).map_err(words)?,
    )
    .map_err(words)?;
    Ok(meta)
}

#[test]
#[ignore = "actual platform PNG/AccessKit: NUDOX_LIFECYCLE_CAPTURE_DIR and NUDOX_LIFECYCLE_CAPTURE_SOURCE required"]
fn native_partial_and_ask_fixture_frames() -> Result<(), String> {
    let out = std::path::PathBuf::from(
        std::env::var_os("NUDOX_LIFECYCLE_CAPTURE_DIR").ok_or("capture directory missing")?,
    );
    let revision = std::env::var("NUDOX_LIFECYCLE_CAPTURE_SOURCE").map_err(words)?;
    std::fs::create_dir_all(&out).map_err(words)?;
    let project = LocalProjectId::new("/fixture/mixed-lifecycle").map_err(words)?;
    let mut row = WorkspaceProject::indexing_with_id(project.clone());
    let mut operation = crate::model::index_operation::tests::claim(&project, 0x85);
    operation.observation = Some(crate::model::index_operation::tests::partially_published(
        &operation,
    )?);
    row.phase = ProjectPhase::Ready;
    row.operation = Some(operation);
    let mut partial = open_fixture(Some(row.clone()), false)?;
    capture(&mut partial, &out, &revision, "partial-headline")?;
    focus_reader(&mut partial, &format!("owner-words-{}", project.as_str()))?;
    key(&mut partial.session, "enter")?;
    let refusal = capture(&mut partial, &out, &revision, "partial-refusal")?;
    assert!(refusal["painted"].as_array().is_some_and(|words| {
        words
            .iter()
            .any(|word| word == "typescript: the compiler is unavailable.")
    }));
    focus_reader(&mut partial, &format!("orbit-tree-{}", project.as_str()))?;
    key(&mut partial.session, "enter")?;
    capture(&mut partial, &out, &revision, "partial-tree")?;
    assert_eq!(
        partial
            .session
            .update(|_, cx| partial.graph.root.read(cx).snapshot().route().clone())
            .map_err(words)?,
        Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project)))
    );
    key(&mut partial.session, "secondary-[")?;
    let back = capture(&mut partial, &out, &revision, "partial-back")?;
    assert_eq!(back["runtime_store_workspace_equal"], true);
    assert_eq!(
        partial
            .session
            .update(|_, cx| partial
                .graph
                .root
                .read(cx)
                .snapshot()
                .workspace()
                .projects
                .to_vec())
            .map_err(words)?,
        vec![row]
    );
    drop(partial);

    let mut ask = open_fixture(None, true)?;
    key(&mut ask.session, "secondary-k")?;
    ask.session
        .apply(
            &Act::Type {
                text: "mystery".to_owned(),
            },
            &mut |_, _, _| {},
        )
        .map_err(words)?;
    advance(&mut ask.session, 400)?;
    let before = capture(&mut ask, &out, &revision, "ask-before-enter")?;
    assert!(
        before["painted"]
            .as_array()
            .is_some_and(|words| words.iter().any(|word| word == "Mystery")),
        "the admitted no-destination row is painted before Enter: {before}"
    );
    key(&mut ask.session, "enter")?;
    let after = capture(&mut ask, &out, &revision, "ask-after-enter")?;
    assert_eq!(after["route"], before["route"]);
    assert_eq!(after["overlay"], before["overlay"]);
    assert_eq!(
        ask.session
            .update(|_, cx| {
                ask.shell
                    .read(cx)
                    .ask_entity()
                    .read(cx)
                    .input()
                    .read(cx)
                    .value()
                    .to_string()
            })
            .map_err(words)?,
        "mystery"
    );
    assert!(
        after["painted"]
            .as_array()
            .is_some_and(|words| words.iter().any(|word| word == "Mystery has no page yet")),
        "the refusal is actually painted in the retained query: {after}"
    );
    Ok(())
}
