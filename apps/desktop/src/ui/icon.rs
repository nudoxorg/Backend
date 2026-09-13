//! The icon set, embedded in the binary and rendered as an alpha mask.
//! Icons are a closed enum: a view names one, it cannot invent a path.
//! Their colour is the text colour, so they inherit whatever ink they sit in.
//!
//! Only chrome gets an icon. Anything that names a *thing* — a kind, a
//! language, a readiness — gets a lettered tile instead, because a letter
//! survives being fourteen pixels tall and a pictogram does not. That division
//! is why this set is fourteen icons and not forty.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use gpui::{AssetSource, Result, SharedString, Styled, Svg, px, svg};
use std::borrow::Cow;

/// Every icon this application draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    /// The omnibar's search mark.
    Search,
    /// The command-palette mark.
    Command,
    /// Add a project.
    Plus,
    /// Choose a folder.
    Folder,
    /// Close a tab or dismiss a surface.
    Close,
    /// Collapse a panel on the left, or walk history back.
    ChevronLeft,
    /// Collapse a panel on the right, or walk history forward.
    ChevronRight,
    /// Open a disclosure.
    ChevronDown,
    /// Copy exact text.
    Copy,
    /// Settings.
    Gear,
    /// The light appearance.
    Sun,
    /// The dark appearance.
    Moon,
    /// Re-read.
    Refresh,
    /// Open outside this window.
    External,
}

impl Icon {
    /// Returns the asset path this icon loads from.
    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Search => "icons/search.svg",
            Self::Command => "icons/command.svg",
            Self::Plus => "icons/plus.svg",
            Self::Folder => "icons/folder.svg",
            Self::Close => "icons/close.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Copy => "icons/copy.svg",
            Self::Gear => "icons/gear.svg",
            Self::Sun => "icons/sun.svg",
            Self::Moon => "icons/moon.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::External => "icons/external.svg",
        }
    }
}

/// Returns one icon at the default fourteen-pixel size in the dim ink.
pub(crate) fn icon(theme: &Theme, mark: Icon) -> Svg {
    sized(theme, mark, 14.0, Paint::TextDim)
}

/// Returns one icon at an explicit size and paint role.
pub(crate) fn sized(theme: &Theme, mark: Icon, side: f32, role: Paint) -> Svg {
    svg()
        .path(mark.path())
        .w(px(side))
        .h(px(side))
        .flex_none()
        .text_color(theme.paint(role))
}

/// The embedded asset source.
///
/// Icons are compiled into the binary rather than read from a bundle so the
/// application draws identically whether it is launched from a build tree, a
/// `.app`, or a test harness.
pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(bytes_for(path).map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL
            .iter()
            .map(|icon| SharedString::new_static(icon.path()))
            .collect())
    }
}

/// Every icon, for the asset listing and the preview fixtures.
pub(crate) const ALL: [Icon; 14] = [
    Icon::Search,
    Icon::Command,
    Icon::Plus,
    Icon::Folder,
    Icon::Close,
    Icon::ChevronLeft,
    Icon::ChevronRight,
    Icon::ChevronDown,
    Icon::Copy,
    Icon::Gear,
    Icon::Sun,
    Icon::Moon,
    Icon::Refresh,
    Icon::External,
];

fn bytes_for(path: &str) -> Option<&'static [u8]> {
    match path {
        "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
        "icons/command.svg" => Some(include_bytes!("../assets/icons/command.svg")),
        "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
        "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
        "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
        "icons/chevron-left.svg" => Some(include_bytes!("../assets/icons/chevron-left.svg")),
        "icons/chevron-right.svg" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
        "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
        "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
        "icons/gear.svg" => Some(include_bytes!("../assets/icons/gear.svg")),
        "icons/sun.svg" => Some(include_bytes!("../assets/icons/sun.svg")),
        "icons/moon.svg" => Some(include_bytes!("../assets/icons/moon.svg")),
        "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
        "icons/external.svg" => Some(include_bytes!("../assets/icons/external.svg")),
        _ => None,
    }
}
