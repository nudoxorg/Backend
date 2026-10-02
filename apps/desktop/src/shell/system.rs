//! What the operating system says about text size, and which display the
//! window is on.
//!
//! There is no text-size setting: the system sets the baseline where it
//! exposes one, and ⌘+ / ⌘− / ⌘0 move away from it per display. Reading the
//! system's size may start a process (`gsettings`, `reg`), so it runs once on
//! the background executor and lands through the shell's own task.
//!
//! Reduced motion uses the native accessibility preference on macOS and
//! Windows, and the standardized XDG Settings portal on Linux/FreeBSD. When
//! no supported source is available, System mode explicitly falls back to
//! full motion.

use gpui::{App, Window};
use std::sync::Arc;

/// A platform preference whose absence stays explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemPreference<T> {
    /// The platform supplied a value.
    Known(T),
    /// The platform does not expose this preference, or the query failed.
    Unavailable,
}

/// A live subscription to the platform's reduced-motion preference.
///
/// The preference can be unavailable on platforms that do not expose a
/// supported system API. In that case the shell's documented System fallback
/// is full motion. Keep this value for the lifetime of the shell that consumes
/// [`Self::changes`]: dropping it unregisters the macOS or Windows observer, or
/// stops the background portal monitor. Cloned receivers only keep the channel
/// alive; they do not keep the native subscription installed.
pub(crate) struct ReducedMotionWatch {
    initial: SystemPreference<bool>,
    changes: async_channel::Receiver<SystemPreference<bool>>,
    _subscription: MotionSubscription,
}

impl ReducedMotionWatch {
    /// The first value read while installing the subscription.
    #[must_use]
    pub(crate) const fn initial(&self) -> SystemPreference<bool> {
        self.initial
    }

    /// Receives changes without polling. Cloning the receiver keeps this
    /// subscription's stream alive; the watch itself owns the platform token.
    #[must_use]
    pub(crate) fn changes(&self) -> async_channel::Receiver<SystemPreference<bool>> {
        self.changes.clone()
    }
}

/// Installs the platform watcher and returns promptly. Linux portal reads and
/// signal monitoring are performed on a worker thread; dropping the watch
/// stops its monitor and reaps the child away from the render path. Native
/// macOS and Windows APIs do not spawn a process. Unsupported APIs produce
/// `Unavailable`, never a guessed value.
#[must_use]
pub(crate) fn watch_reduced_motion() -> ReducedMotionWatch {
    let (sender, changes) = async_channel::unbounded();

    #[cfg(target_os = "macos")]
    let (initial, subscription) = macos_motion::subscribe(sender);

    #[cfg(target_os = "windows")]
    let (initial, subscription) = windows_motion::subscribe(sender);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let (initial, subscription) = {
        let subscription = portal_motion::subscribe(sender);
        (SystemPreference::Unavailable, subscription)
    };

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux", target_os = "freebsd")))]
    let (initial, subscription) = {
        drop(sender);
        (SystemPreference::Unavailable, unsupported_motion::Subscription)
    };

    ReducedMotionWatch {
        initial,
        changes,
        _subscription: subscription,
    }
}

#[cfg(target_os = "macos")]
type MotionSubscription = macos_motion::Subscription;
#[cfg(target_os = "windows")]
type MotionSubscription = windows_motion::Subscription;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
type MotionSubscription = portal_motion::Subscription;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux", target_os = "freebsd")))]
type MotionSubscription = unsupported_motion::Subscription;

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

#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "AppKit's observer registration is the documented NSWorkspace notification boundary")]
mod macos_motion {
    //! NSWorkspace's accessibility preference and its display-options change notification.
    use super::SystemPreference;
    use async_channel::Sender;
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification};
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue};
    use std::ptr::NonNull;

    pub(super) struct Subscription {
        center: Retained<NSNotificationCenter>,
        observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            // SAFETY: this observer token came from this notification center.
            unsafe { self.center.removeObserver(self.observer.as_ref()) };
        }
    }

    pub(super) fn subscribe(sender: Sender<SystemPreference<bool>>) -> (SystemPreference<bool>, Subscription) {
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        let queue = NSOperationQueue::mainQueue();
        let changed = sender.clone();
        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
            let reduced = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
            let _ = changed.try_send(SystemPreference::Known(reduced));
        });
        // SAFETY: the block only captures an async-channel sender, which is
        // Send + Sync; AppKit invokes it on its main operation queue.
        let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification), None, Some(&queue), &block) };
        let initial = SystemPreference::Known(workspace.accessibilityDisplayShouldReduceMotion());
        (initial, Subscription { center, observer })
    }
}

#[cfg(target_os = "windows")]
mod windows_motion {
    //! The user's accessibility animation setting and its WinRT change event.
    use super::SystemPreference;
    use async_channel::Sender;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::ViewManagement::{UISettings, UISettingsAnimationsEnabledChangedEventArgs};

    pub(super) struct Subscription {
        settings: Option<UISettings>,
        token: Option<i64>,
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            if let (Some(settings), Some(token)) = (&self.settings, self.token) {
                let _ = settings.RemoveAnimationsEnabledChanged(token);
            }
        }
    }

    pub(super) fn subscribe(sender: Sender<SystemPreference<bool>>) -> (SystemPreference<bool>, Subscription) {
        let Ok(settings) = UISettings::new() else {
            return (SystemPreference::Unavailable, Subscription { settings: None, token: None });
        };
        let changed = sender.clone();
        let handler = TypedEventHandler::<UISettings, UISettingsAnimationsEnabledChangedEventArgs>::new(move |settings, _| {
            let value = settings.as_ref().map_or(SystemPreference::Unavailable, |settings| {
                settings.AnimationsEnabled().map_or(SystemPreference::Unavailable, |enabled| SystemPreference::Known(!enabled))
            });
            let _ = changed.try_send(value);
            Ok(())
        });
        let Ok(token) = settings.AnimationsEnabledChanged(&handler) else {
            return (
                SystemPreference::Unavailable,
                Subscription {
                    settings: Some(settings),
                    token: None,
                },
            );
        };
        let initial = settings.AnimationsEnabled().map_or(SystemPreference::Unavailable, |enabled| SystemPreference::Known(!enabled));
        (
            initial,
            Subscription {
                settings: Some(settings),
                token: Some(token),
            },
        )
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod portal_motion {
    //! The standardized XDG portal preference and its `SettingChanged` signal.
    use super::SystemPreference;
    use async_channel::Sender;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
    const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
    const REDUCED_MOTION_KEY: &str = "reduced-motion";

    pub(super) struct Subscription {
        child: Arc<Mutex<Option<Child>>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let mut guard = self.child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(mut child) = guard.take() {
                let _ = child.kill();
                // Reap away from shell teardown and the render executor too;
                // killing the monitor closes stdout and wakes its reader.
                thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }

    pub(super) fn subscribe(sender: Sender<SystemPreference<bool>>) -> Subscription {
        let child = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_child = Arc::clone(&child);
        let worker_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let Ok(mut monitor) = Command::new("gdbus")
                .args(["monitor", "--session", "--dest", DESTINATION, "--object-path", OBJECT_PATH])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                let _ = sender.try_send(SystemPreference::Unavailable);
                return;
            };
            let Some(stdout) = monitor.stdout.take() else {
                let _ = monitor.kill();
                let _ = monitor.wait();
                let _ = sender.try_send(SystemPreference::Unavailable);
                return;
            };
            if worker_stop.load(Ordering::Acquire) {
                let _ = monitor.kill();
                let _ = monitor.wait();
                return;
            }
            let mut guard = worker_child.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if worker_stop.load(Ordering::Acquire) {
                drop(guard);
                let _ = monitor.kill();
                let _ = monitor.wait();
                return;
            }
            *guard = Some(monitor);
            drop(guard);

            // Start monitoring before the read, so any concurrent change is
            // buffered on stdout instead of being lost between query and watch.
            let initial = read_portal_motion();
            let _ = sender.try_send(initial);
            let mut lines = BufReader::new(stdout).lines();
            while !worker_stop.load(Ordering::Acquire) {
                let line = match lines.next() {
                    Some(Ok(line)) => line,
                    Some(Err(_)) | None => {
                        let _ = sender.try_send(SystemPreference::Unavailable);
                        break;
                    }
                };
                if let Some(value) = portal_change(&line) {
                    let _ = sender.try_send(value);
                }
            }
        });
        Subscription { child, stop }
    }

    fn read_portal_motion() -> SystemPreference<bool> {
        let Ok(output) = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                DESTINATION,
                "--object-path",
                OBJECT_PATH,
                "--method",
                "org.freedesktop.portal.Settings.ReadOne",
                "--timeout",
                "3",
                APPEARANCE_NAMESPACE,
                REDUCED_MOTION_KEY,
            ])
            .output()
        else {
            return SystemPreference::Unavailable;
        };
        if !output.status.success() {
            return SystemPreference::Unavailable;
        }
        parse_value(&String::from_utf8_lossy(&output.stdout))
    }

    fn portal_change(line: &str) -> Option<SystemPreference<bool>> {
        (line.contains("org.freedesktop.portal.Settings.SettingChanged") && line.contains("'org.freedesktop.appearance'") && line.contains("'reduced-motion'")).then(|| parse_value(line))
    }

    fn parse_value(value: &str) -> SystemPreference<bool> {
        let Some((_, value)) = value.rsplit_once("<uint32 ") else {
            return SystemPreference::Unavailable;
        };
        let Some(value) = value.split('>').next() else {
            return SystemPreference::Unavailable;
        };
        match value.trim() {
            "1" => SystemPreference::Known(true),
            // Portal 0 means “no preference”, so full motion is the shell's
            // explicit fallback rather than a claim that the OS chose it.
            "0" => SystemPreference::Unavailable,
            _ => SystemPreference::Unavailable,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{parse_value, portal_change};
        use crate::shell::system::SystemPreference;

        #[test]
        fn standardized_portal_values_keep_no_preference_unavailable() {
            assert_eq!(parse_value("(<uint32 1>,)"), SystemPreference::Known(true));
            assert_eq!(parse_value("(<uint32 0>,)"), SystemPreference::Unavailable);
            assert_eq!(parse_value("(<uint32 2>,)"), SystemPreference::Unavailable);
            assert_eq!(parse_value("unsupported"), SystemPreference::Unavailable);
        }

        #[test]
        fn portal_monitor_ignores_unrelated_settings() {
            assert_eq!(
                portal_change("Settings.SettingChanged ('org.freedesktop.appearance', 'reduced-motion', <uint32 1>)"),
                Some(SystemPreference::Known(true))
            );
            assert_eq!(portal_change("Settings.SettingChanged ('org.gnome.desktop.interface', 'enable-animations', <uint32 1>)"), None);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux", target_os = "freebsd")))]
mod unsupported_motion {
    pub(super) struct Subscription;
}
