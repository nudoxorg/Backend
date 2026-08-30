//! Bounded GPUI projection for the Wave application service.
//!
//! The shell owns presentation state only. Its input is the production
//! [`wave_application_core::ApplicationReply`] value, so the service remains
//! the sole owner of validation, capability facts, progress cursors, and
//! terminal semantics. The fallback projection is useful to headless callers;
//! the `real-gpui` feature adds the entity-backed [`GpuiShellView`].

mod state;

pub use state::{
    ApplyError, BatchReceipt, HealthProjection, MAX_BATCH_REPLIES, ProgressProjection,
    ProjectionState, SURFACE_COUNT, ShellState, Surface, SurfaceStatus, SurfaceSummary,
};

// These are re-exports of the core types, not shell-owned DTOs. Re-exporting
// them keeps a GPUI consumer's import surface small while preserving one
// business vocabulary.
pub use wave_application_core::{
    ApplicationReply, Capability, CapabilityHealth, CorrelationId, DiagnosticCode, OperationKey,
    ProgressCursor, ProgressPage, ReplyBody, Terminal,
};

#[cfg(feature = "real-gpui")]
mod view;

#[cfg(feature = "real-gpui")]
pub use view::GpuiShellView;
