//! Typed inputs accepted by the responsive shell resolver.

/// A non-negative logical pixel value.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LogicalPx(u32);

impl LogicalPx {
    /// The zero logical length.
    pub const ZERO: Self = Self(0);

    /// Creates a logical length without applying a scale factor.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Converts a measured GPUI value to a stable logical pixel boundary.
    #[must_use]
    pub fn from_f32(value: f32) -> Self {
        if !value.is_finite() || value <= 0.0 {
            return Self::ZERO;
        }
        Self(value.round().min(u32::MAX as f32) as u32)
    }

    /// Returns the underlying logical pixel count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Adds two logical lengths without wrapping.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtracts two logical lengths without producing a negative bound.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// Returns the smaller logical length.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }

    /// Returns the larger logical length.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }
}

/// Measured content size of one GPUI window.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct WindowContentSize {
    width: LogicalPx,
    height: LogicalPx,
}

impl WindowContentSize {
    /// Creates a content size from logical dimensions.
    #[must_use]
    pub const fn new(width: LogicalPx, height: LogicalPx) -> Self {
        Self { width, height }
    }

    /// Creates a content size from floating point post-layout measurements.
    #[must_use]
    pub fn from_f32(width: f32, height: f32) -> Self {
        Self::new(LogicalPx::from_f32(width), LogicalPx::from_f32(height))
    }

    /// Returns the measured width.
    #[must_use]
    pub const fn width(self) -> LogicalPx {
        self.width
    }

    /// Returns the measured height.
    #[must_use]
    pub const fn height(self) -> LogicalPx {
        self.height
    }
}

/// User interface text scale, constrained to the supported 80–200% range.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextScale(u16);

impl TextScale {
    /// Lowest supported text scale.
    pub const MIN: u16 = 80;
    /// Highest supported text scale.
    pub const MAX: u16 = 200;
    /// Default text scale.
    pub const DEFAULT: Self = Self(100);

    /// Clamps an arbitrary preference to the supported range.
    #[must_use]
    pub const fn percent(value: u16) -> Self {
        Self(value.clamp(Self::MIN, Self::MAX))
    }

    /// Returns the percentage represented by this scale.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Durable pane preferences supplied by the reducer-owned snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PanelPreferences {
    /// Whether the project shelf is requested by the user.
    pub shelf_open: bool,
    /// Whether the contextual rail is requested by the user.
    pub context_open: bool,
}

/// Complete pure-resolver input. Routes and content state intentionally do not
/// appear here; they remain owned by the product snapshot/reducer.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct LayoutInput {
    /// Post-layout measured window content size.
    pub window: WindowContentSize,
    /// User text scale.
    pub text_scale: TextScale,
    /// Durable pane preferences.
    pub panels: PanelPreferences,
}

impl LayoutInput {
    /// Builds an input from measured logical dimensions and preferences.
    #[must_use]
    pub const fn new(
        width: LogicalPx,
        height: LogicalPx,
        text_scale: TextScale,
        panels: PanelPreferences,
    ) -> Self {
        Self {
            window: WindowContentSize::new(width, height),
            text_scale,
            panels,
        }
    }
}
