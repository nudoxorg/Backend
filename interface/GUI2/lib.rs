//! The `nudox-gui2` crate is the rewritten desktop surface: package exploration and documentation reading in one window.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! This is the bootstrap skeleton the builders replace module by module; see `DESIGN.md`.

use core::fmt;

use gpui::{App, AppContext as _, Context, IntoElement, Render, Styled as _, Window, WindowOptions, div, prelude::*};
use gpui_component::{ActiveTheme as _, Root};

/// Why the process could not open a window at all.
#[derive(Debug)]
pub enum LaunchRefusal {
    /// The platform refused to open a window.
    Window {
        /// Bounded description from the platform.
        detail: Box<str>,
    },
}

impl fmt::Display for LaunchRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Window { detail } => write!(formatter, "the window refused to open: {detail}"),
        }
    }
}

/// The one entity behind the window, to be replaced by the shell.
struct Bootstrap;

impl Render for Bootstrap {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child("Nudox")
    }
}

/// Opens the application and runs it to completion.
///
/// # Errors
///
/// Returns the exact refusal when the platform cannot open the first window.
pub fn run() -> Result<(), LaunchRefusal> {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let opened = cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| Bootstrap);
                cx.new(|cx| Root::new(view, window, cx))
            });
            if opened.is_err() {
                cx.quit();
            }
        });
    Ok(())
}
