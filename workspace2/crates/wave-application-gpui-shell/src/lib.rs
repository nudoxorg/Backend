//! Bounded GPUI projection for the Wave application service.
//!
//! The shell owns presentation state only. Its input is the production
//! [`wave_application_core::ApplicationReply`] value, so the entity-owned
//! service remains the sole owner of validation, capability facts, execution,
//! and terminal semantics. The fallback projection is useful to headless
//! callers; the `real-gpui` feature adds the entity-backed [`GpuiShellView`].

mod forms;
mod navigation;
mod state;

#[cfg(feature = "real-gpui")]
gpui::actions!(
    wave_application_shell,
    [
        /// Opens the keyboard-first command palette.
        OpenPalette,
        /// Dismisses the visible command palette.
        DismissPalette,
        /// Moves the palette selection down one visible row.
        SelectNextPaletteCommand,
        /// Moves the palette selection up one visible row.
        SelectPreviousPaletteCommand,
        /// Confirms the selected palette command.
        ConfirmPaletteCommand,
        /// Selects the first visible command-palette row.
        SelectFirstPaletteCommand,
        /// Selects the final visible command-palette row.
        SelectLastPaletteCommand,
        /// Advances the palette selection by one fixed render window.
        SelectNextPalettePage,
        /// Moves the palette selection back by one fixed render window.
        SelectPreviousPalettePage,
        /// Opens the Settings destination.
        OpenSettings,
        /// Advances keyboard focus to the next active form field.
        NextFormField,
        /// Moves keyboard focus to the previous active form field.
        PreviousFormField,
        /// Cancels the active form or dismisses the visible palette.
        DismissForm
    ]
);

pub use navigation::{
    CommandFacts, CommandId, CommandPalette, MAX_PALETTE_RESULTS, NavigationState,
    PALETTE_PAGE_ROWS, PaletteDirection, PaletteEditError, ROUTES, Route, RouteFacts, SURFACES,
    ServiceAction, ShortcutFacts, SurfaceFacts,
};

pub use forms::{
    FormError, FormField, FormState, OperationAction, RecoveryAction, RecoverySelection,
    ResultLimit, SnapshotAction,
};

pub use state::{
    AdaptiveProjection, ApplyError, BatchReceipt, ConnectionsPage, ExecutionProjection,
    HealthProjection, HomePage, LibrariesPage, MAX_BATCH_REPLIES, MotionPreference,
    NativeTextInputError, PageSnapshots, ProjectionState, ReplyProjection, SURFACE_COUNT,
    SearchPage, SettingsPage, ShellProjection, ShellState, Surface, SurfaceStatus, SurfaceSummary,
    TextInputTarget,
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
