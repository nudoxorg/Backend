//! What the operating system says about text size, and which display the
//! window is on.
//!
//! There is no text-size setting: the system sets the baseline where it
//! exposes one, and ⌘+ / ⌘− / ⌘0 move away from it per display. Reading the
//! system's size may start a process (`gsettings`, `reg`), so it runs once on
//! the background executor and lands through the shell's own task.

use gpui::{App, Window};
use std::sync::Arc;

/// The key a display's zoom is remembered under: its stable UUID where the
/// platform has one, its session id otherwise, `default` with no display.
pub(crate) fn display_key(window: &Window, cx: &App) -> Arc<str> {
    match window.display(cx) {
        Some(display) => match display.uuid() {
            Ok(uuid) => Arc::from(uuid.to_string()),
            Err(_) => Arc::from(format!("display-{:?}", display.id())),
        },
        None => Arc::from("default"),
    }
}

/// The system's text scale (1.0 = its default), where the OS exposes one.
///
/// - Linux (GNOME and derivatives): `org.gnome.desktop.interface
///   text-scaling-factor`.
/// - Windows: *Make text bigger*, `HKCU\Software\Microsoft\Accessibility
///   TextScaleFactor` (percent).
/// - macOS exposes no system-wide text size to AppKit apps: 1.0.
///
/// Blocking: call from the background executor.
#[must_use]
pub(crate) fn text_scale() -> f32 {
    let scale = platform_text_scale().unwrap_or(1.0);
    if scale.is_finite() { scale.clamp(0.5, 3.0) } else { 1.0 }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn platform_text_scale() -> Option<f32> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "text-scaling-factor"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(target_os = "windows")]
fn platform_text_scale() -> Option<f32> {
    let output = std::process::Command::new("reg")
        .args(["query", r"HKCU\Software\Microsoft\Accessibility", "/v", "TextScaleFactor"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text.split_whitespace().last()?;
    let percent = u32::from_str_radix(value.trim_start_matches("0x"), 16).ok()?;
    Some(percent as f32 / 100.0)
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
fn platform_text_scale() -> Option<f32> {
    None
}
