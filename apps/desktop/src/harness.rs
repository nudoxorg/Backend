//! Live GPUI capture adapter for the single-state desktop.
//!
//! The generic harness owns clocks, input delivery, animation sampling, and
//! artifact policy. This module owns only the production boundary: it starts
//! the same local-first host as the visible app, hydrates a real versioned
//! catalog, installs one [`UiEntityGraph`], and lets the generic driver render
//! that graph offscreen. No fixture store or second UI state model is allowed
//! here.

use backend_client::Session;
use backend_gui_harness::{
    AnimationFrame, CaptureConfig, CaptureError, CaptureSet, GpuiCaptureOptions, GuiState,
    InputStep, OverlayState, PageState, SemanticAnnouncement, SemanticBounds as HarnessBounds,
    SemanticNode, SemanticProbe, SemanticRelations, SemanticRole, SemanticState, Viewport,
    capture_gpui_state_with_adapters_result_and_semantics,
};
use backend_library::{SurfaceCommand, SurfaceReply};
use gpui::{App, AppContext as _, Window};
use image::RgbaImage;
use std::sync::Arc;

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
    let mut session = Session::connect(host.endpoint()).map_err(|error| error.to_string())?;
    let view = session.view().map_err(|error| error.to_string())?;
    let basis = VersionedRoot::new(view.root(), 1);
    let catalog = live_catalog(&mut session);
    let snapshot = snapshot_for_capture(&host, basis, catalog);
    let persistence = crate::model::PersistentState::at(host.data().join("desktop-state.json"));
    let actor = EngineActor::start(
        LocalEngineClient::new(host.endpoint(), host.project().to_string_lossy()),
        32,
    )
    .map_err(|error| error.to_string())?;
    let runtime = DesktopRuntime::new(snapshot, actor);
    let route = route_for_state(&state, runtime.snapshot());
    let overlay = state.overlay;

    capture_gpui_state_with_adapters_result_and_semantics(
        config.viewport,
        state,
        actions,
        frames,
        GpuiCaptureOptions::default(),
        |_frame, _window, _cx| Ok(()),
        |_step, _window, _cx| {},
        |frame, image, viewport, window, cx| {
            capture_rendered_semantics(frame, image, viewport, window, cx)
        },
        move |_window, cx| {
            gpui_component::init(cx);
            crate::theme::fonts::install(cx).expect("bundled capture fonts install");
            let theme = Theme::default();
            crate::theme::sync_components(cx, &theme);
            cx.set_global(theme);
            let graph = UiEntityGraph::install(cx, runtime, Some(persistence));
            let root = graph.root.clone();
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
            root
        },
    )
    .map_err(|error| error.to_string())
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
    let mut rendered_focus_order = 0_u32;
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
            let order = rendered_focus_order;
            rendered_focus_order = rendered_focus_order.saturating_add(1);
            if !measured_focus_order.contains_key(action_id.as_str()) {
                return Err(CaptureError::InvalidConfig(format!(
                    "focusable action {action_id:?} has no measured tab order"
                )));
            }
            Some(order)
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
    probe
        .validate()
        .map_err(|error| CaptureError::InvalidConfig(error.to_string()))?;
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
    host: &DesktopHost,
    basis: VersionedRoot,
    catalog: Option<CatalogState>,
) -> AppSnapshot {
    let mut snapshot = AppSnapshot::empty(basis);
    if let Ok(project) = LocalProjectId::from_path(host.project()) {
        snapshot = snapshot.with_shelf(ShelfState {
            selected: Some(ResourceIdentity::Local(project.clone())),
            items: Arc::from([ShelfItem {
                object: ObjectId::from_backend(backend_library::object_version(
                    host.project().to_string_lossy().as_bytes(),
                )),
                identity: ResourceIdentity::Local(project),
                label: host.project().to_string_lossy().into_owned().into(),
            }]),
        });
    }
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
