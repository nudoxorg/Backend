//! Desktop-session identity and preflight (Linux/BSD).
//!
//! # Why a preflight exists at all
//!
//! GPUI picks its platform backend from the environment in
//! `gpui::guess_compositor`: `WAYLAND_DISPLAY` wins, else `DISPLAY`, else it
//! returns `"Headless"` — and `gpui_linux::current_platform` builds a **real,
//! fully functional headless platform** from that. Nothing fails. The process
//! starts, the engine loads the corpus, `open_window` succeeds, frames are
//! rendered into nothing, and the app sits there forever with no window and no
//! message.
//!
//! That is the single worst failure mode a desktop binary can have, because
//! every observable signal says it is working. It happens routinely on Linux
//! and nowhere else: over SSH without X forwarding, inside a container, from a
//! bare TTY, from a systemd unit that did not import the graphical session's
//! environment. macOS has no equivalent — there is always a window server.
//!
//! So a missing display is treated as what it is — a configuration error the
//! user can fix — and reported before the engine starts, with the two variables
//! actually consulted named in the message. Headless remains reachable, but
//! only by asking for it.
//!
//! # The pure core
//!
//! [`select`] takes the variable lookup as a closure so the whole decision
//! table is testable without touching the process environment; [`from_env`] is
//! the thin shell that supplies the real one. Same split as
//! [`crate::app::corpus`], for the same reason: this decision is made once, at
//! startup, and a wrong answer is expensive to diagnose from the symptom.
//!
//! **The rules here mirror `gpui::guess_compositor` exactly.** If they drift,
//! the preflight starts lying — reporting a session GPUI will not choose. The
//! tests below pin each rule to the upstream behaviour it mirrors.

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The application id, in reverse-DNS form.
///
/// This is desktop *identity*, and on Linux it is load-bearing in a way it is
/// not elsewhere. A Wayland surface carries no icon and no window class; the
/// only thing a compositor can match against an installed `.desktop` entry is
/// this string. Get it wrong and GNOME/KDE show the window as a generic
/// placeholder in the taskbar, the alt-tab switcher and the dock, and refuse to
/// group it with the launcher it was started from.
///
/// It must stay equal to the basename of the installed desktop entry
/// (`packaging/linux/org.nudox.lindsey.desktop`) and to the icon file name. The test
/// `app_id_matches_desktop_entry` checks that against the file on disk, so the
/// two cannot drift silently.
pub const APP_ID: &str = "org.nudox.lindsey";

/// Variable naming the Wayland socket. Consulted first, as GPUI does.
pub const ENV_WAYLAND_DISPLAY: &str = "WAYLAND_DISPLAY";
/// Variable naming the X11 display. Consulted only if Wayland is absent.
pub const ENV_X11_DISPLAY: &str = "DISPLAY";
/// GPUI's opt-in to the headless platform. Presence alone is enough — GPUI
/// tests `var_os(..).is_some()`, so `ZED_HEADLESS=` (empty) still counts.
pub const ENV_HEADLESS: &str = "ZED_HEADLESS";

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Which platform backend GPUI will construct for this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Session {
    /// A Wayland compositor, via `WAYLAND_DISPLAY`.
    Wayland,
    /// An X11 server, via `DISPLAY`.
    X11,
    /// Headless — deliberately requested, never inferred.
    Headless,
}

impl Session {
    /// The name GPUI's own `guess_compositor` returns for this session.
    ///
    /// Kept as a method rather than a `Display` impl so the mirroring is
    /// explicit at the call sites that log it.
    pub fn compositor_name(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
            Self::Headless => "Headless",
        }
    }
}

/// No display server was found, and headless was not requested.
///
/// Carries no data: the two variables consulted are constants, and the message
/// is the whole value of the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoDisplay;

impl std::fmt::Display for NoDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no display server: neither {ENV_WAYLAND_DISPLAY} nor {ENV_X11_DISPLAY} is set.\n\
             \n\
             lindsey is a desktop application and has no terminal interface. This\n\
             usually means one of:\n\
             \n\
             \u{20} · you are on a plain TTY or an SSH session without X forwarding\n\
             \u{20} · you are in a container that does not share the host's session\n\
             \u{20} · a service manager started lindsey without importing the graphical\n\
             \u{20}   session environment (systemd: `systemctl --user import-environment\n\
             \u{20}   {ENV_WAYLAND_DISPLAY} {ENV_X11_DISPLAY}`)\n\
             \n\
             To run without a window anyway — GPUI renders into nothing, which is\n\
             only useful for profiling — set {ENV_HEADLESS}=1."
        )
    }
}

impl std::error::Error for NoDisplay {}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Resolve the session from a variable lookup.
///
/// `var` returns the value of a variable, or `None` when it is unset. An
/// **empty** value counts as unset for the two display variables — that is
/// GPUI's rule (`is_some_and(|d| !d.is_empty())`), and it matters: a shell that
/// exports `DISPLAY=` leaves a variable that is present but useless, and
/// treating it as an X11 session would send us into `X11Client::new().unwrap()`
/// and a panic instead of this diagnostic.
///
/// `ZED_HEADLESS` is checked on *presence* only, again mirroring GPUI.
pub fn select(var: impl Fn(&str) -> Option<String>) -> Result<Session, NoDisplay> {
    if var(ENV_HEADLESS).is_some() {
        return Ok(Session::Headless);
    }

    let non_empty = |name: &str| var(name).filter(|value| !value.is_empty());

    if non_empty(ENV_WAYLAND_DISPLAY).is_some() {
        Ok(Session::Wayland)
    } else if non_empty(ENV_X11_DISPLAY).is_some() {
        Ok(Session::X11)
    } else {
        Err(NoDisplay)
    }
}

/// [`select`] against the real process environment.
pub fn from_env() -> Result<Session, NoDisplay> {
    select(|name| std::env::var(name).ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a lookup over a fixed set of variables.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn wayland_wins_over_x11() {
        // Mirrors guess_compositor: Wayland is tested first, so an XWayland
        // session (both variables set) must resolve to Wayland.
        let got = select(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]));
        assert_eq!(got, Ok(Session::Wayland));
    }

    #[test]
    fn x11_when_no_wayland() {
        assert_eq!(select(env(&[("DISPLAY", ":0")])), Ok(Session::X11));
    }

    #[test]
    fn empty_display_values_do_not_count() {
        // The case that would otherwise reach X11Client::new().unwrap().
        assert_eq!(select(env(&[("DISPLAY", "")])), Err(NoDisplay));
        assert_eq!(select(env(&[("WAYLAND_DISPLAY", "")])), Err(NoDisplay));
        assert_eq!(
            select(env(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0")])),
            Ok(Session::X11),
            "an empty Wayland variable must fall through to X11, not shadow it"
        );
    }

    #[test]
    fn bare_environment_is_an_error_not_headless() {
        // The whole point of the module: silence is not success.
        assert_eq!(select(env(&[])), Err(NoDisplay));
    }

    #[test]
    fn headless_is_opt_in_and_beats_a_live_session() {
        // Presence, not value — GPUI uses var_os(..).is_some().
        assert_eq!(select(env(&[("ZED_HEADLESS", "")])), Ok(Session::Headless));
        assert_eq!(
            select(env(&[("ZED_HEADLESS", "1"), ("WAYLAND_DISPLAY", "wayland-0")])),
            Ok(Session::Headless),
        );
    }

    #[test]
    fn error_message_names_both_variables_and_the_escape_hatch() {
        // The message is the entire value of NoDisplay; an unhelpful one is the
        // bug this module exists to prevent.
        let message = NoDisplay.to_string();
        for needle in [ENV_WAYLAND_DISPLAY, ENV_X11_DISPLAY, ENV_HEADLESS] {
            assert!(
                message.contains(needle),
                "NoDisplay message does not name {needle}: {message}"
            );
        }
    }

    #[test]
    fn compositor_names_match_gpui() {
        // These strings are matched by gpui_linux::current_platform. A typo
        // here would make the startup log describe a backend we did not get.
        assert_eq!(Session::Wayland.compositor_name(), "Wayland");
        assert_eq!(Session::X11.compositor_name(), "X11");
        assert_eq!(Session::Headless.compositor_name(), "Headless");
    }

    #[test]
    fn app_id_matches_desktop_entry() {
        // Identity must agree with what is installed, or the window loses its
        // icon on Wayland. Resolved from the crate root so the test does not
        // depend on the working directory.
        let entry = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("packaging/linux")
            .join(format!("{APP_ID}.desktop"));
        assert!(
            entry.is_file(),
            "no desktop entry at {} — APP_ID and the installed entry have drifted",
            entry.display()
        );

        let contents = std::fs::read_to_string(&entry).expect("read desktop entry");
        assert!(
            contents.contains(&format!("Icon={APP_ID}")),
            "desktop entry does not declare Icon={APP_ID}",
        );
        assert!(
            contents.contains(&format!("StartupWMClass={APP_ID}")),
            "desktop entry does not declare StartupWMClass={APP_ID} — the \
             compositor matches the window to this entry by app id",
        );
    }
}
