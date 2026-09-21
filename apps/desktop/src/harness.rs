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
    InputStep, OverlayState, PageState, Viewport, capture_gpui_state_with_adapters_result,
};
use backend_library::{SurfaceCommand, SurfaceReply};
use gpui::AppContext as _;
use std::sync::Arc;

use crate::core::{LocalProjectId, ResourceIdentity, VersionedRoot};
use crate::DesktopHost;
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

    capture_gpui_state_with_adapters_result(
        config.viewport,
        state,
        actions,
        frames,
        GpuiCaptureOptions::default(),
        |_frame, _window, _cx| Ok(()),
        |_step, _window, _cx| {},
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
