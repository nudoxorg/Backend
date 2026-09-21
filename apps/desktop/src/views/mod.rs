//! Stateful render entities: the window and the panels it composes.
//! One entity owns the stores, the focus, and every action; panels only draw.
//! This is the only layer in the crate that needs a platform window to exist.

pub(crate) mod actions;
mod browse;
pub(crate) mod chrome;
mod context;
pub(crate) mod design;
mod home;
pub(crate) mod keys;
pub(crate) mod library;
mod omnibar;
mod overlays;
mod package;
mod page;
mod palette;
mod project;
mod reader;
mod source;
mod status;
pub(crate) mod workspace;

/// Shared width classification for the shell's titlebar and transient
/// surfaces. It is visual layout state, not a second navigation owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewportClass {
    /// Full labels and side rails fit in the window.
    Wide,
    /// Compact controls are needed at this width.
    Compact,
}

impl ViewportClass {
    /// The breakpoint shared by titlebar and overlays.
    pub(crate) const BREAKPOINT: f32 = 900.0;

    /// Classifies a logical window width.
    pub(crate) const fn for_width(width: f32) -> Self {
        if width < Self::BREAKPOINT {
            Self::Compact
        } else {
            Self::Wide
        }
    }

    /// Whether the compact control layout is active.
    pub(crate) const fn is_compact(self) -> bool {
        matches!(self, Self::Compact)
    }
}
