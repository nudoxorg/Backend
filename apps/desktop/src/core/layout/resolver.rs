//! Pure responsive shell resolver.

use super::input::{LayoutInput, LogicalPx, TextScale, WindowContentSize};
use super::regions::{
    HorizontalOverflow, PanelMode, RegionBounds, RegionId, RegionPresentation, RegionSlot,
    SafeContentBounds, SheetKind, ShellRegions,
};
use super::tokens::LayoutTokens;

/// Descriptive width class derived from the same content budget as regions.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WidthClass {
    /// Icon/status floor.
    IconStatus,
    /// Icon-first navigation with a single reader.
    Compact,
    /// One reader with optional pane sheets.
    SingleColumn,
    /// Shelf remains in flow while context becomes a sheet.
    ContextSheet,
    /// Shelf and reader remain in flow.
    ShelfFlow,
    /// Full wide shell.
    Wide,
}

/// Progressive collapse stage. Ordering is stable and useful to transition
/// planning; it does not own animation or product state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CollapseStage {
    /// Full panes and wide titlebar.
    Wide,
    /// Context moved behind a sheet trigger.
    ContextSheet,
    /// Shelf moved behind a chooser/sheet trigger.
    ShelfSheet,
    /// Titlebar controls become overflow and navigation becomes icon-first.
    CompactNavigation,
    /// Only icon/status affordances remain in the first frame.
    IconStatus,
}

/// Capacity class for persistent titlebar actions.
///
/// This is intentionally independent from pane collapse: a window can keep a
/// single-column reader while its titlebar has already switched to icon
/// labels. The resolver therefore remains the sole authority for both layout
/// and which controls may enter the focus order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TitlebarDensity {
    /// Full product name, route identity, and labelled commands.
    Expanded,
    /// Icon-labelled commands with history preserved.
    Compact,
    /// Only the brand and global search fit; other commands remain available
    /// through the global palette and keyboard bindings.
    Essential,
}

impl TitlebarDensity {
    /// Returns whether back and forward controls fit in the titlebar.
    #[must_use]
    pub const fn shows_history(self) -> bool {
        !matches!(self, Self::Essential)
    }

    /// Returns whether the dedicated settings control fits in the titlebar.
    #[must_use]
    pub const fn shows_settings(self) -> bool {
        !matches!(self, Self::Essential)
    }

    /// Returns whether labels may use their expanded text form.
    #[must_use]
    pub const fn is_expanded(self) -> bool {
        matches!(self, Self::Expanded)
    }
}

impl CollapseStage {
    /// Returns whether this stage is compact enough for the 90ms switch beat.
    #[must_use]
    pub const fn is_compact(self) -> bool {
        matches!(self, Self::CompactNavigation | Self::IconStatus)
    }
}

/// Complete output of one pure resolution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResponsiveLayout {
    /// Measured window content size.
    pub window: WindowContentSize,
    /// Applied text scale.
    pub text_scale: TextScale,
    /// Diagnostic width class.
    pub width: WidthClass,
    /// Progressive collapse stage.
    pub collapse: CollapseStage,
    /// Capacity-derived titlebar presentation and action admission policy.
    pub titlebar_density: TitlebarDensity,
    /// Canonical semantic regions.
    pub regions: ShellRegions,
    /// Primary navigation mode.
    pub orbit: PanelMode,
    /// Shelf mode.
    pub shelf: PanelMode,
    /// Context mode.
    pub context: PanelMode,
    /// Width supplied to the component-owned shelf sheet when the shelf is
    /// out of flow.
    pub shelf_sheet_width: LogicalPx,
    /// Width supplied to the component-owned context/details sheet when the
    /// contextual rail is out of flow.
    pub context_sheet_width: LogicalPx,
    /// Inner reader padding from the same rem-aware token set.
    pub content_padding: LogicalPx,
}

impl ResponsiveLayout {
    /// Returns one region by semantic identity.
    #[must_use]
    pub const fn region(self, id: RegionId) -> RegionSlot {
        self.regions.get(id)
    }

    /// Returns the safe reader bounds.
    #[must_use]
    pub const fn safe_content(self) -> SafeContentBounds {
        self.regions.safe_content
    }

    /// Returns the flow width of the shelf.
    #[must_use]
    pub const fn shelf_width(self) -> LogicalPx {
        self.regions.project_shelf.bounds.width
    }

    /// Returns the flow width of the contextual rail.
    #[must_use]
    pub const fn context_width(self) -> LogicalPx {
        self.regions.context_rail.bounds.width
    }

    /// Returns the width occupied by every flow column.
    #[must_use]
    pub const fn flow_width(self) -> LogicalPx {
        self.regions.reader.bounds.right()
    }

    /// Returns the resolver-owned width for a component-managed sheet.
    #[must_use]
    pub const fn sheet_width(self, kind: SheetKind) -> LogicalPx {
        match kind {
            SheetKind::Shelf => self.shelf_sheet_width,
            SheetKind::Context | SheetKind::Details => self.context_sheet_width,
        }
    }
}

/// Resolves measured content geometry into one allocation-free shell value.
#[must_use]
pub fn resolve(input: LayoutInput) -> ResponsiveLayout {
    let tokens = LayoutTokens::at(input.text_scale);
    let width = input.window.width().get();
    let height = input.window.height().get();

    // The expanded budget includes route identity and labelled search and
    // settings actions. The compact budget retains four 44px focus targets,
    // a brand mark, tight gaps, and edge padding. Both scale with interface
    // text so larger type collapses before controls overlap.
    let titlebar_density = if width >= scaled_threshold(620, input.text_scale) {
        TitlebarDensity::Expanded
    } else if width >= scaled_threshold(264, input.text_scale) {
        TitlebarDensity::Compact
    } else {
        TitlebarDensity::Essential
    };

    // Titlebar/rail compaction is itself content-driven. The thresholds are
    // budgets, not device names, and are intentionally stable at +/-1 px.
    let icon_navigation = width <= scaled_threshold(240, input.text_scale);
    let icon_status = width <= scaled_threshold(120, input.text_scale);
    let titlebar_height = if icon_navigation {
        tokens.compact_titlebar.get()
    } else {
        tokens.titlebar.get()
    };
    let status_height = if icon_status {
        tokens.compact_status.get()
    } else {
        tokens.status.get()
    };
    let titlebar_height = titlebar_height.min(height);
    let status_height = status_height.min(height.saturating_sub(titlebar_height));
    let body_height = height
        .saturating_sub(titlebar_height)
        .saturating_sub(status_height);
    // Keep the rail's logical boundary stable while its controls change from
    // labelled to icon-first. This preserves reader anchors and makes the
    // readable rectangle monotonic within every width transition.
    let orbit_width = tokens.orbit_rail.min(LogicalPx::new(width)).get();

    let reader_min = tokens.reader_min.get();
    let shelf_width = tokens.shelf.get();
    let context_width = tokens.context.get();
    let shelf_requested = input.panels.shelf_open;
    let context_requested = input.panels.context_open;

    // Context is the first pane to leave flow. When it fits, it receives its
    // measured 300px wide reference rectangle at 100%; when it does not, it
    // remains reachable as a sheet presentation.
    let context_fits = context_requested
        && width
            >= orbit_width
                .saturating_add(if shelf_requested { shelf_width } else { 0 })
                .saturating_add(reader_min)
                .saturating_add(context_width);
    let shelf_fits = shelf_requested
        && width
            >= orbit_width
                .saturating_add(reader_min)
                .saturating_add(shelf_width);

    let context = if context_fits {
        PanelMode::Full
    } else if context_requested {
        PanelMode::Sheet
    } else {
        PanelMode::Hidden
    };
    let shelf = if shelf_fits {
        PanelMode::Full
    } else if shelf_requested {
        PanelMode::Sheet
    } else {
        PanelMode::Hidden
    };

    // Stable tie-breaking: if both panes need an overlay, context is the
    // primary sheet target because it was collapsed first. The shelf remains
    // reachable through the same component-owned chooser trigger.
    let reader_x = orbit_width.saturating_add(if shelf.in_flow() { shelf_width } else { 0 });
    let context_flow_width = if context.in_flow() { context_width } else { 0 };
    let reader_width = width
        .saturating_sub(reader_x)
        .saturating_sub(context_flow_width);
    let reader_bounds = RegionBounds::new(reader_x, titlebar_height, reader_width, body_height);
    let status_y = titlebar_height.saturating_add(body_height);
    let document_measure = reader_width.min(tokens.reader_measure.get());
    let safe_content = SafeContentBounds {
        bounds: reader_bounds,
        document_measure: LogicalPx::new(document_measure),
        document_overflow: HorizontalOverflow::Clip,
        source_overflow: HorizontalOverflow::Scroll,
    };
    let titlebar = RegionSlot::new(
        RegionId::Titlebar,
        RegionBounds::new(0, 0, width, titlebar_height),
        RegionPresentation::Full,
    );
    let orbit = RegionSlot::new(
        RegionId::OrbitRail,
        RegionBounds::new(0, titlebar_height, orbit_width, body_height),
        RegionPresentation::Rail,
    );
    let shelf_bounds = if shelf.in_flow() {
        RegionBounds::new(orbit_width, titlebar_height, shelf_width, body_height)
    } else {
        RegionBounds::ZERO
    };
    let shelf_region = RegionSlot::new(RegionId::ProjectShelf, shelf_bounds, shelf.presentation());
    let context_x = reader_bounds.right().get();
    let context_bounds = if context.in_flow() {
        RegionBounds::new(context_x, titlebar_height, context_width, body_height)
    } else {
        RegionBounds::ZERO
    };
    let context_region = RegionSlot::new(
        RegionId::ContextRail,
        context_bounds,
        context.presentation(),
    );
    let reader_region = RegionSlot::new(RegionId::Reader, reader_bounds, RegionPresentation::Full);
    let status_region = RegionSlot::new(
        RegionId::Status,
        RegionBounds::new(0, status_y, width, status_height),
        RegionPresentation::Full,
    );
    let collapse = if icon_status {
        CollapseStage::IconStatus
    } else if icon_navigation {
        CollapseStage::CompactNavigation
    } else if matches!(shelf, PanelMode::Sheet) {
        CollapseStage::ShelfSheet
    } else if matches!(context, PanelMode::Sheet) {
        CollapseStage::ContextSheet
    } else {
        CollapseStage::Wide
    };
    let width_class = match collapse {
        CollapseStage::IconStatus => WidthClass::IconStatus,
        CollapseStage::CompactNavigation => WidthClass::Compact,
        CollapseStage::ShelfSheet => WidthClass::SingleColumn,
        CollapseStage::ContextSheet if shelf.in_flow() => WidthClass::ContextSheet,
        CollapseStage::ContextSheet => WidthClass::SingleColumn,
        CollapseStage::Wide if shelf.in_flow() && context.in_flow() => WidthClass::Wide,
        CollapseStage::Wide => WidthClass::ShelfFlow,
    };

    ResponsiveLayout {
        window: input.window,
        text_scale: input.text_scale,
        width: width_class,
        collapse,
        titlebar_density,
        regions: ShellRegions {
            titlebar,
            orbit_rail: orbit,
            project_shelf: shelf_region,
            reader: reader_region,
            context_rail: context_region,
            status: status_region,
            safe_content,
        },
        orbit: PanelMode::Rail,
        shelf,
        context,
        shelf_sheet_width: LogicalPx::new(shelf_width.min(width)),
        context_sheet_width: LogicalPx::new(context_width.min(width)),
        content_padding: tokens.reader_padding,
    }
}

fn scaled_threshold(base: u32, scale: TextScale) -> u32 {
    base.saturating_mul(u32::from(scale.get()))
        .saturating_add(50)
        / 100
}

#[cfg(test)]
mod tests {
    use super::{CollapseStage, PanelMode, TitlebarDensity, WidthClass, resolve};
    use crate::core::layout::{
        LayoutInput, LogicalPx, PanelPreferences, RegionBounds, RegionId, RegionPresentation,
        SheetKind, TextScale,
    };

    fn input(width: u32, height: u32, scale: u16) -> LayoutInput {
        LayoutInput::new(
            LogicalPx::new(width),
            LogicalPx::new(height),
            TextScale::percent(scale),
            PanelPreferences {
                shelf_open: true,
                context_open: true,
            },
        )
    }

    #[test]
    fn wide_geometry_matches_the_measured_reference() {
        let layout = resolve(input(1280, 800, 100));
        assert_eq!(
            layout.region(RegionId::Titlebar).bounds,
            RegionBounds::new(0, 0, 1280, 50)
        );
        assert_eq!(
            layout.region(RegionId::OrbitRail).bounds,
            RegionBounds::new(0, 50, 54, 724)
        );
        assert_eq!(
            layout.region(RegionId::ProjectShelf).bounds,
            RegionBounds::new(54, 50, 264, 724)
        );
        assert_eq!(
            layout.region(RegionId::Reader).bounds,
            RegionBounds::new(318, 50, 662, 724)
        );
        assert_eq!(
            layout.region(RegionId::ContextRail).bounds,
            RegionBounds::new(980, 50, 300, 724)
        );
        assert_eq!(
            layout.region(RegionId::Status).bounds,
            RegionBounds::new(0, 774, 1280, 26)
        );
        assert_eq!(layout.collapse, CollapseStage::Wide);
    }

    #[test]
    fn collapse_boundaries_are_deterministic_at_one_pixel() {
        let at_shelf = resolve(input(618, 800, 100));
        let below_shelf = resolve(input(617, 800, 100));
        assert_eq!(at_shelf.shelf, PanelMode::Full);
        assert_eq!(below_shelf.shelf, PanelMode::Sheet);
        assert_eq!(below_shelf.context, PanelMode::Sheet);
        assert_eq!(
            resolve(input(241, 800, 100)).collapse,
            CollapseStage::ShelfSheet
        );
        assert_eq!(
            resolve(input(240, 800, 100)).collapse,
            CollapseStage::CompactNavigation
        );
        assert_eq!(
            resolve(input(121, 800, 100)).collapse,
            CollapseStage::CompactNavigation
        );
        assert_eq!(
            resolve(input(120, 800, 100)).collapse,
            CollapseStage::IconStatus
        );
    }

    #[test]
    fn titlebar_capacity_has_exact_scale_aware_boundaries() {
        assert_eq!(
            resolve(input(620, 800, 100)).titlebar_density,
            TitlebarDensity::Expanded
        );
        assert_eq!(
            resolve(input(619, 800, 100)).titlebar_density,
            TitlebarDensity::Compact
        );
        assert_eq!(
            resolve(input(264, 800, 100)).titlebar_density,
            TitlebarDensity::Compact
        );
        assert_eq!(
            resolve(input(263, 800, 100)).titlebar_density,
            TitlebarDensity::Essential
        );
        assert_eq!(
            resolve(input(1_239, 800, 200)).titlebar_density,
            TitlebarDensity::Compact
        );
        assert_eq!(
            resolve(input(527, 800, 200)).titlebar_density,
            TitlebarDensity::Essential
        );
    }

    #[test]
    fn scale_range_changes_budget_without_negative_regions() {
        for scale in [80, 100, 125, 150, 175, 200] {
            let layout = resolve(input(1280, 800, scale));
            for id in [
                RegionId::Titlebar,
                RegionId::OrbitRail,
                RegionId::ProjectShelf,
                RegionId::Reader,
                RegionId::ContextRail,
                RegionId::Status,
            ] {
                let bounds = layout.region(id).bounds;
                assert!(bounds.width.get() <= 1280);
                assert!(bounds.height.get() <= 800);
            }
            assert!(layout.safe_content().bounds.width.get() <= 1280);
        }
    }

    #[test]
    fn readable_width_is_monotonic_as_window_grows() {
        let mut previous = 0;
        let mut stage = None;
        for width in 90..=1280 {
            let layout = resolve(input(width, 420, 100));
            let current = layout.safe_content().bounds.width.get();
            if stage != Some(layout.collapse) {
                previous = current;
                stage = Some(layout.collapse);
                continue;
            }
            assert!(
                current >= previous,
                "reader shrank inside a stage at width {width}"
            );
            previous = current;
        }
    }

    #[test]
    fn short_and_tall_windows_share_horizontal_identity() {
        let short = resolve(input(1280, 140, 100));
        let tall = resolve(input(1280, 1400, 100));
        for id in [
            RegionId::Titlebar,
            RegionId::OrbitRail,
            RegionId::ProjectShelf,
            RegionId::Reader,
            RegionId::ContextRail,
            RegionId::Status,
        ] {
            assert_eq!(short.region(id).bounds.x, tall.region(id).bounds.x);
            assert_eq!(short.region(id).bounds.width, tall.region(id).bounds.width);
        }
        assert!(!short.safe_content().bounds.is_empty());
        assert!(tall.safe_content().bounds.height.get() > short.safe_content().bounds.height.get());
    }

    #[test]
    fn width_class_is_derived_from_the_same_stage() {
        assert_eq!(resolve(input(1280, 800, 100)).width, WidthClass::Wide);
        assert_eq!(
            resolve(input(620, 800, 100)).width,
            WidthClass::ContextSheet
        );
        assert_eq!(
            resolve(input(380, 800, 100)).width,
            WidthClass::SingleColumn
        );
    }

    #[test]
    fn semantic_identity_anchors_and_overflow_survive_collapse() {
        let ids = [
            RegionId::Titlebar,
            RegionId::OrbitRail,
            RegionId::ProjectShelf,
            RegionId::Reader,
            RegionId::ContextRail,
            RegionId::Status,
        ];
        for width in [90, 120, 121, 160, 239, 240, 241, 380, 617, 618, 620, 1280] {
            let layout = resolve(input(width, 800, 100));
            for id in ids {
                assert_eq!(layout.region(id).id, id);
            }
            let safe = layout.safe_content();
            assert_eq!(
                safe.document_overflow,
                crate::core::layout::HorizontalOverflow::Clip
            );
            assert_eq!(
                safe.source_overflow,
                crate::core::layout::HorizontalOverflow::Scroll
            );
            assert!(safe.bounds.right() <= LogicalPx::new(width));
            assert!(safe.bounds.bottom() <= LogicalPx::new(800));
            assert!(
                layout.region(RegionId::Reader).bounds.x
                    >= layout.region(RegionId::OrbitRail).bounds.right()
            );
            assert!(
                layout.region(RegionId::Status).bounds.y
                    >= layout.region(RegionId::Titlebar).bounds.bottom()
            );
            if layout.shelf == PanelMode::Sheet {
                assert_eq!(
                    layout.region(RegionId::ProjectShelf).presentation,
                    RegionPresentation::Sheet
                );
                assert!(layout.sheet_width(SheetKind::Shelf) <= LogicalPx::new(width));
            }
            if layout.context == PanelMode::Sheet {
                assert_eq!(
                    layout.region(RegionId::ContextRail).presentation,
                    RegionPresentation::Sheet
                );
                assert!(layout.sheet_width(SheetKind::Context) <= LogicalPx::new(width));
            }
        }
    }

    #[test]
    fn table_driven_stress_matrix_has_no_negative_or_unreachable_bounds() {
        let widths = [
            90, 119, 120, 121, 159, 160, 239, 240, 241, 379, 380, 617, 618, 620, 919, 1280, 1600,
        ];
        let heights = [1, 22, 36, 80, 160, 260, 480, 800, 1400];
        let scales = [80, 100, 150, 200];
        let ids = [
            RegionId::Titlebar,
            RegionId::OrbitRail,
            RegionId::ProjectShelf,
            RegionId::Reader,
            RegionId::ContextRail,
            RegionId::Status,
        ];
        for width in widths {
            for height in heights {
                for scale in scales {
                    let layout = resolve(input(width, height, scale));
                    for id in ids {
                        let bounds = layout.region(id).bounds;
                        assert!(bounds.right() <= LogicalPx::new(width));
                        assert!(bounds.bottom() <= LogicalPx::new(height));
                    }
                    let safe = layout.safe_content().bounds;
                    assert!(safe.right() <= LogicalPx::new(width));
                    assert!(safe.bottom() <= LogicalPx::new(height));
                    assert!(layout.sheet_width(SheetKind::Shelf) <= LogicalPx::new(width));
                    assert!(layout.sheet_width(SheetKind::Context) <= LogicalPx::new(width));
                }
            }
        }
    }
}
