//! Product adapter for the deterministic GPUI screenshot harness.
//!
//! Unlike generic harness tests, this path starts the real local host,
//! hydrates its authenticated immutable root, and builds the production
//! `Workspace` entity.  It is feature-gated because it intentionally reaches
//! the live service and is used by screenshot/journey binaries rather than by
//! the default unit-test suite.

use crate::store::prefs;
use crate::store::service::Endpoint;
pub use crate::views::workspace::WorkspaceSemanticProbe;
use crate::views::workspace::{Bootstrap, Workspace};
use crate::{DesktopHost, Model, UnixSubscriptionTransport};
use backend_gui_harness::{
    ActionDescriptor, ActionTarget, AnimationFrame, CaptureConfig, CaptureError, CaptureSet,
    GpuiCaptureOptions, GuiState, InputStep, ThemeState, capture_gpui_state_with_adapters_result,
};
use gpui::{App, AppContext as _, Global, WeakEntity, Window};
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

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

/// The action tree installed by the actual desktop keymap. It is populated
/// from the product bindings at capture startup, never reconstructed from a
/// handwritten harness list.
#[derive(Clone, Debug)]
pub(crate) struct HarnessActions(pub(crate) Vec<ActionDescriptor>);

impl Global for HarnessActions {}

/// A production capture plus the semantic probes observed at every frame.
/// The probes are emitted by the live `Workspace` entity that rendered the
/// PNGs, so a manifest can be audited without trusting scenario metadata.
#[derive(Clone, Debug)]
pub struct LiveCapture {
    /// Encoded frames from the direct GPUI renderer.
    pub capture: CaptureSet,
    /// Store-derived semantic state for each frame, in capture order.
    pub semantics: Vec<WorkspaceSemanticProbe>,
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
    let host = DesktopHost::start().map_err(|error| format!("start desktop host: {error}"))?;
    let mut transport = UnixSubscriptionTransport::connect(host.endpoint())
        .map_err(|error| format!("connect desktop subscription: {error}"))?;
    let (root, cursor) = transport
        .bootstrap_root()
        .map_err(|error| format!("hydrate admitted desktop root: {error}"))?;
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
    let action_tree = production_action_inventory();
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
        move |frame, _window, cx| {
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
            let probe = workspace
                .update(cx, |workspace, cx| {
                    if apply_initial_state {
                        let expected = expected_state_for_frame.borrow().clone();
                        workspace.assert_harness_state(&expected, cx)
                    } else {
                        Ok(workspace.harness_semantic_probe(cx))
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
            cx.bind_keys(
                crate::views::actions::editing_bindings()
                    .as_keybindings(Some(crate::views::actions::FIELD_CONTEXT)),
            );
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
            cx.set_global(HarnessActions(action_tree));
            let workspace = cx.new(|cx| Workspace::new(opened, cx));
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
    })
    .map_err(|error: CaptureError| error.to_string())
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
