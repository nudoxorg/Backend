//! The platform seam — everything that must differ by OS, in one place.
//!
//! # Why this module exists
//!
//! lindsey grew out of a macOS-first shell, so platform differences leaked
//! into leaf widgets as `cfg!(target_os = "macos")` branches. Two of those
//! were honest (macOS symbols vs. words), one was a bug (the `platform`
//! modifier fell through to `"win "` on **Linux**, where the Super key has
//! never been called "win"), and none of them named the thing they were
//! branching on. This module is that thing, given a name.
//!
//! The migration to `gpui-ce` (which adds a Windows backend alongside macOS
//! and Linux) is what makes the seam load-bearing rather than cosmetic: there
//! are now three platforms whose presentation genuinely differs, and a fourth
//! (`wasm`) that `gpui-ce` can target. Anything that renders differently per
//! OS should read from here, not from a `cfg!` in the widget.
//!
//! # What belongs here
//!
//! * **Key-cap rendering** — [`ModifierLabels`], the glyph-vs-word choice for
//!   ⌃⌥⇧⌘ / ctrl-alt-shift-super / ctrl-alt-shift-win. The rest of key-cap
//!   formatting (the `⏎`/`⌫`/arrow symbols, the "esc is a word" rule) is
//!   platform-neutral and stays in `ui::key_hint`.
//! * **Menus** — [`has_global_menu`]. The menu *definition* is already
//!   platform-neutral: [`crate::app::lifecycle::refresh_menus`] hands the same
//!   `Vec<Menu>` to both `cx.set_menus` (macOS AppKit bar) and
//!   `gpui_component::GlobalState::set_app_menus` (the Windows/Linux
//!   `AppMenuBar`). Mounting that `AppMenuBar` in the window on non-macOS is
//!   the remaining piece.
//! * **Lifecycle** — [`supports_background_residency`]. The dock-click reopen
//!   route and `cmd-H` are macOS gestures; `app::lifecycle` gates them on this
//!   predicate so Linux/Windows fall back to gpui's default quit-on-last-window
//!   model.
//!
//! # The rule
//!
//! A `cfg(target_os = …)` branch may live *inside* this module. A `cfg!` or
//! `#[cfg]` in a view, store, or the app layer is a smell that belongs here
//! instead — with the one exception of `#[cfg(target_os = "macos")]` gating
//! for tests that only a real AppKit/Metal platform can run.

/// The modifier names a key cap should print on this platform.
///
/// macOS prints the physical-key symbols (⌃⌥⇧⌘); every other desktop
/// platform prints a word. The word forms carry a trailing space so they
/// compose next to the following key (`"shift k"`, `"super enter"`); the
/// macOS symbols do not (`⌘K`).
///
/// `const fn current()` is a compile-time constant on every target, so a
/// key-cap hot path pays no per-frame lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierLabels {
    /// The control-modifier label (⌃ or `^`).
    pub control: &'static str,
    /// The alt/option-modifier label (⌥ or `alt `).
    pub alt: &'static str,
    /// The shift-modifier label (⇧ or `shift `).
    pub shift: &'static str,
    /// The platform-modifier label (⌘, `super `, or `win `).
    pub platform: &'static str,
}

impl ModifierLabels {
    /// The labels for the platform this binary was compiled for.
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                control: "⌃",
                alt: "⌥",
                shift: "⇧",
                platform: "⌘",
            }
        }
        #[cfg(target_os = "windows")]
        {
            Self {
                control: "^",
                alt: "alt ",
                shift: "shift ",
                platform: "win ",
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self {
                control: "^",
                alt: "alt ",
                shift: "shift ",
                // The Super key is "super" on Linux (and on every other
                // non-Windows desktop), never "win" — the pre-seam code got
                // this wrong for Linux by treating non-macOS as Windows.
                platform: "super ",
            }
        }
    }
}

/// Whether this platform has a global application menu bar that keeps working
/// with the window dismissed — the macOS menu bar.
///
/// Windows has a per-window menu strip instead; Linux typically has no global
/// menu. The predicate describes *where* the menu renders, not *whether* to
/// define it: [`crate::app::lifecycle::refresh_menus`] calls `cx.set_menus` on
/// every platform, because that one `app_menus()` list feeds both the macOS
/// AppKit bar and the in-window `gpui_component::menu::AppMenuBar` that
/// Windows/Linux render. Wiring that `AppMenuBar` into the window (non-macOS)
/// is the remaining piece of the cross-platform menu story.
pub const fn has_global_menu() -> bool {
    cfg!(target_os = "macos")
}

/// Whether closing the last window leaves the process resident (macOS
/// `QuitMode::Explicit`) rather than quitting it (every other platform's
/// `QuitMode::LastWindowClosed`).
///
/// The whole "windowless but still hosting the MCP endpoint" model — the
/// subject of [`crate::app::lifecycle`] — is only reachable on macOS. On Linux
/// and Windows the window closing ends the process, so the dock-click reopen
/// route and the `cmd-H` hide gesture have no counterpart there.
pub const fn supports_background_residency() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::ModifierLabels;

    /// The platform-modifier label must never silently claim a platform it
    /// is not: Linux reads "super", Windows reads "win", macOS reads ⌘.
    #[test]
    fn platform_label_names_the_actual_key() {
        let labels = ModifierLabels::current();
        #[cfg(target_os = "macos")]
        assert_eq!(labels.platform, "⌘");
        #[cfg(target_os = "windows")]
        assert_eq!(labels.platform, "win ");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(labels.platform, "super ");
    }

    /// Word labels compose next to a key; macOS symbols do not.
    #[test]
    fn word_labels_carry_a_trailing_space_and_symbols_do_not() {
        let labels = ModifierLabels::current();
        #[cfg(target_os = "macos")]
        {
            assert_eq!(labels.alt, "⌥");
            assert_eq!(labels.shift, "⇧");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(labels.alt.ends_with(' '));
            assert!(labels.shift.ends_with(' '));
        }
    }
}
