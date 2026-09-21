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
use crate::{DesktopHost, Model, UnixSubscriptionTransport};
use backend_gui_harness::{
    AnimationFrame, CaptureConfig, CaptureError, CaptureSet, GpuiCaptureOptions, GuiState,
    InputStep, ThemeState, capture_gpui_state_with_adapters,
};
use gpui::{App, AppContext as _, Window};

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
    let appearance = match config.theme {
        ThemeState::Ink => crate::theme::palette::Appearance::Ink,
        ThemeState::Vellum => crate::theme::palette::Appearance::Vellum,
    };
    let reduced_motion = state.reduced_motion;
    capture_gpui_state_with_adapters(
        viewport,
        state,
        actions,
        frames,
        GpuiCaptureOptions::default(),
        |_frame, _window, _cx| {},
        move |step, _window, cx| {
            if let InputStep::Theme { value } = step {
                let appearance =
                    crate::theme::palette::Appearance::parse(value).unwrap_or(appearance);
                cx.set_global(crate::theme::Theme::new(
                    appearance,
                    crate::theme::tokens::InterfaceSize::DEFAULT,
                    reduced_motion,
                ));
                cx.set_reduce_motion(reduced_motion);
            }
        },
        move |_window: &mut Window, cx: &mut App| {
            cx.set_global(crate::theme::Theme::new(
                appearance,
                crate::theme::tokens::InterfaceSize::DEFAULT,
                reduced_motion,
            ));
            cx.set_reduce_motion(reduced_motion);
            cx.new(|cx| Workspace::new(opened, cx))
        },
    )
    .map_err(|error: CaptureError| error.to_string())
}
