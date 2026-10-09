//! The shelf drawer's sampled occupancy, independent of offscreen size changes.

use facet::motion::{Motion, Spec, spec};
use gpui::{App, Pixels, Window, px};

#[derive(Clone, Copy)]
pub(super) struct DrawerScene {
    pub offset: Pixels,
    pub visible: bool,
    pub moving: bool,
}

#[derive(Default)]
pub(super) struct DrawerPresentation {
    motion: Motion,
    last: Option<DrawerScene>,
    #[cfg(test)]
    control: Option<(Motion, Option<f32>)>,
}

impl DrawerPresentation {
    #[cfg(test)]
    pub(super) fn diagnostic_scene(&self, cx: &App) -> (Option<DrawerScene>, bool) {
        (self.last, self.motion.is_live(cx))
    }

    #[cfg(test)]
    pub(super) fn start_spring_control(&mut self) { self.control = Some((Motion::new(), None)); }

    #[cfg(test)]
    pub(super) fn spring_control_value(&self) -> Option<f32> {
        self.control.as_ref().and_then(|(_, value)| *value)
    }

    pub fn sample(&mut self, active: bool, width: Pixels, window: &mut Window, cx: &mut App) -> DrawerScene {
        // A wholly absent drawer has no exit pixels or gesture to preserve.
        // Reposition its hidden edge directly when resize/text size changes;
        // animating from the old hidden edge could reveal an unopened drawer.
        // Once mounted, retain the ordinary velocity-preserving spring through
        // close/re-entry until the same sampled occupancy test releases it.
        let present = active || self.last.is_some_and(|scene| scene.visible);
        let target = if active { px(0.0) } else { -width };
        let motion = if present { spec::SETTLE } else { Spec::Snap };
        let offset = px(self.motion.animate("drawer-x", f32::from(target), motion, window, cx));
        let visible = active || offset > -width + px(0.5);
        let scene = DrawerScene { offset, visible, moving: visible && self.motion.is_live(cx) };
        #[cfg(test)]
        if let Some((control, value)) = &mut self.control {
            // Motion's frame gate requires the actual rendering entity.
            // This independent unchanged spring samples beside the real one,
            // never from the test's out-of-render observation closure. Its
            // oracle ends once the drawer is absent, so it schedules no hidden
            // reference tail during the following native liveness assertions.
            let spec = if scene.visible { spec::SETTLE } else { Spec::Snap };
            *value = Some(control.animate("drawer-control", f32::from(target), spec, window, cx));
        }
        self.last = Some(scene);
        scene
    }
}
