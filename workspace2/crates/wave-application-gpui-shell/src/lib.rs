//! Bounded GPUI projection for the Wave application service.
//!
//! The shell owns presentation state only. Its input is the production
//! [`wave_application_core::ApplicationReply`] value, so the entity-owned
//! service remains the sole owner of validation, capability facts, execution,
//! and terminal semantics. The fallback projection is useful to headless
//! callers; the `real-gpui` feature adds the entity-backed [`GpuiShellView`].

mod state;

pub use state::{
    AdaptiveProjection, ApplyError, BatchReceipt, ExecutionProjection, HealthProjection,
    MAX_BATCH_REPLIES, ProjectionState, SURFACE_COUNT, ShellProjection, ShellState, Surface,
    SurfaceStatus, SurfaceSummary,
};

// These are re-exports of the core types, not shell-owned DTOs. Re-exporting
// them keeps a GPUI consumer's import surface small while preserving one
// business vocabulary.
pub use wave_application_core::{
    AdaptiveDisposition, ApplicationInput, ApplicationReply, ApplicationService, Capability,
    CapabilityHealth, CapabilityTransition, CorrelationId, DiagnosticCode, ExecutionState,
    OperationKey, ReplyBody, Terminal,
};

#[cfg(feature = "real-gpui")]
mod view;

#[cfg(feature = "real-gpui")]
pub use view::GpuiShellView;
