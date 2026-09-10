//! Defines the motion presets and durations for `interface-gui`.
//! This module owns every interval and spring constant the interface animates by.
//! Its narrow surface keeps timing out of element builders.

use core::time::Duration;

use crate::motion::spring::SpringParams;

/// The three springs. A fourth would be a fourth opinion about how this interface feels.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Preset {
    /// Panel widths, sheet reveals, the palette.
    Default,
    /// Selection nibs and anything chasing the pointer or the arrow keys.
    Snappy,
    /// Layout that changes size under the reader, where speed reads as a jolt.
    Gentle,
}

impl Preset {
    /// Every preset.
    pub const ALL: [Self; 3] = [Self::Default, Self::Snappy, Self::Gentle];

    /// The constants for this preset.
    #[must_use]
    pub fn params(self) -> SpringParams {
        match self {
            Self::Default => SpringParams::new(459.0, 41.6),
            Self::Snappy => SpringParams::new(1225.0, 67.9),
            Self::Gentle => SpringParams::new(145.0, 23.4),
        }
    }
}

/// The four timed transitions, for the things a spring would over-serve.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Timing {
    /// A hover or selection tint.
    Hover,
    /// A surface appearing or leaving.
    Reveal,
    /// A disclosure opening or closing.
    Disclosure,
    /// How long a pointer must rest before a hover card is fetched.
    HoverCardDelay,
}

impl Timing {
    /// The interval.
    #[must_use]
    pub const fn duration(self) -> Duration {
        Duration::from_millis(match self {
            Self::Hover => 90,
            Self::Reveal => 140,
            Self::Disclosure => 220,
            Self::HoverCardDelay => 350,
        })
    }
}

/// How often the shell asks the epoch file whether another writer changed the library.
///
/// There is no daemon and no file watcher: one cheap read of one small file, off the main thread,
/// slow enough to cost nothing and fast enough that a package added in the terminal shows up in
/// the window before the reader has switched to it.
pub const WATCH_INTERVAL: Duration = Duration::from_millis(750);

/// Whether the reader has asked for motion to be removed.
///
/// Reduced motion does not mean *faster*: it means every spring snaps and no frame is requested,
/// so the interface holds completely still between inputs.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MotionPreference {
    /// Springs run.
    #[default]
    Full,
    /// Springs snap; nothing animates.
    Reduced,
}

impl MotionPreference {
    /// Whether a value under this preference may travel over time.
    #[must_use]
    pub const fn animates(self) -> bool {
        matches!(self, Self::Full)
    }

    /// The word shown beside the motion control.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Full => "Full",
            Self::Reduced => "Reduced",
        }
    }

    /// The other preference, for a two-state toggle.
    #[must_use]
    pub const fn flipped(self) -> Self {
        match self {
            Self::Full => Self::Reduced,
            Self::Reduced => Self::Full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timings_are_ordered_the_way_a_reader_expects() {
        assert!(Timing::Hover.duration() < Timing::Reveal.duration());
        assert!(Timing::Reveal.duration() < Timing::Disclosure.duration());
        assert!(Timing::Disclosure.duration() < Timing::HoverCardDelay.duration());
    }

    #[test]
    fn snappy_is_stiffer_than_default_which_is_stiffer_than_gentle() {
        assert!(Preset::Snappy.params().stiffness() > Preset::Default.params().stiffness());
        assert!(Preset::Default.params().stiffness() > Preset::Gentle.params().stiffness());
    }
}
