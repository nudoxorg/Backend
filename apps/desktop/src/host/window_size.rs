//! The window's size, remembered: a person who resized it finds it that size
//! again. Saved once the resize stops (never per frame), read back at launch.

use crate::model::WindowSize;
use crate::navigation::Intent;
use crate::runtime::UiRootEntity;
use gpui::{App, AppContext as _, Context, Entity, Global, Pixels, Size, Subscription, Task, WeakEntity, Window, WindowBounds, px};
use std::time::Duration;

/// How long a resize must rest before it is remembered.
const REST: Duration = Duration::from_millis(400);

/// The largest edge a remembered size may ask for (a display that is gone
/// must not leave a window the size of a wall).
const MOST: f32 = 8_192.0;

/// The size a window opens at: what the person left, kept between `least`
/// and [`MOST`], else `usual`.
#[must_use]
pub(crate) fn opening(saved: Option<WindowSize>, least: Size<Pixels>, usual: Size<Pixels>) -> Size<Pixels> {
    let Some(saved) = saved else { return usual };
    #[allow(clippy::cast_precision_loss)]
    let clamp = |wanted: u32, least: Pixels| px((wanted as f32).clamp(f32::from(least), MOST));
    Size { width: clamp(saved.width, least.width), height: clamp(saved.height, least.height) }
}

/// The size to remember for a window of `size`.
#[must_use]
pub(crate) fn size_of(size: Size<Pixels>) -> WindowSize {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = |edge: Pixels| f32::from(edge).round().clamp(0.0, MOST) as u32;
    WindowSize { width: whole(size.width), height: whole(size.height) }
}

/// Remembers the size of `window` through `root`, once each resize rests.
pub(crate) fn remember(window: &mut Window, root: &Entity<UiRootEntity>, cx: &mut App) {
    let memory = cx.new(|cx| Memory::watching(root.downgrade(), window, cx));
    cx.set_global(Kept(memory));
}

/// The entity that watches one window's bounds for the life of the app.
struct Memory {
    root: WeakEntity<UiRootEntity>,
    /// The resize still resting: a newer one replaces it.
    resting: Option<Task<()>>,
    _watching: Subscription,
}

struct Kept(#[allow(dead_code)] Entity<Memory>);

impl Global for Kept {}

impl Memory {
    fn watching(root: WeakEntity<UiRootEntity>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let watching = cx.observe_window_bounds(window, |memory, window, cx| {
            // Fullscreen and maximised are not a size to come back to.
            let WindowBounds::Windowed(bounds) = window.window_bounds() else { return };
            let size = size_of(bounds.size);
            memory.resting = Some(cx.spawn(async move |memory, cx| {
                cx.background_executor().timer(REST).await;
                let _ = memory.update(cx, |memory, cx| {
                    if let Some(root) = memory.root.upgrade() {
                        root.update(cx, |root, cx| root.dispatch(Intent::WindowResized { width: size.width, height: size.height }, cx));
                    }
                });
            }));
        });
        Self { root, resting: None, _watching: watching }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn a_remembered_size_comes_back_within_what_a_window_can_be() {
        let (least, usual) = (size(px(320.0), px(480.0)), size(px(1380.0), px(880.0)));
        assert_eq!(opening(None, least, usual), usual, "never resized: the window's own size");
        assert_eq!(opening(Some(WindowSize { width: 1100, height: 800 }), least, usual), size(px(1100.0), px(800.0)));
        assert_eq!(opening(Some(WindowSize { width: 100, height: 100 }), least, usual), least, "never smaller than the least");
        assert_eq!(opening(Some(WindowSize { width: 60_000, height: 60_000 }), least, usual), size(px(MOST), px(MOST)));
        assert_eq!(size_of(size(px(1100.4), px(799.6))), WindowSize { width: 1100, height: 800 });
    }
}
