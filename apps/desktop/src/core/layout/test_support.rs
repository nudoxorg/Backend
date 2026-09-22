//! Capture matrix contract for a future native GPUI `test_support` harness.

use super::input::{LayoutInput, LogicalPx, PanelPreferences, TextScale, WindowContentSize};
use super::regions::RegionId;
use super::resolver::ResponsiveLayout;

/// Expected wide shell bounds used by native capture support.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExpectedWideGeometry {
    /// Titlebar height.
    pub titlebar: u32,
    /// Orbit width.
    pub orbit: u32,
    /// Shelf width.
    pub shelf: u32,
    /// Context width.
    pub context: u32,
    /// Status height.
    pub status: u32,
}

/// One table-driven capture fixture. `route` and `state` are labels for the
/// harness manifest; layout itself does not own either value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureCase {
    /// Stable fixture ID.
    pub id: &'static str,
    /// Exact logical viewport.
    pub viewport: WindowContentSize,
    /// Text scale.
    pub text_scale: TextScale,
    /// Product route label supplied by the harness.
    pub route: &'static str,
    /// Product state label supplied by the harness.
    pub state: &'static str,
    /// Expected wide geometry, when the fixture is wide.
    pub expected_wide: Option<ExpectedWideGeometry>,
}

/// Canonical capture matrix. The native harness may add interaction frames
/// around these rows without rebuilding a second responsive authority.
pub const CAPTURE_MATRIX: &[CaptureCase] = &[
    CaptureCase {
        id: "wide-package-rest",
        viewport: WindowContentSize::new(LogicalPx::new(1280), LogicalPx::new(800)),
        text_scale: TextScale::percent(100),
        route: "package/overview",
        state: "rest",
        expected_wide: Some(ExpectedWideGeometry {
            titlebar: 50,
            orbit: 54,
            shelf: 264,
            context: 300,
            status: 26,
        }),
    },
    CaptureCase {
        id: "context-sheet-620",
        viewport: WindowContentSize::new(LogicalPx::new(620), LogicalPx::new(800)),
        text_scale: TextScale::percent(100),
        route: "package/overview",
        state: "context-sheet",
        expected_wide: None,
    },
    CaptureCase {
        id: "shelf-sheet-380",
        viewport: WindowContentSize::new(LogicalPx::new(380), LogicalPx::new(800)),
        text_scale: TextScale::percent(100),
        route: "source/detail",
        state: "shelf-sheet",
        expected_wide: None,
    },
    CaptureCase {
        id: "compact-navigation-240",
        viewport: WindowContentSize::new(LogicalPx::new(240), LogicalPx::new(480)),
        text_scale: TextScale::percent(100),
        route: "docs/reader",
        state: "compact",
        expected_wide: None,
    },
    CaptureCase {
        id: "icon-status-90",
        viewport: WindowContentSize::new(LogicalPx::new(90), LogicalPx::new(160)),
        text_scale: TextScale::percent(100),
        route: "package/overview",
        state: "icon-status",
        expected_wide: None,
    },
    CaptureCase {
        id: "large-text-short",
        viewport: WindowContentSize::new(LogicalPx::new(620), LogicalPx::new(260)),
        text_scale: TextScale::percent(200),
        route: "source/detail",
        state: "short-window",
        expected_wide: None,
    },
];

/// Resolves one capture row using the production authority.
#[must_use]
pub fn resolve_case(case: CaptureCase) -> ResponsiveLayout {
    super::resolver::resolve(LayoutInput::new(
        case.viewport.width(),
        case.viewport.height(),
        case.text_scale,
        PanelPreferences {
            shelf_open: true,
            context_open: true,
        },
    ))
}

/// Returns the expected geometry field for a wide fixture.
#[must_use]
pub fn wide_geometry(layout: ResponsiveLayout) -> Option<ExpectedWideGeometry> {
    let titlebar = layout.region(RegionId::Titlebar).bounds.height.get();
    let orbit = layout.region(RegionId::OrbitRail).bounds.width.get();
    let shelf = layout.region(RegionId::ProjectShelf).bounds.width.get();
    let context = layout.region(RegionId::ContextRail).bounds.width.get();
    let status = layout.region(RegionId::Status).bounds.height.get();
    Some(ExpectedWideGeometry {
        titlebar,
        orbit,
        shelf,
        context,
        status,
    })
}
