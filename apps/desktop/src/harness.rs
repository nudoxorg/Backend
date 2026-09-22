//! Live GPUI capture adapter for the single-state desktop.
//!
//! The generic harness owns clocks, input delivery, animation sampling, and
//! artifact policy. This module owns only the production boundary: it starts
//! the same local-first host as the visible app, hydrates a real versioned
//! catalog, installs one [`UiEntityGraph`], and lets the generic driver render
//! that graph offscreen. No fixture store or second UI state model is allowed
//! here.

use backend_client::{LocalSubscriptionTransport, Session};
use backend_gui_harness::{
    AnimationFrame, CaptureConfig, CaptureError, CaptureSet, GpuiCaptureOptions, GuiState,
    ImeObservation, ImeOperation, InputError, InputStep, OverlayState, PageState,
    SemanticAnnouncement, SemanticBounds as HarnessBounds, SemanticError, SemanticNode,
    SemanticProbe, SemanticRelations, SemanticRole, SemanticState, Viewport,
    capture_gpui_state_with_timed_adapters_and_ime_result,
};
use backend_library::{SurfaceCommand, SurfaceReply};
use gpui::{App, AppContext as _, Entity, FocusHandle, Focusable as _, WeakEntity, Window};
use gpui_component::{WindowExt as _, input::AnyInputState};
use image::RgbaImage;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::DesktopHost;
use crate::core::{LocalProjectId, ResourceIdentity, VersionedRoot};
use crate::model::{AppSnapshot, CatalogState, ObjectId, ShelfItem, ShelfState};
use crate::navigation::{Coordinate, Intent, OrbitRoute, PackageLane, PackageRoute, Route};
use crate::runtime::{DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use crate::theme::Theme;

/// Captures a real product graph at deterministic animation frames.
///
/// The endpoint, project, service revision, package records, fonts, and
/// renderer are all live. A missing service or headless renderer is returned
/// as a capture error, never replaced with a screenshot fixture.
pub fn capture_live(
    config: CaptureConfig,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
) -> Result<CaptureSet, String> {
    config.validate().map_err(|error| error.to_string())?;
    let host = DesktopHost::start().map_err(|error| error.to_string())?;
    let host_project = LocalProjectId::from_path(host.project()).map_err(|error| {
        format!("the discovered workspace path cannot be represented safely: {error}")
    })?;
    let mut subscription =
        LocalSubscriptionTransport::connect(host.endpoint()).map_err(|error| error.to_string())?;
    let (view, revision) = subscription
        .bootstrap_root()
        .map_err(|error| error.to_string())?;
    if revision.root() != view.root() {
        return Err(
            "the live capture subscription returned mismatched startup identities".to_owned(),
        );
    }
    let mut session = Session::connect(host.endpoint()).map_err(|error| error.to_string())?;
    let basis = VersionedRoot::from_revision(1, revision, 0);
    let catalog = live_catalog(&mut session);
    let snapshot = snapshot_for_capture(&host_project, basis, catalog);
    let persistence = crate::model::PersistentState::at(host.data().join("desktop-state.json"));
    let actor = EngineActor::start(
        LocalEngineClient::new(host.endpoint(), host_project.clone()),
        32,
    )
    .map_err(|error| error.to_string())?;
    let runtime = DesktopRuntime::new(snapshot, actor);
    let route = route_for_state(&state, runtime.snapshot());
    let overlay = state.overlay;
    let capture_root: Rc<RefCell<Option<WeakEntity<crate::runtime::UiRootEntity>>>> =
        Rc::new(RefCell::new(None));
    let frame_root = capture_root.clone();
    let input_root = capture_root.clone();
    let focus_restore: Rc<RefCell<Option<FocusHandle>>> = Rc::new(RefCell::new(None));
    let input_focus_restore = focus_restore.clone();

    capture_gpui_state_with_timed_adapters_and_ime_result(
        config.viewport,
        state,
        actions,
        frames,
        GpuiCaptureOptions::default(),
        move |frame, _window, cx| {
            if let Some(root) = frame_root.borrow().as_ref().and_then(WeakEntity::upgrade) {
                root.update(cx, |root, _cx| {
                    root.set_capture_time(Duration::from_millis(frame.time_ms));
                });
            }
            Ok(())
        },
        move |time_ms, step, window, cx| {
            if let Some(root) = input_root.borrow().as_ref().and_then(WeakEntity::upgrade) {
                root.update(cx, |root, _cx| {
                    root.set_capture_time(Duration::from_millis(time_ms));
                });
                if let InputStep::WindowFocus { focused } = step {
                    root.update(cx, |root, cx| root.set_window_focused(*focused, cx));
                    if *focused {
                        window.activate_window();
                        if let Some(handle) = input_focus_restore.borrow_mut().take() {
                            handle.focus(window, cx);
                        }
                    } else {
                        *input_focus_restore.borrow_mut() = window.focused(cx);
                        window.blur();
                    }
                }
            }
        },
        dispatch_ime,
        |frame, image, viewport, window, cx| {
            capture_rendered_semantics(frame, image, viewport, window, cx)
        },
        move |window, cx| {
            gpui_component::init(cx);
            crate::theme::fonts::install(cx).expect("bundled capture fonts install");
            let theme = Theme::default();
            crate::theme::sync_components(cx, &theme);
            cx.set_global(theme);
            let graph = UiEntityGraph::install(cx, runtime, Some(persistence));
            let root = graph.root.clone();
            root.update(cx, |root, _cx| root.set_capture_time(Duration::ZERO));
            root.update(cx, |root, cx| root.observe_window_activation(window, cx));
            root.update(cx, |root, cx| {
                if let Some(route) = route.clone() {
                    root.queue(Intent::Navigate(route), cx);
                }
                match overlay {
                    Some(OverlayState::Palette) | Some(OverlayState::Omnibar) => {
                        root.queue(Intent::OpenCommandPalette, cx);
                    }
                    Some(
                        OverlayState::SettingsAppearance
                        | OverlayState::SettingsEditor
                        | OverlayState::SettingsAgents
                        | OverlayState::SettingsDiagnostics
                        | OverlayState::SettingsLegend
                        | OverlayState::SettingsIndex
                        | OverlayState::SettingsRegistry,
                    ) => {
                        root.queue(
                            Intent::OpenSettings(crate::navigation::SettingsPage::Appearance),
                            cx,
                        );
                    }
                    _ => {}
                }
            });
            *capture_root.borrow_mut() = Some(root.downgrade());
            cx.new(|cx| gpui_component::Root::new(root, window, cx).bordered(false))
        },
    )
    .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Default)]
struct ImeStateEvidence {
    marked_range: Option<std::ops::Range<usize>>,
    marked_text: Option<String>,
    selected_range: Option<std::ops::Range<usize>>,
}

/// Reads IME ranges from the same CE state that installed the native
/// `Window::handle_input` handler during paint. This is deliberately generic
/// over the three text editors and does not maintain a second text/selection
/// model in the harness.
fn read_ime_state<S: gpui::EntityInputHandler>(
    entity: &Entity<S>,
    window: &mut Window,
    cx: &mut App,
) -> ImeStateEvidence {
    entity.update(cx, |state, cx| {
        let marked_range = state.marked_text_range(window, cx);
        let marked_text = marked_range.clone().and_then(|range| {
            let mut adjusted_range = None;
            state.text_for_range(range, &mut adjusted_range, window, cx)
        });
        let selected_range = state
            .selected_text_range(false, window, cx)
            .map(|selection| selection.range);
        ImeStateEvidence {
            marked_range,
            marked_text,
            selected_range,
        }
    })
}

fn read_any_ime_state(
    state: &AnyInputState,
    window: &mut Window,
    cx: &mut App,
) -> Result<ImeStateEvidence, InputError> {
    match state {
        AnyInputState::Input(entity) => Ok(read_ime_state(entity, window, cx)),
        AnyInputState::Textarea(entity) => Ok(read_ime_state(entity, window, cx)),
        AnyInputState::Editor(entity) => Ok(read_ime_state(entity, window, cx)),
        AnyInputState::Otp(_) => Err(InputError::Unsupported {
            capability: "ime-marked-text".to_owned(),
            evidence: "the focused GPUI CE OtpState does not implement EntityInputHandler; marked composition cannot be expressed".to_owned(),
        }),
    }
}

fn update_ime_state<S: gpui::EntityInputHandler>(
    entity: &Entity<S>,
    step: &InputStep,
    window: &mut Window,
    cx: &mut App,
) -> ImeStateEvidence {
    entity.update(cx, |state, cx| {
        match step {
            InputStep::ImeText { value } | InputStep::ImeCommit { value } => {
                // CE's replace path closes the current marked range and opens
                // one undo transaction for this commit.
                state.replace_text_in_range(None, value, window, cx);
            }
            InputStep::ImeCompose { value } => {
                // A None range tells CE to replace its existing marked range,
                // so update-compose never appends a duplicate composition.
                state.replace_and_mark_text_in_range(None, value, None, window, cx);
            }
            InputStep::ImeCancel => {
                // Empty marked text is CE's tested cancellation path: it
                // removes the active composition without inserting text.
                state.replace_and_mark_text_in_range(None, "", None, window, cx);
            }
            _ => unreachable!("update_ime_state is only called for IME steps"),
        }
        let marked_range = state.marked_text_range(window, cx);
        let marked_text = marked_range.clone().and_then(|range| {
            let mut adjusted_range = None;
            state.text_for_range(range, &mut adjusted_range, window, cx)
        });
        let selected_range = state
            .selected_text_range(false, window, cx)
            .map(|selection| selection.range);
        ImeStateEvidence {
            marked_range,
            marked_text,
            selected_range,
        }
    })
}

/// Delivers one IME action through the focused CE input entity and returns
/// readback evidence. A missing/blurred editor is a typed unsupported result;
/// the generic driver never treats it as a successful no-op.
fn dispatch_ime(
    virtual_time_ms: u64,
    step: &InputStep,
    window: &mut Window,
    cx: &mut App,
) -> Result<ImeObservation, InputError> {
    let Some(operation) = ImeOperation::from_step(step) else {
        return Err(InputError::Ime(
            "dispatch_ime received a non-IME input step".to_owned(),
        ));
    };
    let focused = window
        .focused_input(cx)
        .ok_or_else(|| InputError::Unsupported {
            capability: "ime-focused-editor".to_owned(),
            evidence: format!("no GPUI CE input is registered at virtual time {virtual_time_ms}ms"),
        })?;
    let focus_handle = focused.focus_handle(cx);
    if !focus_handle.is_focused(window) {
        return Err(InputError::Unsupported {
            capability: "ime-focused-editor".to_owned(),
            evidence: "WindowExt::focused_input returned a stale registration whose native focus handle is blurred".to_owned(),
        });
    }
    let text_before = focused.value(cx).to_string();
    let before = read_any_ime_state(&focused, window, cx)?;
    let after = match &focused {
        AnyInputState::Input(entity) => update_ime_state(entity, step, window, cx),
        AnyInputState::Textarea(entity) => update_ime_state(entity, step, window, cx),
        AnyInputState::Editor(entity) => update_ime_state(entity, step, window, cx),
        AnyInputState::Otp(_) => unreachable!("read_any_ime_state rejects OTP before update"),
    };
    let text_after = focused.value(cx).to_string();
    Ok(ImeObservation {
        operation,
        focused: focus_handle.is_focused(window),
        text_before,
        text_after,
        marked_range_before: before.marked_range,
        marked_range_after: after.marked_range,
        marked_text_before: before.marked_text,
        marked_text_after: after.marked_text,
        selected_range_after: after.selected_range,
        commit_count: match operation {
            ImeOperation::Text | ImeOperation::Commit => 1,
            ImeOperation::Compose | ImeOperation::Cancel => 0,
        },
    })
}

/// Projects the product's per-window action frame into the generic semantic
/// evidence schema. The callback runs after the exact screenshot is rendered,
/// so the probe's hash and frame label can never refer to a different image.
/// Canonical post-layout semantic export for every desktop render root,
/// including the component gallery. Call this from the generic driver's
/// semantic callback after pixels are drawn; it consumes the shared action
/// frame and measured GPUI rectangles instead of rebuilding a JSON mirror.
pub fn capture_rendered_semantics(
    frame: &AnimationFrame,
    image: &RgbaImage,
    viewport: Viewport,
    window: &mut Window,
    cx: &mut App,
) -> Result<Option<SemanticProbe>, CaptureError> {
    let theme = cx.global::<Theme>();
    let tree = theme.action_tree(window);
    let measured_bounds = theme.action_bounds(window);
    let measured_focus_order = theme.action_focus_order(window);
    let measured_focus_owner = theme.action_focus_owner(window);
    let native_focus_owner = theme.action_native_focus_owner(window);
    if measured_focus_owner != native_focus_owner {
        return Err(CaptureError::InvalidConfig(format!(
            "declared focus owner {:?} differs from native GPUI owner {:?}",
            measured_focus_owner, native_focus_owner
        )));
    }
    let mut probe = SemanticProbe::new(
        frame.label.clone(),
        frame.time_ms,
        (viewport.width, viewport.height),
        tree.route().to_string(),
        image,
    );
    probe.modal_root = tree.modal_root().map(ToString::to_string);
    probe.focus_trap = tree.focus_trap();
    probe.restore_focus = tree.restore_focus().map(ToString::to_string);
    probe.native_focus_owner = native_focus_owner.clone();
    for action in tree.iter() {
        let role = match action.role() {
            crate::ui::components::ActionRole::Window => SemanticRole::Window,
            crate::ui::components::ActionRole::Dialog => SemanticRole::Dialog,
            crate::ui::components::ActionRole::List => SemanticRole::List,
            crate::ui::components::ActionRole::ListItem
            | crate::ui::components::ActionRole::TreeItem => SemanticRole::ListItem,
            crate::ui::components::ActionRole::Tab => SemanticRole::Tab,
            crate::ui::components::ActionRole::TextInput => SemanticRole::TextInput,
            crate::ui::components::ActionRole::Search => SemanticRole::Search,
            crate::ui::components::ActionRole::Disclosure => SemanticRole::Disclosure,
            crate::ui::components::ActionRole::Status => SemanticRole::Status,
            crate::ui::components::ActionRole::Alert => SemanticRole::Alert,
            crate::ui::components::ActionRole::Navigation => SemanticRole::Navigation,
            crate::ui::components::ActionRole::Button
            | crate::ui::components::ActionRole::Setting => SemanticRole::Button,
        };
        let states = action
            .states()
            .iter()
            .map(|state| match state {
                crate::ui::components::ActionState::Focused => SemanticState::Focused,
                crate::ui::components::ActionState::Hovered => SemanticState::Hovered,
                crate::ui::components::ActionState::Pressed => SemanticState::Pressed,
                crate::ui::components::ActionState::Selected => SemanticState::Selected,
                crate::ui::components::ActionState::Expanded => SemanticState::Expanded,
                crate::ui::components::ActionState::Disabled => SemanticState::Disabled,
                crate::ui::components::ActionState::Loading => SemanticState::Busy,
                crate::ui::components::ActionState::Invalid => SemanticState::Invalid,
                crate::ui::components::ActionState::Inert => SemanticState::Inert,
            })
            .collect();
        let bounds = semantic_bounds(action.bounds());
        let hit_target = action.hit_target_bounds().and_then(semantic_bounds);
        let action_id = action.id().to_string();
        // Convert the product frame's deferred/logical enum at this boundary
        // into the harness rectangle value. Product semantics stay independent
        // of the harness schema types.
        let measured = measured_bounds
            .get(action_id.as_str())
            .copied()
            .and_then(semantic_bounds);
        if action.is_focusable() && measured.is_none() {
            return Err(CaptureError::InvalidConfig(format!(
                "focusable action {action_id:?} remains Deferred after GPUI prepaint"
            )));
        }
        let measured_inside_viewport = measured.is_some_and(|bounds| {
            bounds.x.saturating_add(bounds.width) <= viewport.width
                && bounds.y.saturating_add(bounds.height) <= viewport.height
        });
        // A scrollable overlay may retain semantic actions outside the current
        // clipped viewport. Keep them in the app action frame for traversal,
        // but exclude them from this rendered semantic tree until GPUI brings
        // their concrete rectangle back into view.
        let rendered_visible =
            action.is_visible() && (!action.is_focusable() || measured_inside_viewport);
        let focus_order = if action.is_focusable() && rendered_visible {
            Some(
                *measured_focus_order
                    .get(action_id.as_str())
                    .ok_or_else(|| {
                        CaptureError::InvalidConfig(format!(
                            "focusable action {action_id:?} has no measured tab order"
                        ))
                    })?,
            )
        } else {
            None
        };
        let mut relations = SemanticRelations::default();
        for (kind, target) in action.relations() {
            let target = target.to_string();
            match kind {
                crate::ui::components::ActionRelation::LabelledBy => {
                    relations.labelled_by.push(target)
                }
                crate::ui::components::ActionRelation::DescribedBy => {
                    relations.described_by.push(target)
                }
                crate::ui::components::ActionRelation::Controls => relations.controls.push(target),
                crate::ui::components::ActionRelation::Owns => relations.owns.push(target),
                crate::ui::components::ActionRelation::FlowTo => relations.flow_to.push(target),
            }
        }
        if action.is_focused() && native_focus_owner.as_deref() != Some(action_id.as_str()) {
            return Err(CaptureError::InvalidConfig(format!(
                "keyboard focus owner {action_id:?} was not observed on its native GPUI control"
            )));
        }
        let node = SemanticNode {
            id: action_id,
            role,
            name: action.label().to_string(),
            description: action.description_value().map(ToString::to_string),
            value: action.value().map(ToString::to_string),
            states,
            enabled: action.is_enabled(),
            visible: rendered_visible,
            focus_order,
            parent: action.parent_value().map(ToString::to_string),
            relations,
            declared_bounds: bounds,
            declared_hit_target: hit_target,
            measured_bounds: measured,
            measured_hit_target: measured,
            keyboard: action
                .shortcut_value()
                .map(|shortcut| vec![shortcut.to_string()])
                .unwrap_or_default(),
            disabled_reason: action.disabled_reason_value().map(ToString::to_string),
        };
        if let Some(error) = action.error_value() {
            probe.announcements.push(SemanticAnnouncement {
                id: format!("{}-error", node.id),
                text: error.to_string(),
                assertive: true,
            });
        }
        probe.nodes.push(node);
    }
    probe.focused = native_focus_owner;
    probe.validate().map_err(|error| {
        let detail = match &error {
            SemanticError::OverlappingFocusTargets { left, right } => {
                let target = |id: &str| {
                    probe
                        .nodes
                        .iter()
                        .find(|node| node.id == id)
                        .and_then(|node| node.measured_hit_target)
                };
                format!(
                    "; measured {left:?}={:?}, {right:?}={:?}",
                    target(left),
                    target(right)
                )
            }
            _ => String::new(),
        };
        CaptureError::InvalidConfig(format!("{error}{detail}"))
    })?;
    Ok(Some(probe))
}

fn semantic_bounds(bounds: crate::ui::components::SemanticBounds) -> Option<HarnessBounds> {
    match bounds {
        crate::ui::components::SemanticBounds::Logical {
            x,
            y,
            width,
            height,
        } => Some(HarnessBounds {
            x,
            y,
            width,
            height,
        }),
        // A declared rectangle is optional metadata only. The capture path
        // uses `measured_bounds` from the post-layout registry as the evidence
        // required for focusable controls; no synthetic origin target is valid.
        crate::ui::components::SemanticBounds::Deferred => None,
    }
}

fn live_catalog(session: &mut Session) -> Option<CatalogState> {
    let reply = session
        .surface(SurfaceCommand::Explore {
            query: None,
            limit: 64,
        })
        .ok()?;
    let SurfaceReply::Explored(records) = reply else {
        return None;
    };
    Some(CatalogState {
        packages: records
            .iter()
            .filter_map(crate::runtime::client::package_summary)
            .collect::<Vec<_>>()
            .into(),
    })
}

fn snapshot_for_capture(
    host_project: &LocalProjectId,
    basis: VersionedRoot,
    catalog: Option<CatalogState>,
) -> AppSnapshot {
    let mut snapshot = AppSnapshot::empty(basis);
    snapshot = snapshot.with_shelf(ShelfState {
        selected: Some(ResourceIdentity::Local(host_project.clone())),
        items: Arc::from([ShelfItem {
            object: ObjectId::from_backend(host_project.key()),
            identity: ResourceIdentity::Local(host_project.clone()),
            label: host_project.as_str().into(),
        }]),
    });
    catalog.map_or(snapshot.clone(), |catalog| {
        snapshot.with_catalog(catalog, basis)
    })
}

fn route_for_state(state: &GuiState, snapshot: Arc<AppSnapshot>) -> Option<Route> {
    let package = snapshot.catalog().loaded_value()?.packages.first()?;
    let package = package.coordinate.clone();
    let selected = Some(snapshot.catalog().loaded_value()?.packages[0].object);
    match state.page {
        Some(PageState::Browse) | None => Some(Route::Orbit(OrbitRoute::Home)),
        Some(PageState::Project) => Some(Route::Orbit(OrbitRoute::Home)),
        Some(PageState::Package)
        | Some(PageState::Dependencies)
        | Some(PageState::Dependents)
        | Some(PageState::Releases)
        | Some(PageState::Security) => Some(Route::Package(PackageRoute {
            project: None,
            package,
            lane: match state.page {
                Some(PageState::Dependencies) => PackageLane::Dependencies,
                Some(PageState::Dependents) => PackageLane::Dependents,
                Some(PageState::Releases) => PackageLane::Releases,
                Some(PageState::Security) => PackageLane::Security,
                _ => PackageLane::Overview,
            },
            selected,
        })),
        Some(PageState::Declaration) | Some(PageState::Docs) | Some(PageState::Code) => {
            Some(Route::Page(crate::navigation::PageRoute {
                project: None,
                package,
                coordinate: Coordinate::new("package").ok()?,
                selected,
            }))
        }
        Some(PageState::Source) => Some(Route::Source(crate::navigation::SourceRoute {
            project: None,
            package,
            page: Coordinate::new("package").ok()?,
            line: 1,
            selected,
        })),
        Some(PageState::Graph) | Some(PageState::CodeSearch) => {
            Some(Route::Orbit(OrbitRoute::Home))
        }
    }
}

/// Deterministic capture helper for tests that need a small standard scene.
pub fn capture_default(
    viewport: Viewport,
    state: GuiState,
    frames: &[AnimationFrame],
) -> Result<CaptureSet, CaptureError> {
    capture_live(CaptureConfig::deterministic(viewport), state, &[], frames)
        .map_err(CaptureError::Gpui)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        TestAppContext, VisualTestContext, div,
    };
    use gpui_component::{
        Root,
        input::{Input, InputState},
    };

    struct ImeProbe {
        input: Entity<InputState>,
        other_focus: FocusHandle,
    }

    impl Render for ImeProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(div().track_focus(&self.other_focus))
                .child(Input::new(&self.input))
        }
    }

    #[gpui::test]
    fn ime_adapter_uses_focused_ce_state_for_compose_update_commit_cancel_and_blur(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let mut input = None;
        let mut other_focus = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let state = cx.new(|cx| InputState::new(window, cx));
                let other = cx.focus_handle();
                input = Some(state.clone());
                other_focus = Some(other.clone());
                let probe = cx.new(|_| ImeProbe {
                    input: state,
                    other_focus: other,
                });
                cx.new(|cx| Root::new(probe, window, cx))
            })
            .expect("test window")
        });
        let input = input.expect("input state");
        let other_focus = other_focus.expect("fallback focus handle");
        let mut cx = VisualTestContext::from_window(window.into(), cx);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
            input.focus_handle(cx).focus(window, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let compose = cx
            .update(|window, cx| {
                dispatch_ime(
                    0,
                    &InputStep::ImeCompose {
                        value: "n".to_owned(),
                    },
                    window,
                    cx,
                )
            })
            .expect("compose dispatch");
        assert_eq!(compose.marked_text_after.as_deref(), Some("n"));
        assert_eq!(compose.commit_count, 0);

        let update = cx
            .update(|window, cx| {
                dispatch_ime(
                    1,
                    &InputStep::ImeCompose {
                        value: "你😀".to_owned(),
                    },
                    window,
                    cx,
                )
            })
            .expect("composition update");
        assert_eq!(update.marked_text_after.as_deref(), Some("你😀"));
        assert_eq!(update.marked_range_after, Some(0..3));

        let commit = cx
            .update(|window, cx| {
                dispatch_ime(
                    2,
                    &InputStep::ImeCommit {
                        value: "你😀".to_owned(),
                    },
                    window,
                    cx,
                )
            })
            .expect("composition commit");
        assert_eq!(commit.marked_range_after, None);
        assert_eq!(commit.marked_text_after, None);
        assert_eq!(commit.commit_count, 1);

        let before_cancel = commit.text_after.clone();
        cx.update(|window, cx| {
            dispatch_ime(
                3,
                &InputStep::ImeCompose {
                    value: "候".to_owned(),
                },
                window,
                cx,
            )
            .expect("second composition");
        });
        let cancel = cx
            .update(|window, cx| dispatch_ime(4, &InputStep::ImeCancel, window, cx))
            .expect("composition cancel");
        assert_eq!(cancel.text_before, format!("{before_cancel}候"));
        assert_eq!(cancel.text_after, before_cancel);
        assert_eq!(cancel.marked_range_after, None);
        cancel
            .validate_for(&InputStep::ImeCancel)
            .expect("cancel removes only provisional marked text");

        cx.update(|window, cx| other_focus.focus(window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let rejected = cx.update(|window, cx| {
            dispatch_ime(
                5,
                &InputStep::ImeText {
                    value: "no focused editor".to_owned(),
                },
                window,
                cx,
            )
        });
        assert!(matches!(rejected, Err(InputError::Unsupported { .. })));
    }
}
