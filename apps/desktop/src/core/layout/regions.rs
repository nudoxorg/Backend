//! Typed semantic regions produced by the resolver.

use super::input::LogicalPx;

/// Stable IDs for shell regions. These IDs are semantic anchors, not view
/// instance IDs, so a pane can move between flow and sheet presentation while
/// retaining its action, focus, and scroll identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RegionId {
    /// Window title and command controls.
    Titlebar,
    /// Persistent primary navigation rail.
    OrbitRail,
    /// Project chooser/shelf.
    ProjectShelf,
    /// Main reader/content column.
    Reader,
    /// Contextual package/document navigation.
    ContextRail,
    /// Persistent status row.
    Status,
}

/// A half-open logical rectangle.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct RegionBounds {
    /// Left edge.
    pub x: LogicalPx,
    /// Top edge.
    pub y: LogicalPx,
    /// Width.
    pub width: LogicalPx,
    /// Height.
    pub height: LogicalPx,
}

impl RegionBounds {
    /// The empty rectangle at the origin.
    pub const ZERO: Self = Self {
        x: LogicalPx::ZERO,
        y: LogicalPx::ZERO,
        width: LogicalPx::ZERO,
        height: LogicalPx::ZERO,
    };

    /// Creates a rectangle from four logical lengths.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x: LogicalPx::new(x),
            y: LogicalPx::new(y),
            width: LogicalPx::new(width),
            height: LogicalPx::new(height),
        }
    }

    /// Returns the right edge without overflow.
    #[must_use]
    pub const fn right(self) -> LogicalPx {
        self.x.saturating_add(self.width)
    }

    /// Returns the bottom edge without overflow.
    #[must_use]
    pub const fn bottom(self) -> LogicalPx {
        self.y.saturating_add(self.height)
    }

    /// Reports whether the rectangle has no paintable area.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width.get() == 0 || self.height.get() == 0
    }
}

/// Whether a semantic region participates in normal flow.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RegionPresentation {
    /// Full reading presentation in normal flow.
    Full,
    /// Compact icon/rail presentation in normal flow.
    Rail,
    /// Available through a component-managed sheet/drawer.
    Sheet,
    /// Not requested or not currently reachable in flow.
    Hidden,
}

/// Stable panel vocabulary retained for callers that need a compact mode
/// check without inspecting the complete region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PanelMode {
    /// The panel does not occupy flow.
    Hidden,
    /// The panel occupies its compact rail width.
    Rail,
    /// The panel occupies its full reading width.
    Full,
    /// The panel is presented by a sheet trigger.
    Sheet,
}

impl PanelMode {
    /// Returns whether this mode has a flow rectangle.
    #[must_use]
    pub const fn in_flow(self) -> bool {
        matches!(self, Self::Rail | Self::Full)
    }

    /// Converts the mode to its semantic presentation.
    #[must_use]
    pub const fn presentation(self) -> RegionPresentation {
        match self {
            Self::Hidden => RegionPresentation::Hidden,
            Self::Rail => RegionPresentation::Rail,
            Self::Full => RegionPresentation::Full,
            Self::Sheet => RegionPresentation::Sheet,
        }
    }
}

/// A resolved region with stable semantic identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RegionSlot {
    /// Stable semantic identity.
    pub id: RegionId,
    /// Flow rectangle. Sheet regions use [`RegionBounds::ZERO`].
    pub bounds: RegionBounds,
    /// Flow/sheet/hidden presentation.
    pub presentation: RegionPresentation,
}

impl RegionSlot {
    /// Creates a region slot.
    #[must_use]
    pub const fn new(id: RegionId, bounds: RegionBounds, presentation: RegionPresentation) -> Self {
        Self {
            id,
            bounds,
            presentation,
        }
    }
}

/// Horizontal overflow policy for readable content.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HorizontalOverflow {
    /// The document reader keeps a bounded measure and clips excess paint.
    Clip,
    /// Source/code content scrolls horizontally while its gutter remains fixed.
    Scroll,
}

/// Readable area guaranteed by the current shell geometry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SafeContentBounds {
    /// The full safe reader rectangle.
    pub bounds: RegionBounds,
    /// Preferred document measure within that rectangle.
    pub document_measure: LogicalPx,
    /// Documents keep a bounded measure.
    pub document_overflow: HorizontalOverflow,
    /// Source readers retain a fixed gutter and horizontal scroll policy.
    pub source_overflow: HorizontalOverflow,
}

/// Which pane a sheet represents.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SheetKind {
    /// Project chooser/shelf sheet.
    Shelf,
    /// Contextual rail sheet.
    Context,
    /// Details sheet used at the icon/status floor.
    Details,
}

/// Side from which a sheet enters the shell.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SheetSide {
    /// Left-side project chooser.
    Left,
    /// Right-side contextual details.
    Right,
}

/// A component-managed sheet presentation. `open` remains a presentation
/// concern; reducer-owned pane preference is represented by the containing
/// region and is never duplicated as transient focus or route state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SheetPresentation {
    /// Sheet semantic identity.
    pub kind: SheetKind,
    /// Entry side.
    pub side: SheetSide,
    /// Clamped sheet bounds.
    pub bounds: RegionBounds,
    /// Whether the CE sheet should currently be mounted open.
    pub open: bool,
}

/// Overlay/sheet presentation emitted by the canonical resolver.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OverlayPresentation {
    /// No collapsed pane requires a sheet trigger.
    None,
    /// One collapsed pane is the deterministic primary sheet target.
    Sheet(SheetPresentation),
}

/// All shell regions and the safe reader rectangle for one frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ShellRegions {
    /// Titlebar region.
    pub titlebar: RegionSlot,
    /// Orbit rail region.
    pub orbit_rail: RegionSlot,
    /// Project shelf region.
    pub project_shelf: RegionSlot,
    /// Reader region.
    pub reader: RegionSlot,
    /// Context rail region.
    pub context_rail: RegionSlot,
    /// Status region.
    pub status: RegionSlot,
    /// Safe content and overflow policies.
    pub safe_content: SafeContentBounds,
    /// Sheet/drawer presentation.
    pub overlay: OverlayPresentation,
}

impl ShellRegions {
    /// Looks up a region by stable semantic ID without allocating.
    #[must_use]
    pub const fn get(self, id: RegionId) -> RegionSlot {
        match id {
            RegionId::Titlebar => self.titlebar,
            RegionId::OrbitRail => self.orbit_rail,
            RegionId::ProjectShelf => self.project_shelf,
            RegionId::Reader => self.reader,
            RegionId::ContextRail => self.context_rail,
            RegionId::Status => self.status,
        }
    }
}
