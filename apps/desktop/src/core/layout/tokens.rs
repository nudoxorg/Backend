//! Rem-aware shell geometry tokens.

use super::input::{LogicalPx, TextScale};

/// Base geometry and its scale-aware projection.
///
/// The base values are the 100% logical-pixel contract. They are expressed as
/// named rem-like tokens so a larger reading preference scales the complete
/// shell budget coherently instead of scattering pixel literals through views.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LayoutTokens {
    /// Full titlebar height.
    pub titlebar: LogicalPx,
    /// Compact titlebar height used by icon-first navigation.
    pub compact_titlebar: LogicalPx,
    /// Full status row height.
    pub status: LogicalPx,
    /// Compact status row height.
    pub compact_status: LogicalPx,
    /// Wide orbit rail width.
    pub orbit_rail: LogicalPx,
    /// Full project shelf width.
    pub shelf: LogicalPx,
    /// Compact shelf chooser rail width.
    pub shelf_rail: LogicalPx,
    /// Full contextual rail width.
    pub context: LogicalPx,
    /// Compact contextual rail width.
    pub context_rail: LogicalPx,
    /// Minimum readable reader width.
    pub reader_min: LogicalPx,
    /// Preferred reader measure.
    pub reader_measure: LogicalPx,
    /// Inner reader padding.
    pub reader_padding: LogicalPx,
    /// Minimum actionable hit target.
    pub min_hit: LogicalPx,
    /// Gap before a sheet edge.
    pub sheet_gutter: LogicalPx,
}

impl LayoutTokens {
    /// The exact 100% reference geometry.
    pub const BASE: Self = Self {
        titlebar: LogicalPx::new(50),
        compact_titlebar: LogicalPx::new(36),
        status: LogicalPx::new(26),
        compact_status: LogicalPx::new(22),
        orbit_rail: LogicalPx::new(54),
        shelf: LogicalPx::new(264),
        shelf_rail: LogicalPx::new(44),
        context: LogicalPx::new(300),
        context_rail: LogicalPx::new(44),
        reader_min: LogicalPx::new(300),
        reader_measure: LogicalPx::new(960),
        reader_padding: LogicalPx::new(40),
        min_hit: LogicalPx::new(28),
        sheet_gutter: LogicalPx::new(12),
    };

    /// Projects the base rem-like tokens to one text scale.
    #[must_use]
    pub fn at(scale: TextScale) -> Self {
        let scale_one = |value: LogicalPx| {
            let value = value.get();
            let percent = u32::from(scale.get());
            LogicalPx::new(value.saturating_mul(percent).saturating_add(50) / 100)
        };
        Self {
            titlebar: scale_one(Self::BASE.titlebar),
            compact_titlebar: scale_one(Self::BASE.compact_titlebar),
            status: scale_one(Self::BASE.status),
            compact_status: scale_one(Self::BASE.compact_status),
            orbit_rail: scale_one(Self::BASE.orbit_rail).max(Self::BASE.min_hit),
            shelf: scale_one(Self::BASE.shelf),
            shelf_rail: scale_one(Self::BASE.shelf_rail).max(Self::BASE.min_hit),
            context: scale_one(Self::BASE.context),
            context_rail: scale_one(Self::BASE.context_rail).max(Self::BASE.min_hit),
            reader_min: scale_one(Self::BASE.reader_min),
            reader_measure: scale_one(Self::BASE.reader_measure),
            reader_padding: scale_one(Self::BASE.reader_padding),
            min_hit: Self::BASE.min_hit,
            sheet_gutter: scale_one(Self::BASE.sheet_gutter),
        }
    }
}
