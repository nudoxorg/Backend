//! Product adapter for the deterministic GPUI screenshot harness.
//!
//! Unlike generic harness tests, this path starts the real local host,
//! hydrates its authenticated immutable root, and builds the production
//! `Workspace` entity.  It is feature-gated because it intentionally reaches
//! the live service and is used by screenshot/journey binaries rather than by
//! the default unit-test suite.

use crate::store::prefs;
use crate::store::service::Endpoint;
use crate::views::workspace::{Bootstrap, Workspace};
pub use crate::views::workspace::{WorkspaceActionProbe, WorkspaceSemanticProbe};
use crate::{DesktopHost, Model, UnixSubscriptionTransport};
use backend_client::Session;
use backend_gui_harness::{
    ActionDescriptor, ActionTarget, AnimationFrame, CaptureConfig, CaptureError, CaptureSet,
    GpuiCaptureOptions, GuiState, InputStep, PageState, ThemeState,
    capture_gpui_state_with_adapters_result,
};
use backend_runtime::WorkspacePaths;
use gpui::{App, AppContext as _, Global, WeakEntity, Window};
use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Locale selected for a deterministic visual run. Product formatters may
/// read this global without consulting the host process locale.
#[derive(Clone, Debug)]
pub(crate) struct HarnessLocale(pub(crate) String);

impl Global for HarnessLocale {}

#[derive(Clone, Debug)]
pub(crate) struct HarnessDirection(pub(crate) String);

impl Global for HarnessDirection {}

/// The exact rendered frame being inspected by the semantic adapter.
#[derive(Clone, Debug)]
pub(crate) struct HarnessFrame {
    pub(crate) label: String,
    pub(crate) time_ms: u64,
}

impl Global for HarnessFrame {}

/// The most recent production input dispatched before the current frame.
#[derive(Clone, Debug)]
pub(crate) struct HarnessInput {
    pub(crate) index: usize,
    pub(crate) step: InputStep,
}

impl Global for HarnessInput {}

/// A production capture plus the semantic probes observed at every frame.
/// The probes are emitted by the live `Workspace` entity that rendered the
/// PNGs, so a manifest can be audited without trusting scenario metadata.
#[derive(Clone, Debug)]
pub struct LiveCapture {
    /// Encoded frames from the direct GPUI renderer.
    pub capture: CaptureSet,
    /// Store-derived semantic state for each frame, in capture order.
    pub semantics: Vec<WorkspaceSemanticProbe>,
    /// Production service readiness proof used before the route was applied.
    pub readiness: Option<crate::ReadinessReport>,
}

/// Returns the action inventory registered by the production desktop launch.
/// The visual suite uses this to detect drift between the documented keyboard
/// tree and the actions that GPUI will actually dispatch.
pub fn production_action_inventory() -> Vec<ActionDescriptor> {
    crate::views::actions::window_bindings()
        .into_iter()
        .map(|binding| ActionDescriptor {
            id: action_id(binding.action().name()),
            label: binding.action().name().to_owned(),
            shortcut: Some(
                binding
                    .keystrokes()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            target: action_target(binding.action().name()),
        })
        .collect()
}

fn action_target(name: &str) -> ActionTarget {
    let name = name.rsplit("::").next().unwrap_or(name);
    if name.contains("Omnibar") || name == "Complete" {
        ActionTarget::Omnibar
    } else if name.contains("Palette")
        || matches!(name, "MoveUp" | "MoveDown" | "SelectFirst" | "SelectLast")
    {
        ActionTarget::Palette
    } else if name.contains("Settings") || name.contains("Appearance") || name.contains("Motion") {
        ActionTarget::Settings
    } else if name.contains("Library") || name == "AddProject" {
        ActionTarget::Library
    } else if name.contains("Tab")
        || matches!(name, "GoBack" | "GoForward" | "OpenSource" | "OpenEditor")
    {
        ActionTarget::Reader
    } else {
        ActionTarget::Workspace
    }
}

fn action_id(name: &str) -> String {
    let name = name.rsplit("::").next().unwrap_or(name);
    let mut id = String::with_capacity(name.len());
    for (index, character) in name.chars().enumerate() {
        if character.is_ascii_uppercase() && index != 0 {
            id.push('-');
        }
        id.push(character.to_ascii_lowercase());
    }
    id
}

/// Captures a production workspace from the live authenticated local service.
///
/// The endpoint, root, cursor, transport, preferences, and host mode all come
/// from the same [`DesktopHost`] bootstrap used by the visible application.
/// A missing service, incomplete root, or unavailable direct renderer is
/// returned as an error; no fixture pixels are substituted.
pub fn capture_live_workspace(
    config: CaptureConfig,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
) -> Result<CaptureSet, String> {
    Ok(capture_live_workspace_with_semantics(config, state, actions, frames)?.capture)
}

/// Captures a production workspace and returns the semantic state observed at
/// each frame. This is the adapter entry point used by the matrix runner.
pub fn capture_live_workspace_with_semantics(
    config: CaptureConfig,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
) -> Result<LiveCapture, String> {
    capture_live_workspace_mode(config, state, actions, frames, true)
}

/// Captures a journey from the product's empty window state. The journey must
/// establish its route through real keyboard/pointer/IME input; the adapter
/// does not seed a page or overlay by calling a store method.
pub fn capture_live_workspace_journey(
    config: CaptureConfig,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
) -> Result<LiveCapture, String> {
    capture_live_workspace_mode(config, state, actions, frames, false)
}

fn capture_live_workspace_mode(
    config: CaptureConfig,
    state: GuiState,
    actions: &[InputStep],
    frames: &[AnimationFrame],
    apply_initial_state: bool,
) -> Result<LiveCapture, String> {
    config.validate().map_err(|error| error.to_string())?;
    let host = capture_host(&state)?;
    let mut transport = UnixSubscriptionTransport::connect(host.endpoint())
        .map_err(|error| format!("connect desktop subscription: {error}"))?;
    let (initial_root, _initial_cursor) = transport
        .bootstrap_root()
        .map_err(|error| format!("hydrate admitted desktop root: {error}"))?;
    let mut readiness = if apply_initial_state {
        let mut session = Session::connect(host.endpoint())
            .map_err(|error| format!("connect readiness session: {error}"))?;
        let options = crate::ReadinessOptions::from_env().map_err(|error| error.to_string())?;
        Some(
            crate::await_readiness(
                &mut session,
                host.project(),
                reader_surface(state.page),
                &initial_root,
                options,
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let (root, cursor) = transport
        .bootstrap_root()
        .map_err(|error| format!("hydrate route-ready desktop root: {error}"))?;
    if let Some(readiness) = readiness.as_mut() {
        readiness
            .pin_admitted_root(&root)
            .map_err(|error| error.to_string())?;
    }
    let basis = root.basis().root;
    let model = Model::try_new_at(root.clone(), cursor, basis)
        .map_err(|error| format!("admit desktop model: {error}"))?;
    let opened = Bootstrap {
        endpoint: Endpoint::new(host.endpoint()),
        project: host.project().to_path_buf(),
        data: host.data().to_path_buf(),
        mode: host.mode(),
        root,
        model,
        transport,
        prefs: prefs::load(host.data()),
    };
    let viewport = config.viewport;
    let appearance = match state.theme {
        ThemeState::Ink => crate::theme::palette::Appearance::Ink,
        ThemeState::Vellum => crate::theme::palette::Appearance::Vellum,
    };
    let reduced_motion = state.reduced_motion;
    let locale = config.locale.clone();
    let expected_state = Rc::new(RefCell::new(state.clone()));
    let expected_state_for_frame = Rc::clone(&expected_state);
    let expected_state_for_input = Rc::clone(&expected_state);
    let state_for_build = state.clone();
    let workspace_slot: Rc<RefCell<Option<WeakEntity<Workspace>>>> = Rc::new(RefCell::new(None));
    let frame_workspace = Rc::clone(&workspace_slot);
    let input_workspace = Rc::clone(&workspace_slot);
    let semantic_probes: Rc<RefCell<Vec<WorkspaceSemanticProbe>>> =
        Rc::new(RefCell::new(Vec::with_capacity(frames.len())));
    let semantic_probes_for_frame = Rc::clone(&semantic_probes);
    let build_error: Rc<RefCell<Option<CaptureError>>> = Rc::new(RefCell::new(None));
    let build_error_for_build = Rc::clone(&build_error);
    let build_error_for_frame = Rc::clone(&build_error);
    let final_frame_time = frames.last().map(|frame| frame.time_ms);
    capture_gpui_state_with_adapters_result(
        viewport,
        state,
        actions,
        frames,
        GpuiCaptureOptions::default(),
        move |frame, window, cx| {
            if let Some(error) = build_error_for_frame.borrow_mut().take() {
                return Err(error);
            }
            let workspace = frame_workspace
                .borrow()
                .clone()
                .ok_or_else(|| CaptureError::Gpui("workspace root was not installed".to_owned()))?;
            cx.set_global(HarnessFrame {
                label: frame.label.clone(),
                time_ms: frame.time_ms,
            });
            // The phase names are generated by the deterministic capture
            // clock, but the retarget/reversal themselves must be performed
            // by the production shell before it renders this frame.
            if !reduced_motion {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.harness_drive_animation_phase(&frame.label, cx);
                    })
                    .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            }
            let probe = workspace
                .update(cx, |workspace, cx| {
                    if apply_initial_state {
                        let expected = expected_state_for_frame.borrow().clone();
                        workspace.assert_harness_state(window, &expected, cx)
                    } else {
                        Ok(workspace.harness_semantic_probe(window, cx))
                    }
                })
                .map_err(|error| CaptureError::Gpui(error.to_string()))?
                .map_err(CaptureError::Gpui)?;
            semantic_probes_for_frame.borrow_mut().push(probe);
            if final_frame_time == Some(frame.time_ms) {
                workspace
                    .update(cx, |workspace, cx| workspace.harness_stop_live_feed(cx))
                    .map_err(|error| CaptureError::Gpui(error.to_string()))?;
            }
            Ok(())
        },
        {
            let mut input_index = 0_usize;
            move |step, window, cx| {
                cx.set_global(HarnessInput {
                    index: input_index,
                    step: step.clone(),
                });
                input_index = input_index.saturating_add(1);
                update_expected_state(&mut expected_state_for_input.borrow_mut(), step);
                if let InputStep::Theme { value } = step {
                    let appearance =
                        crate::theme::palette::Appearance::parse(value).unwrap_or(appearance);
                    if let Some(workspace) = input_workspace.borrow().clone() {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.harness_set_appearance(appearance, cx);
                        });
                    }
                    cx.set_global(crate::theme::Theme::new(
                        appearance,
                        crate::theme::tokens::InterfaceSize::DEFAULT,
                        reduced_motion,
                    ));
                    cx.set_reduce_motion(reduced_motion);
                }
                if let InputStep::Locale { value } = step {
                    cx.set_global(HarnessLocale(value.clone()));
                }
                if let InputStep::ImeText { value } | InputStep::ImeCompose { value } = step {
                    if let Some(workspace) = input_workspace.borrow().clone() {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.harness_ime_text(value, window, cx);
                        });
                    }
                }
                if let InputStep::ImeCommit { value } = step {
                    if let Some(workspace) = input_workspace.borrow().clone() {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.harness_ime_commit(value, window, cx);
                        });
                    }
                }
                if matches!(step, InputStep::ImeCancel) {
                    if let Some(workspace) = input_workspace.borrow().clone() {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.harness_ime_cancel(window, cx);
                        });
                    }
                }
            }
        },
        move |window: &mut Window, cx: &mut App| {
            // Mirror the visible launcher before the first frame. This is the
            // actual GPUI keymap, so journey keystrokes exercise production
            // action dispatch rather than a harness-side switch statement.
            gpui_component::init(cx);
            cx.bind_keys(crate::views::actions::window_bindings());
            if let Err(error) = cx.text_system().add_fonts(
                backend_gui_harness::bundled_font_bytes()
                    .into_iter()
                    .map(Cow::Borrowed)
                    .collect(),
            ) {
                *build_error_for_build.borrow_mut() = Some(CaptureError::Gpui(error.to_string()));
            }
            cx.set_global(crate::theme::Theme::new(
                appearance,
                crate::theme::tokens::InterfaceSize::DEFAULT,
                reduced_motion,
            ));
            cx.set_reduce_motion(reduced_motion);
            cx.set_global(HarnessLocale(locale));
            cx.set_global(HarnessDirection(config.text_direction.clone()));
            let workspace = cx.new(|cx| Workspace::new(opened, window, cx));
            if state_for_build.id == "onboarding" || state_for_build.id.starts_with("onboarding-") {
                workspace.update(cx, |workspace, cx| workspace.harness_set_onboarding(cx));
            }
            workspace.update(cx, |workspace, cx| {
                workspace.harness_set_reduced_motion(reduced_motion, cx);
            });
            if apply_initial_state {
                if let Err(error) = workspace.update(cx, |workspace, cx| {
                    workspace.harness_set_appearance(appearance, cx);
                    workspace.apply_harness_state(&state_for_build, window, cx)
                }) {
                    *build_error_for_build.borrow_mut() =
                        Some(CaptureError::Gpui(error.to_string()));
                }
            }
            // A visible launch focuses the workspace window before its first
            // key event. Reproduce that focus ownership in headless journeys
            // so production shortcuts are delivered to the real key context.
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.harness_focus_window(window, cx);
            });
            *workspace_slot.borrow_mut() = Some(workspace.downgrade());
            workspace
        },
    )
    .map(|capture| LiveCapture {
        capture,
        semantics: semantic_probes.borrow().clone(),
        readiness,
    })
    .map_err(|error: CaptureError| error.to_string())
}

fn reader_surface(page: Option<PageState>) -> crate::ReaderSurface {
    match page {
        None | Some(PageState::Browse) => crate::ReaderSurface::Browse,
        Some(PageState::Project) => crate::ReaderSurface::Project,
        Some(PageState::Package) => crate::ReaderSurface::Package,
        Some(PageState::Declaration) => crate::ReaderSurface::Declaration,
        Some(PageState::Source) => crate::ReaderSurface::Source,
        Some(PageState::Code) => crate::ReaderSurface::Code,
        Some(PageState::Docs) => crate::ReaderSurface::Docs,
        Some(PageState::Graph) => crate::ReaderSurface::Graph,
        Some(PageState::Dependencies) => crate::ReaderSurface::Dependencies,
        Some(PageState::Dependents) => crate::ReaderSurface::Dependents,
        Some(PageState::Releases) => crate::ReaderSurface::Releases,
        Some(PageState::Security) => crate::ReaderSurface::Security,
        Some(PageState::CodeSearch) => crate::ReaderSurface::CodeSearch,
    }
}

/// Starts the ordinary attached/embedded host for every capture, except for
/// the explicit onboarding state. That state gets a fresh, empty durable
/// workspace through the same owner lease and authenticated service path; it
/// is deliberately not a fabricated root or a preview dossier. Keeping the
/// special case keyed by the closed state id makes a complete matrix safe to
/// run beside a developer's existing live workspace.
fn capture_host(state: &GuiState) -> Result<DesktopHost, String> {
    if state.id != "onboarding" && !state.id.starts_with("onboarding-") {
        return DesktopHost::start().map_err(|error| format!("start desktop host: {error}"));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("read capture clock: {error}"))?
        .as_nanos();
    // Unix socket paths have a small platform limit. `/tmp` keeps this
    // deterministic fixture workspace short enough even under a Nix shell.
    let root = PathBuf::from("/tmp").join(format!(
        "nudox-gui-onboarding-{}-{nonce}",
        std::process::id()
    ));
    let project = root.join("starter");
    let data = root.join("workspace");
    std::fs::create_dir_all(&project)
        .map_err(|error| format!("create onboarding project: {error}"))?;
    let paths = WorkspacePaths::discover(Some(project), Some(data), None)
        .map_err(|error| format!("discover onboarding workspace: {error}"))?;
    DesktopHost::start_with_paths(paths).map_err(|error| format!("start onboarding host: {error}"))
}

fn update_expected_state(state: &mut GuiState, step: &InputStep) {
    match step {
        InputStep::Key { value } => match value.as_str() {
            "cmd-shift-p" | "ctrl-shift-p" => {
                state.overlay = Some(backend_gui_harness::OverlayState::Palette)
            }
            "cmd-," | "ctrl-," => {
                state.overlay = Some(backend_gui_harness::OverlayState::SettingsAppearance)
            }
            "cmd-k" | "ctrl-k" | "cmd-l" | "ctrl-l" => {
                state.focus = backend_gui_harness::FocusState::Omnibar;
                state.overlay = Some(backend_gui_harness::OverlayState::Omnibar);
            }
            "cmd-u" | "ctrl-u" => state.page = Some(backend_gui_harness::PageState::Source),
            "cmd-shift-h" | "ctrl-shift-h" => {
                state.page = Some(backend_gui_harness::PageState::Browse)
            }
            "escape" => state.overlay = None,
            _ => {}
        },
        InputStep::Theme { value } => {
            state.theme = match value.as_str() {
                "vellum" => ThemeState::Vellum,
                _ => ThemeState::Ink,
            };
        }
        InputStep::FocusNext | InputStep::FocusPrevious => {}
        _ => {}
    }
}
