//! Closed design-token vocabulary shared by shell views.
//!
//! Views choose a token family and semantic mark; they do not pass arbitrary
//! color names or state strings through the architecture. A visual theme can
//! map these values to the exact design-system palette at its edge.

/// Semantic palette channels used by shell surfaces and marks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PaletteChannel {
    /// Dense foreground and structural ink.
    Ink,
    /// Reading surface/paper.
    Paper,
    /// Quiet neutral chrome.
    Neutral,
    /// Navigation and command accent.
    Accent,
    /// Successful/available state.
    Success,
    /// Waiting or attention state.
    Warning,
    /// Fault or stopped state.
    Danger,
    /// De-emphasized metadata.
    Muted,
}

/// Stable shell surface families. Content routes share the same surface
/// vocabulary and do not grow page-specific token sets.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SurfaceToken {
    /// Orbit/shelf surface.
    Orbit,
    /// Package surface.
    Package,
    /// Declaration page surface.
    Page,
    /// Source reader surface.
    Source,
    /// Persistent shelf rail/panel.
    Shelf,
    /// Context panel.
    Context,
    /// Shell overlay/modal surface.
    Overlay,
}

/// Closed semantic marks that may be projected onto a palette channel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticMark {
    /// Untouched dense data.
    Plain,
    /// Pointer hover.
    Hovered,
    /// Keyboard focus.
    Focused,
    /// Current selection.
    Selected,
    /// Work in progress.
    Loading,
    /// Explicit fault.
    Error,
    /// Producer does not provide the value.
    Unavailable,
}

/// Closed density policy for data surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DensityToken {
    /// Plain-until-touched dense data.
    Dense,
    /// Breathing space used for shell controls.
    Roomy,
}
