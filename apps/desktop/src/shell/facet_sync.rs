//! Settings → the active [`Facet`]: appearance (following the system when
//! asked), text scale, density, contrast and motion. Held reveal modes are
//! transient and never persisted; they are carried over untouched.

use crate::model::{AppearancePreference, ContrastPreference, DensityPreference, MotionPreference, SettingsState};
use facet::{Appearance, Contrast, Density, Facet, Reveal};

/// What the window's surroundings contribute: the system's appearance and
/// text size, and the display the window is on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Surroundings {
    /// The system is in dark mode.
    pub dark: bool,
    /// The system's text scale (1.0 = its default).
    pub text: f32,
    /// The display's zoom key.
    pub display: std::sync::Arc<str>,
}

/// The facet a snapshot's settings ask for in these surroundings, keeping
/// the currently held reveal.
#[must_use]
pub(crate) fn facet_for(settings: &SettingsState, around: &Surroundings, reveal: Reveal) -> Facet {
    let system_dark = around.dark;
    Facet {
        appearance: match settings.appearance {
            AppearancePreference::Abyss => Appearance::Abyss,
            AppearancePreference::Glacier => Appearance::Glacier,
            AppearancePreference::System if system_dark => Appearance::Abyss,
            AppearancePreference::System => Appearance::Glacier,
        },
        text_scale: around.text * settings.zoom.factor(&around.display),
        reduced_motion: settings.motion == MotionPreference::Reduced,
        contrast: match settings.contrast {
            ContrastPreference::Normal => Contrast::Normal,
            ContrastPreference::High => Contrast::High,
        },
        density: match settings.density {
            DensityPreference::Comfortable => Density::Comfortable,
            DensityPreference::Compact => Density::Compact,
            DensityPreference::Dense => Density::Dense,
        },
        reveal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ZoomPreference, ZoomStep};

    fn around(dark: bool, text: f32, display: &str) -> Surroundings {
        Surroundings {
            dark,
            text,
            display: std::sync::Arc::from(display),
        }
    }

    #[test]
    fn every_setting_reaches_the_facet() {
        let settings = SettingsState {
            appearance: AppearancePreference::System,
            zoom: ZoomPreference::default().to_percent("wall", 125),
            density: DensityPreference::Dense,
            contrast: ContrastPreference::High,
            motion: MotionPreference::Reduced,
            ..SettingsState::default()
        };
        let dark = facet_for(&settings, &around(true, 1.0, "wall"), Reveal::default());
        assert_eq!(dark.appearance, Appearance::Abyss);
        assert!((dark.text_scale - 1.25).abs() < f32::EPSILON);
        assert_eq!(dark.density, Density::Dense);
        assert_eq!(dark.contrast, Contrast::High);
        assert!(dark.reduced_motion);
        let light = facet_for(&settings, &around(false, 1.0, "wall"), Reveal::default());
        assert_eq!(light.appearance, Appearance::Glacier, "system follows the OS");
        let held = Reveal { keys: true, xray: false };
        assert_eq!(facet_for(&settings, &around(true, 1.0, "wall"), held).reveal, held);
    }

    #[test]
    fn zoom_is_per_display_on_top_of_the_system_size() {
        let zoom = ZoomPreference::default().step("laptop", ZoomStep::In).step("laptop", ZoomStep::In);
        let settings = SettingsState {
            zoom,
            ..SettingsState::default()
        };
        // Two steps up on the laptop: 125 %; the wall screen is untouched.
        let laptop = facet_for(&settings, &around(true, 1.0, "laptop"), Reveal::default());
        assert!((laptop.text_scale - 1.25).abs() < 1e-6, "{}", laptop.text_scale);
        let wall = facet_for(&settings, &around(true, 1.0, "wall"), Reveal::default());
        assert!((wall.text_scale - 1.0).abs() < 1e-6);
        // The system's size is the baseline the zoom multiplies.
        let big_system = facet_for(&settings, &around(true, 1.5, "wall"), Reveal::default());
        assert!((big_system.text_scale - 1.5).abs() < 1e-6);
        // ⌘0 returns the laptop to the system size; steps clamp at the ends.
        let reset = settings.zoom.step("laptop", ZoomStep::Reset);
        assert_eq!(reset.percent("laptop"), 100);
        let mut far = ZoomPreference::default();
        for _ in 0..20 {
            far = far.step("x", ZoomStep::Out);
        }
        assert_eq!(far.percent("x"), 85);
    }
}
