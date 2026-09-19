//! Window chrome the platform does not draw: caption buttons and the drag area.
//! macOS draws its own traffic lights over a transparent titlebar. Windows,
//! asked for a transparent titlebar, draws nothing at all — so this window
//! draws minimise, maximise, and close itself, at the sizes and in the order
//! a Windows user expects, and marks the rest of the titlebar as the handle.
//!
//! Linux keeps the server's decorations; a Linux desktop has opinions about
//! its own titlebars and this application does not argue with them.

use super::workspace::Workspace;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::ui::icon::{self, Icon};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, WindowControlArea, div, px,
};

/// Width of one caption button, the Windows convention.
const CAPTION: f32 = 36.0;

/// Which side of the titlebar the platform keeps for itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Platform {
    /// Traffic lights on the left, drawn by the system.
    Mac,
    /// Caption buttons on the right, drawn here.
    Windows,
    /// Server-side decorations; the titlebar is ordinary content.
    Linux,
}

impl Platform {
    /// Returns the platform this build runs on.
    pub(crate) const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// Returns the space kept clear at the left of the titlebar.
    pub(crate) const fn leading_gap(self) -> f32 {
        match self {
            Self::Mac => 78.0,
            Self::Windows | Self::Linux => 8.0,
        }
    }

    /// Returns whether this window draws its own caption buttons.
    pub(crate) const fn draws_controls(self) -> bool {
        matches!(self, Self::Windows)
    }

    /// Returns whether the titlebar should be marked as the drag handle.
    pub(crate) const fn owns_drag(self) -> bool {
        matches!(self, Self::Windows)
    }
}

impl Workspace {
    /// Returns the caption buttons, when this platform needs them drawn.
    pub(super) fn window_controls(
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !Platform::current().draws_controls() {
            return None;
        }
        let controls = window.window_controls();
        let maximized = window.is_maximized();
        Some(
            div()
                .flex_none()
                .h_full()
                .flex()
                .items_stretch()
                .when(controls.minimize, |row| {
                    row.child(caption(theme, "win-min", Icon::Minimize, WindowControlArea::Min, false)
                        .on_click(cx.listener(|_, _, window, _| window.minimize_window())))
                })
                .when(controls.maximize, |row| {
                    row.child(
                        caption(
                            theme,
                            "win-max",
                            if maximized { Icon::Restore } else { Icon::Maximize },
                            WindowControlArea::Max,
                            false,
                        )
                        .on_click(cx.listener(|_, _, window, _| window.zoom_window())),
                    )
                })
                .child(
                    caption(theme, "win-close", Icon::Close, WindowControlArea::Close, true)
                        .on_click(cx.listener(|_, _, _, cx| cx.quit())),
                )
                .into_any_element(),
        )
    }
}

fn caption(
    theme: &Theme,
    id: &'static str,
    mark: Icon,
    area: WindowControlArea,
    destructive: bool,
) -> gpui::Stateful<gpui::Div> {
    let hover = if destructive {
        theme.paint(Paint::Fault)
    } else {
        theme.paint(Paint::Hover)
    };
    div()
        .id(ElementId::Name(SharedString::new_static(id)))
        .w(px(CAPTION))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .window_control_area(area)
        .hover(move |style| style.bg(hover))
        .child(icon::sized(theme, mark, 12.0, Paint::TextDim))
}
