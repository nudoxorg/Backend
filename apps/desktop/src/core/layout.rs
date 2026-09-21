//! Responsive shell transformations shared by every route.

/// Width classes are closed layout data; pages do not keep independent
/// responsive state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WidthClass {
    /// Compact rail layout.
    Compact,
    /// Standard two-column layout.
    Standard,
    /// Wide reading layout.
    Wide,
    /// Full three-column layout.
    Full,
}

impl WidthClass {
    /// Resolves the class from the physical viewport width.
    #[must_use]
    pub const fn from_width(width: f32) -> Self {
        if width < 800.0 {
            Self::Compact
        } else if width < 1_100.0 {
            Self::Standard
        } else if width < 1_440.0 {
            Self::Wide
        } else {
            Self::Full
        }
    }
}

/// A panel's responsive representation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PanelMode {
    /// Panel is not shown in the current width class.
    Hidden,
    /// Panel is reduced to an icon/rail.
    Rail,
    /// Panel has its full reading width.
    Full,
}

/// Shared shell transformation for all routes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponsiveLayout {
    /// Resolved width class.
    pub width: WidthClass,
    /// Sidebar representation.
    pub shelf: PanelMode,
    /// Context representation.
    pub context: PanelMode,
    /// Content reading padding.
    pub content_padding: f32,
}

impl ResponsiveLayout {
    /// Computes one layout from viewport dimensions and persistent panel prefs.
    #[must_use]
    pub const fn resolve(width: f32, shelf_open: bool, context_open: bool) -> Self {
        let width_class = WidthClass::from_width(width);
        match width_class {
            WidthClass::Compact => Self {
                width: width_class,
                shelf: PanelMode::Rail,
                context: PanelMode::Hidden,
                content_padding: 12.0,
            },
            WidthClass::Standard => Self {
                width: width_class,
                shelf: if shelf_open {
                    PanelMode::Full
                } else {
                    PanelMode::Rail
                },
                context: PanelMode::Hidden,
                content_padding: 20.0,
            },
            WidthClass::Wide => Self {
                width: width_class,
                shelf: if shelf_open {
                    PanelMode::Full
                } else {
                    PanelMode::Rail
                },
                context: if context_open {
                    PanelMode::Rail
                } else {
                    PanelMode::Hidden
                },
                content_padding: 28.0,
            },
            WidthClass::Full => Self {
                width: width_class,
                shelf: if shelf_open {
                    PanelMode::Full
                } else {
                    PanelMode::Rail
                },
                context: if context_open {
                    PanelMode::Full
                } else {
                    PanelMode::Hidden
                },
                content_padding: 36.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_transforms_are_shared_and_page_agnostic() {
        assert_eq!(
            ResponsiveLayout::resolve(640.0, true, true).context,
            PanelMode::Hidden
        );
        assert_eq!(
            ResponsiveLayout::resolve(1_200.0, true, true).context,
            PanelMode::Rail
        );
        assert_eq!(
            ResponsiveLayout::resolve(1_600.0, true, true).context,
            PanelMode::Full
        );
    }
}
