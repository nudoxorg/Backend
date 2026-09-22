//! GPUI boundary and small versioned layout cache.

use super::input::{LayoutInput, LogicalPx, PanelPreferences, TextScale, WindowContentSize};
use super::resolver::{ResponsiveLayout, resolve};
use gpui::Window;

/// Reuses a pure layout result when measured inputs are unchanged.
///
/// The cache stores one `Copy` result and a version counter. It intentionally
/// owns no route, focus, scroll, action, or viewport state.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutCache {
    input: Option<LayoutInput>,
    layout: Option<ResponsiveLayout>,
    version: u64,
    hits: u64,
    misses: u64,
}

impl LayoutCache {
    /// Resolves the input, reusing the prior result when all inputs match.
    #[must_use]
    pub fn resolve(&mut self, input: LayoutInput) -> ResponsiveLayout {
        if self.input == Some(input) {
            self.hits = self.hits.saturating_add(1);
            return self.layout.expect("matching layout cache entry");
        }
        let layout = resolve(input);
        self.input = Some(input);
        self.layout = Some(layout);
        self.version = self.version.saturating_add(1);
        self.misses = self.misses.saturating_add(1);
        layout
    }

    /// Returns the monotonically increasing input version.
    #[must_use]
    pub const fn version(self) -> u64 {
        self.version
    }

    /// Returns cache hit evidence for capture/performance assertions.
    #[must_use]
    pub const fn hits(self) -> u64 {
        self.hits
    }

    /// Returns cache miss evidence for capture/performance assertions.
    #[must_use]
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Builds an input from the actual GPUI content bounds.
    #[must_use]
    pub fn input_from_window(
        window: &Window,
        text_scale: TextScale,
        panels: PanelPreferences,
    ) -> LayoutInput {
        // `viewport_size` is the drawable content rectangle. `bounds()` is in
        // global window coordinates and includes platform chrome, so using it
        // would shift every breakpoint by the titlebar/frame.
        let size = window.viewport_size();
        LayoutInput::new(
            LogicalPx::from_f32(f32::from(size.width)),
            LogicalPx::from_f32(f32::from(size.height)),
            text_scale,
            panels,
        )
    }

    /// Builds an input from a measured content size without involving GPUI.
    #[must_use]
    pub const fn input(
        window: WindowContentSize,
        text_scale: TextScale,
        panels: PanelPreferences,
    ) -> LayoutInput {
        LayoutInput {
            window,
            text_scale,
            panels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LayoutCache;
    use crate::core::layout::{LayoutInput, LogicalPx, PanelPreferences, TextScale};

    fn input(width: u32) -> LayoutInput {
        LayoutInput::new(
            LogicalPx::new(width),
            LogicalPx::new(800),
            TextScale::DEFAULT,
            PanelPreferences {
                shelf_open: true,
                context_open: true,
            },
        )
    }

    #[test]
    fn identical_inputs_reuse_the_copy_layout_without_a_new_version() {
        let mut cache = LayoutCache::default();
        let first = cache.resolve(input(1280));
        let first_version = cache.version();
        let second = cache.resolve(input(1280));
        assert_eq!(first, second);
        assert_eq!(cache.version(), first_version);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);
    }
}
