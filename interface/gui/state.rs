//! Defines state behavior for `interface-gui`, whose purpose is to render and control the unified application service through GPUI.
//! This module owns the state invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed-capacity state projection for core application replies.

use core::ops::Deref;

use interface_core::{
    AdaptiveDisposition, ApplicationDisposition, ApplicationInput, ApplicationOutcome,
    ApplicationReply, Capability, CapabilityHealth, CorrelationId, Diagnostic, DiagnosticCode,
    DiagnosticDetail, ExecutionState, OperationKey, PackageCompilePhase, ReplyBody,
};

use crate::{
    CommandId, DocumentFilter, DocumentSearchScope, DocumentationState, FormError, FormField,
    FormState, NavigationState, PaletteDirection, PaletteEditError, ResultLimit, Route,
    ServiceAction,
};
use interface_core::InputText;

/// Maximum number of replies accepted at one UI boundary.
pub const MAX_BATCH_REPLIES: usize = 8;

/// Number of stable rows rendered by the shell.
pub const SURFACE_COUNT: usize = 7;

/// A stable presentation surface in the application shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    /// Compiler registry output.
    Generation,
    /// Pure adaptive placement decisions.
    Adaptive,
    /// Service-owned local capability execution.
    Execution,
    /// Snapshot/index-backed results.
    Index,
    /// Graph-backed results.
    Graph,
    /// Vector-backed results.
    Vector,
    /// Capability health facts.
    Health,
}

/// Coarse state label derived from the core disposition or capability facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionState {
    /// No core reply has reached this surface yet.
    Checking,
    /// A bounded operation was accepted for a separate execution owner.
    Accepted,
    /// The surface has usable local facts.
    Ready,
    /// The surface has useful facts but a named capability is unavailable.
    Degraded(Capability),
    /// The core cancelled this surface's operation.
    Cancelled,
    /// The core rejected or failed the operation.
    Failed,
    /// The surface has observed a nonterminal execution state.
    Active,
}

/// Explicit motion setting for the local GPUI projection.
///
/// This setting affects only keyed presentation transitions. It never schedules a timer, poll, or
/// continuous frame.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MotionPreference {
    /// Permit one short keyed transition for a discrete user interaction.
    #[default]
    Standard,
    /// Render discrete interaction changes immediately while preserving layout.
    Reduced,
    /// Render all interaction changes immediately.
    None,
}

/// Presentation status for a non-health, non-adaptive, non-execution surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceStatus {
    /// No reply has reached this surface yet.
    Checking,
    /// A body-bearing core disposition remains exact and non-duplicated.
    Resolved(ApplicationDisposition),
    /// A diagnostic-bearing failure remains available through `last_reply`.
    Failed {
        /// Closed diagnostic class retained by the owning core reply.
        code: DiagnosticCode,
    },
}

impl SurfaceStatus {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Resolved(disposition) => projection_from_disposition(disposition),
            Self::Failed { .. } => ProjectionState::Failed,
        }
    }
}

/// The last successful compiler/publication result retained as typed visible shell state.
///
/// The artifact is kept verbatim from the core reply, including every source, recipe, fragment,
/// and durable-publication authority. Its presence proves the core delivered a complete generated
/// body; the shell never constructs it from a failed outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeneratedProjection {
    /// Complete generated artifact authority from the core reply.
    pub artifact: interface_core::GeneratedArtifact,
}

/// Ordered visible progress for one exact pinned-package compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageJourneyProjection {
    /// Correlation of the package request that owns these phase facts.
    pub correlation: CorrelationId,
    /// Most recently entered ordered phase.
    pub last_phase: PackageCompilePhase,
    /// Number of distinct ordered phases entered so far.
    pub entered_phases: u8,
    /// True only after the corresponding complete or diagnostic reply was projected.
    pub complete: bool,
}

/// Direct projection of a pure adaptive policy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveProjection {
    /// No adaptive decision has reached the shell yet.
    Checking,
    /// The exact C6 decision and core disposition retained without conversion.
    Reported {
        /// Pure policy disposition.
        disposition: AdaptiveDisposition,
        /// Exact core body disposition.
        result: ApplicationDisposition,
    },
}

impl AdaptiveProjection {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Reported { result, .. } => projection_from_disposition(result),
        }
    }
}

/// Direct projection of a service-owned adaptive execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionProjection {
    /// No execution reply has reached the shell yet.
    Checking,
    /// The local action was accepted and is owned by the service future.
    Started {
        /// Core operation identity retained unchanged.
        operation: OperationKey,
        /// Exact selected local transition retained unchanged.
        transition: interface_core::CapabilityTransition,
    },
    /// The exact service-owned execution state.
    Reported {
        /// Finite execution observation.
        state: ExecutionState,
    },
}

impl ExecutionProjection {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Started { .. } => ProjectionState::Accepted,
            Self::Reported { state, .. } => match state {
                ExecutionState::Pending { .. } => ProjectionState::Active,
                ExecutionState::Completed { .. } => ProjectionState::Ready,
                ExecutionState::Cancelled { .. } => ProjectionState::Cancelled,
                ExecutionState::Failed { .. } => ProjectionState::Failed,
            },
        }
    }
}

/// Direct projection of the core health body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthProjection {
    /// No health reply has reached the shell yet.
    Checking,
    /// The exact core health array and core disposition.
    Reported {
        /// Core capability facts retained without conversion.
        facts: [CapabilityHealth; 6],
        /// Exact core body disposition.
        result: ApplicationDisposition,
    },
}

impl HealthProjection {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Reported { facts, result } => {
                if let Some(capability) = first_unavailable(facts) {
                    ProjectionState::Degraded(capability)
                } else {
                    projection_from_disposition(result)
                }
            }
        }
    }
}

/// One row in the fixed shell summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceSummary {
    /// Stable surface identity.
    pub surface: Surface,
    /// Derived state label.
    pub state: ProjectionState,
}

/// Deterministic visible facts for the Home route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HomePage {
    /// Compiler/IR availability projected from the exact core reply.
    pub generation: ProjectionState,
    /// Last generated artifact, when one has been published.
    pub generated: Option<GeneratedProjection>,
    /// Current or most recently completed pinned-package journey.
    pub package_journey: Option<PackageJourneyProjection>,
    /// Current local execution lifecycle.
    pub execution: ProjectionState,
    /// Current capability-health state.
    pub health: ProjectionState,
}

/// Deterministic visible facts for the Libraries route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LibrariesPage {
    /// Local snapshot/index availability.
    pub index: ProjectionState,
    /// Graph extension availability.
    pub graph: ProjectionState,
    /// Vector extension availability.
    pub vector: ProjectionState,
}

/// Deterministic visible facts for the Search route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SearchPage {
    /// Exact retrieval shares the local index fact.
    pub exact: ProjectionState,
    /// Lexical retrieval shares the local index fact.
    pub lexical: ProjectionState,
    /// Graph retrieval fact.
    pub graph: ProjectionState,
    /// Vector retrieval fact.
    pub vector: ProjectionState,
}

/// Deterministic visible facts for the Connections route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionsPage {
    /// Current placement decision.
    pub placement: ProjectionState,
    /// Current operation lifecycle.
    pub execution: ProjectionState,
    /// Health of local and remote connections.
    pub health: ProjectionState,
}

/// Deterministic visible facts for the Settings route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingsPage {
    /// Current capability-health state.
    pub health: ProjectionState,
    /// Last diagnostic code, if a core reply supplied a source-preserving diagnostic.
    pub diagnostic: Option<DiagnosticCode>,
    /// Last reply correlation retained for operational support.
    pub correlation: Option<CorrelationId>,
    /// Bounded coalesced update epoch.
    pub notification_epoch: u64,
}

/// Immutable, typed page projections rendered by the production GPUI shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageSnapshots {
    /// Home route state.
    pub home: HomePage,
    /// Libraries route state.
    pub libraries: LibrariesPage,
    /// Search route state.
    pub search: SearchPage,
    /// Connections route state.
    pub connections: ConnectionsPage,
    /// Settings route state.
    pub settings: SettingsPage,
}

/// Native text surface currently owning platform composition and selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextInputTarget {
    /// Global package and symbol search.
    DocumentationSearch,
    /// Command-palette query.
    Palette,
    /// Closed typed form field.
    Form(FormField),
}

/// Exact fixed-bound rejection from platform paste or composition input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTextInputError {
    /// The representable replacement exceeds fixed input capacity.
    InputTooLong {
        /// Native text surface that rejected the replacement.
        target: TextInputTarget,
        /// Requested UTF-8 byte length after replacement.
        actual: usize,
        /// Fixed accepted UTF-8 byte length.
        maximum: usize,
    },
    /// The mathematical joined replacement length cannot be represented by `usize`.
    InputLengthOverflow {
        /// Native text surface that rejected the replacement.
        target: TextInputTarget,
        /// Existing prefix byte length.
        prefix: usize,
        /// Inserted byte length.
        inserted: usize,
        /// Existing suffix byte length.
        suffix: usize,
    },
}

/// Fixed-capacity presentation state for one application window.
#[derive(Debug, Eq, PartialEq)]
pub struct ShellState {
    projection: ShellProjection,
}

/// Immutable public projection facts for one application window.
#[derive(Debug, Eq, PartialEq)]
pub struct ShellProjection {
    /// Stable product information architecture and visible-only palette state.
    pub navigation: NavigationState,
    /// Documentation catalog navigation, search, and disclosure state.
    pub documentation: DocumentationState,
    /// Explicit user motion preference, projected without platform sniffing.
    pub motion: MotionPreference,
    /// Compiler-generation surface state.
    pub generation: SurfaceStatus,
    /// Last successful generated artifact retained for the visible generation surface.
    pub generated: Option<GeneratedProjection>,
    /// Current or most recently completed pinned-package journey.
    pub package_journey: Option<PackageJourneyProjection>,
    /// Adaptive placement projection.
    pub adaptive: AdaptiveProjection,
    /// Service-owned execution projection.
    pub execution: ExecutionProjection,
    /// Index surface state.
    pub index: SurfaceStatus,
    /// Graph surface state.
    pub graph: SurfaceStatus,
    /// Vector surface state.
    pub vector: SurfaceStatus,
    /// Health projection.
    pub health: HealthProjection,
    /// Number of coalesced notification epochs.
    pub notification_epoch: u64,
    /// Last source-preserving core reply, owned without cloning its cold diagnostic allocation.
    pub last_reply: Option<ApplicationReply>,
    /// Route-specific visible state derived from the same core facts.
    pub pages: PageSnapshots,
    /// The active closed typed action form, if the user explicitly opened one.
    pub form: Option<FormState>,
    /// Most recent closed form rejection, rendered beside the form without string conversion.
    pub form_error: Option<FormError>,
    /// Most recent closed command-palette edit rejection.
    pub palette_error: Option<PaletteEditError>,
    /// Most recent bounded projection rejection, retained instead of being dropped by an
    /// asynchronous UI update.
    pub projection_error: Option<ApplyError>,
    /// Exact native text replacement rejection, if the fixed field bound rejected an edit.
    pub input_error: Option<NativeTextInputError>,
    /// Operation currently owned by the retained wake-driven foreground task.
    pub foreground_operation: Option<OperationKey>,
}

impl Deref for ShellState {
    type Target = ShellProjection;

    fn deref(&self) -> &Self::Target {
        &self.projection
    }
}

impl ShellState {
    /// Applies one ordered package-compilation phase without constructing a synthetic reply.
    ///
    /// # Errors
    ///
    /// Rejects an out-of-order phase, a correlation change during active work, or notification
    /// epoch exhaustion while retaining the exact observed operands.
    pub fn apply_package_phase(
        &mut self,
        correlation: CorrelationId,
        phase: PackageCompilePhase,
    ) -> Result<(), ApplyError> {
        let next = match self.projection.package_journey {
            None if phase == PackageCompilePhase::Locate => PackageJourneyProjection {
                correlation,
                last_phase: phase,
                entered_phases: 1,
                complete: false,
            },
            None => {
                return self.retain_projection_error(ApplyError::PackagePhaseWithoutLocate {
                    correlation,
                    observed: phase,
                });
            }
            Some(previous) if previous.complete && phase == PackageCompilePhase::Locate => {
                PackageJourneyProjection {
                    correlation,
                    last_phase: phase,
                    entered_phases: 1,
                    complete: false,
                }
            }
            Some(previous) if previous.correlation != correlation => {
                return self.retain_projection_error(ApplyError::PackagePhaseCorrelation {
                    active: previous.correlation,
                    observed: correlation,
                });
            }
            Some(previous)
                if package_phase_ordinal(phase)
                    == package_phase_ordinal(previous.last_phase).saturating_add(1) =>
            {
                PackageJourneyProjection {
                    correlation,
                    last_phase: phase,
                    entered_phases: previous.entered_phases.saturating_add(1),
                    complete: false,
                }
            }
            Some(previous) => {
                return self.retain_projection_error(ApplyError::PackagePhaseOrder {
                    correlation,
                    preceding: previous.last_phase,
                    observed: phase,
                });
            }
        };
        let Some(epoch) = self.projection.notification_epoch.checked_add(1) else {
            return self.retain_projection_error(ApplyError::NotificationEpochExhausted);
        };
        self.projection.package_journey = Some(next);
        self.projection.notification_epoch = epoch;
        self.projection.projection_error = None;
        self.refresh_pages();
        Ok(())
    }

    /// Selects a product route without changing application-service facts.
    pub fn select_route(&mut self, route: Route) {
        self.projection.navigation.select_route(route);
    }

    /// Contracts or expands the package navigation tree.
    pub fn toggle_sidebar(&mut self) {
        self.projection.documentation.sidebar_collapsed =
            !self.projection.documentation.sidebar_collapsed;
    }

    /// Selects one first-party document and opens the library reader.
    pub fn select_document(&mut self, item: usize) {
        self.projection.documentation.select_item(item);
        self.select_route(Route::Libraries);
    }

    /// Navigates to the previous visited document.
    pub fn navigate_document_back(&mut self) {
        self.projection.documentation.go_back();
        self.select_route(Route::Libraries);
    }

    /// Navigates to the next visited document.
    pub fn navigate_document_forward(&mut self) {
        self.projection.documentation.go_forward();
        self.select_route(Route::Libraries);
    }

    /// Toggles one package row in the library tree.
    pub fn toggle_document_package(&mut self, package: usize) {
        self.projection.documentation.toggle_package(package);
    }

    /// Replaces the global package and symbol search query.
    pub fn replace_documentation_query(&mut self, query: String) {
        self.projection.documentation.replace_query(query);
        self.select_route(Route::Search);
    }

    /// Clears the documentation search query.
    pub fn clear_documentation_query(&mut self) {
        self.projection.documentation.clear_query();
    }

    /// Selects an exact keyboard row in the unified documentation results.
    pub fn select_documentation_result(&mut self, index: usize) {
        self.projection.documentation.selected_result = index.min(
            self.projection
                .documentation
                .search_rows()
                .len()
                .saturating_sub(1),
        );
    }

    /// Moves the documentation result cursor by one row.
    pub fn move_documentation_result(&mut self, forward: bool) {
        self.projection.documentation.move_result_selection(forward);
    }

    /// Shows or hides the on-page document outline.
    pub fn set_documentation_outline_visible(&mut self, visible: bool) {
        self.projection.documentation.outline_visible = visible;
    }

    /// Expands or collapses the selected document's source section.
    pub fn toggle_documentation_source(&mut self) {
        self.projection.documentation.source_expanded =
            !self.projection.documentation.source_expanded;
    }

    /// Opens package discovery with an empty query.
    pub fn discover_packages(&mut self) {
        self.projection.documentation.clear_query();
        self.projection
            .documentation
            .select_scope(DocumentSearchScope::Packages);
        self.select_route(Route::Search);
    }

    /// Selects the global search population.
    pub fn select_documentation_scope(&mut self, scope: DocumentSearchScope) {
        self.projection.documentation.select_scope(scope);
        self.select_route(Route::Search);
    }

    /// Selects the symbol-kind filter used by documentation search.
    pub fn select_documentation_filter(&mut self, filter: DocumentFilter) {
        self.projection.documentation.select_filter(filter);
    }

    /// Adds or removes one catalog package from the reader's library.
    pub fn set_document_package_added(&mut self, package: usize, added: bool) {
        self.projection
            .documentation
            .set_package_added(package, added);
    }

    /// Focuses a closed visible form and selects its owning route.
    pub fn select_action(&mut self, action: ServiceAction) {
        self.projection.navigation.select_action(action);
        self.projection.form = Some(FormState::from_action(action, self.active_operation()));
        self.projection.form_error = None;
    }

    /// Replaces one active form field with transport-validated text.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when no form or matching field is active.
    pub fn replace_form_text(
        &mut self,
        field: FormField,
        value: InputText,
    ) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.replace_text(field, value));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Retains the exact rejection reported by a native platform text edit.
    #[cfg(feature = "real-gpui")]
    pub(crate) fn retain_form_error(&mut self, error: Option<FormError>) {
        self.projection.form_error = error;
    }

    /// Retains or clears the exact native text replacement rejection.
    #[cfg(feature = "real-gpui")]
    pub(crate) fn retain_input_error(&mut self, error: Option<NativeTextInputError>) {
        self.projection.input_error = error;
    }

    /// Publishes the operation currently owned by the wake-driven foreground task.
    #[cfg(feature = "real-gpui")]
    pub(crate) fn set_foreground_operation(&mut self, operation: Option<OperationKey>) {
        self.projection.foreground_operation = operation;
    }

    /// Replaces the active graph/vector/search typed result limit.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if this form has no bounded result limit.
    pub fn replace_form_limit(&mut self, value: ResultLimit) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.replace_limit(value));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Selects one visible field in the active closed form.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if no form or matching field is active.
    pub fn select_form_field(&mut self, field: FormField) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.select_field(field));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Moves keyboard focus to the next or previous field in the active closed form.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when the active form has no editable field.
    pub fn move_form_field(&mut self, forward: bool) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.move_field_focus(forward));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Appends keyboard text to the focused bounded form field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if the focused field cannot accept the text.
    pub fn append_form_text(&mut self, text: &str) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.append_focused_text(text));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Erases one character from the focused bounded form field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if the field is unavailable or empty.
    pub fn erase_form_text(&mut self) -> Result<(), FormError> {
        let result = self
            .projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)
            .and_then(FormState::erase_focused_text);
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Builds one typed service command from the active form without semantic revalidation.
    ///
    /// # Errors
    ///
    /// Returns a closed form error for a missing field or canonical authority.
    pub fn submit_form(
        &mut self,
        correlation: CorrelationId,
    ) -> Result<ApplicationInput, FormError> {
        let result = self
            .projection
            .form
            .ok_or(FormError::NoActiveForm)
            .and_then(|form| form.submit(correlation));
        retain_transition(&mut self.projection.form_error, result)
    }

    /// Closes the active typed form without submitting it.
    pub fn cancel_form(&mut self) {
        self.projection.form = None;
        self.projection.form_error = None;
    }

    /// Changes the local presentation motion policy without touching service state.
    pub fn set_motion_preference(&mut self, motion: MotionPreference) {
        self.projection.motion = motion;
    }

    /// Opens the visible-only command palette.
    pub fn open_palette(&mut self) {
        self.projection.palette_error = None;
        self.projection.navigation.palette.open();
    }

    /// Dismisses the command palette and clears its transient query.
    pub fn dismiss_palette(&mut self) {
        self.projection.navigation.palette.dismiss();
        self.projection.palette_error = None;
    }

    /// Moves the authoritative command identity and returns its virtual row for reveal.
    #[must_use]
    pub fn move_palette_selection(&mut self, direction: PaletteDirection) -> Option<usize> {
        self.projection.navigation.palette.move_selection(direction)
    }

    /// Selects a visible palette command by its stable identity.
    pub fn select_palette_command(&mut self, command: CommandId) {
        self.projection.navigation.palette.select(command);
    }

    /// Replaces the palette's visible-only filter with transport-validated text.
    pub fn replace_palette_query(&mut self, query: InputText) {
        self.projection.navigation.palette.replace_query(query);
        self.projection.palette_error = None;
    }

    /// Appends input typed while the command palette is visible.
    ///
    /// # Errors
    ///
    /// Returns the palette's closed edit rejection when the bounded query cannot retain the
    /// requested input.
    pub fn append_palette_text(&mut self, text: &str) -> Result<(), PaletteEditError> {
        let result = self.projection.navigation.palette.append_text(text);
        retain_transition(&mut self.projection.palette_error, result)
    }

    /// Erases one UTF-8 character from the visible command-palette query.
    ///
    /// # Errors
    ///
    /// Returns the palette's closed edit rejection when its canonical query is empty.
    pub fn erase_palette_character(&mut self) -> Result<(), PaletteEditError> {
        let result = self.projection.navigation.palette.erase_last_character();
        retain_transition(&mut self.projection.palette_error, result)
    }

    /// Confirms the palette selection, routing through the same navigation state as the rail.
    #[must_use]
    pub fn confirm_palette(&mut self) -> Option<CommandId> {
        let command = self.projection.navigation.palette.confirm()?;
        match command {
            CommandId::OpenRoute(route) => self.select_route(route),
            CommandId::InspectSurface(surface) => {
                self.select_route(crate::navigation::surface_destination(surface));
            }
            CommandId::FocusAction(action) => self.select_action(action),
        }
        Some(command)
    }
    /// Applies one owned core reply and emits one coalesced notification epoch.
    ///
    /// The shell takes ownership so a cold diagnostic's single allocation is retained directly
    /// rather than cloned into presentation state.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::NotificationEpochExhausted`] when the notification counter cannot
    /// advance.
    pub fn apply(&mut self, reply: ApplicationReply) -> Result<BatchReceipt, ApplyError> {
        self.apply_batch([reply])
    }

    /// Applies a caller-owned fixed reply batch and emits one coalesced notification epoch.
    ///
    /// The array is consumed so the last reply can retain its exact diagnostic without an `Arc`,
    /// clone, or second allocation. Its compile-time capacity is checked before any mutation.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::BatchTooLarge`] when the supplied array exceeds the fixed boundary,
    /// or [`ApplyError::NotificationEpochExhausted`] when the notification counter cannot advance.
    pub fn apply_batch<const REPLIES: usize>(
        &mut self,
        replies: [ApplicationReply; REPLIES],
    ) -> Result<BatchReceipt, ApplyError> {
        if REPLIES > MAX_BATCH_REPLIES {
            let error = ApplyError::BatchTooLarge {
                limit: MAX_BATCH_REPLIES,
                actual: REPLIES,
            };
            self.projection.projection_error = Some(error);
            return Err(error);
        }
        if REPLIES == 0 {
            self.projection.projection_error = None;
            return Ok(BatchReceipt {
                applied_replies: 0,
                notifications: 0,
                notification_epoch: self.projection.notification_epoch,
            });
        }

        let Some(epoch) = self.projection.notification_epoch.checked_add(1) else {
            let error = ApplyError::NotificationEpochExhausted;
            self.projection.projection_error = Some(error);
            return Err(error);
        };
        for reply in replies {
            self.apply_reply(&reply);
            self.projection.last_reply = Some(reply);
        }
        self.projection.notification_epoch = epoch;
        self.projection.projection_error = None;
        self.refresh_pages();
        Ok(BatchReceipt {
            applied_replies: REPLIES,
            notifications: 1,
            notification_epoch: epoch,
        })
    }

    /// Returns the fixed ordered summary used by the view.
    #[must_use]
    pub fn summaries(&self) -> [SurfaceSummary; SURFACE_COUNT] {
        [
            SurfaceSummary {
                surface: Surface::Generation,
                state: self.projection.generation.projection(),
            },
            SurfaceSummary {
                surface: Surface::Adaptive,
                state: self.projection.adaptive.projection(),
            },
            SurfaceSummary {
                surface: Surface::Execution,
                state: self.projection.execution.projection(),
            },
            SurfaceSummary {
                surface: Surface::Index,
                state: self.projection.index.projection(),
            },
            SurfaceSummary {
                surface: Surface::Graph,
                state: self.projection.graph.projection(),
            },
            SurfaceSummary {
                surface: Surface::Vector,
                state: self.projection.vector.projection(),
            },
            SurfaceSummary {
                surface: Surface::Health,
                state: self.projection.health.projection(),
            },
        ]
    }

    fn apply_reply(&mut self, reply: &ApplicationReply) {
        if let Some(journey) = self.projection.package_journey.as_mut()
            && journey.correlation == reply.correlation
        {
            journey.complete = true;
        }
        match &reply.outcome {
            ApplicationOutcome::Resolved(body) => self.apply_resolved(body),
            ApplicationOutcome::Failed { diagnostic } => self.apply_failure(diagnostic),
        }
    }

    fn apply_resolved(&mut self, body: &ReplyBody) {
        let result = ApplicationDisposition::from(body);
        match body {
            ReplyBody::Generated(artifact) => {
                self.projection.generation.set_resolved(result);
                self.projection.generated = Some(GeneratedProjection {
                    artifact: *artifact,
                });
            }
            ReplyBody::DependencyUnavailable { capability } => {
                if let Some(status) = self.status_for(*capability) {
                    status.set_resolved(result);
                }
            }
            ReplyBody::Health(facts) => {
                self.projection.health = HealthProjection::Reported {
                    facts: *facts,
                    result,
                };
                self.project_health_status(*facts);
            }
            ReplyBody::Adaptive(disposition) => {
                self.projection.adaptive = AdaptiveProjection::Reported {
                    disposition: *disposition,
                    result,
                };
            }
            ReplyBody::ExecutionStarted {
                operation,
                transition,
            } => {
                self.projection.execution = ExecutionProjection::Started {
                    operation: *operation,
                    transition: *transition,
                };
            }
            ReplyBody::Execution(state) => {
                self.projection.execution = ExecutionProjection::Reported {
                    state: (*state).into(),
                };
            }
            ReplyBody::Snapshot(_) | ReplyBody::Retrieval(_) | ReplyBody::IndexRemoved(_) => {
                self.projection.index.set_resolved(result);
            }
        }
    }

    fn apply_failure(&mut self, diagnostic: &Diagnostic) {
        match &diagnostic.detail {
            DiagnosticDetail::Compiler(_) | DiagnosticDetail::Frontend(_) => {
                self.projection.generation.set_failed(diagnostic.code);
            }
            DiagnosticDetail::Execution(state) => {
                self.projection.execution = ExecutionProjection::Reported { state: *state };
            }
            DiagnosticDetail::Capability(capability) => {
                if let Some(status) = self.status_for(*capability) {
                    status.set_failed(diagnostic.code);
                }
            }
            DiagnosticDetail::Retrieval(_) => {
                self.projection.index.set_failed(diagnostic.code);
            }
            DiagnosticDetail::Text(_)
            | DiagnosticDetail::Limit { .. }
            | DiagnosticDetail::TextLength { .. }
            | DiagnosticDetail::Operation(_)
            | DiagnosticDetail::Policy(_) => {}
        }
    }

    fn project_health_status(&mut self, facts: [CapabilityHealth; 6]) {
        for fact in facts {
            match fact {
                CapabilityHealth::LocalReady(capability) => {
                    if let Some(status) = self.status_for(capability) {
                        status.set_resolved(ApplicationDisposition::Complete { emitted: 1 });
                    }
                }
                CapabilityHealth::Unavailable(capability) => {
                    if let Some(status) = self.status_for(capability) {
                        status.set_resolved(ApplicationDisposition::Degraded {
                            emitted: 0,
                            unavailable: capability,
                        });
                    }
                }
            }
        }
    }

    fn status_for(&mut self, capability: Capability) -> Option<&mut SurfaceStatus> {
        match capability {
            Capability::CompilerRegistry | Capability::CompilerOutput => {
                Some(&mut self.projection.generation)
            }
            Capability::Index => Some(&mut self.projection.index),
            Capability::Graph => Some(&mut self.projection.graph),
            Capability::Vector => Some(&mut self.projection.vector),
            Capability::LocalAnalyzer | Capability::Remote => None,
        }
    }

    fn active_operation(&self) -> Option<OperationKey> {
        match self.projection.execution {
            ExecutionProjection::Started { operation, .. }
            | ExecutionProjection::Reported {
                state:
                    ExecutionState::Pending { operation, .. }
                    | ExecutionState::Completed { operation, .. }
                    | ExecutionState::Cancelled { operation, .. }
                    | ExecutionState::Failed { operation, .. },
                ..
            } => Some(operation),
            ExecutionProjection::Checking => None,
        }
    }

    fn refresh_pages(&mut self) {
        self.projection.pages = PageSnapshots {
            home: HomePage {
                generation: self.projection.generation.projection(),
                generated: self.projection.generated,
                package_journey: self.projection.package_journey,
                execution: self.projection.execution.projection(),
                health: self.projection.health.projection(),
            },
            libraries: LibrariesPage {
                index: self.projection.index.projection(),
                graph: self.projection.graph.projection(),
                vector: self.projection.vector.projection(),
            },
            search: SearchPage {
                exact: self.projection.index.projection(),
                lexical: self.projection.index.projection(),
                graph: self.projection.graph.projection(),
                vector: self.projection.vector.projection(),
            },
            connections: ConnectionsPage {
                placement: self.projection.adaptive.projection(),
                execution: self.projection.execution.projection(),
                health: self.projection.health.projection(),
            },
            settings: SettingsPage {
                health: self.projection.health.projection(),
                diagnostic: self.projection.last_reply.as_ref().and_then(|reply| {
                    if let ApplicationOutcome::Failed { diagnostic } = &reply.outcome {
                        Some(diagnostic.code)
                    } else {
                        None
                    }
                }),
                correlation: self
                    .projection
                    .last_reply
                    .as_ref()
                    .map(|reply| reply.correlation),
                notification_epoch: self.projection.notification_epoch,
            },
        };
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            projection: ShellProjection {
                navigation: NavigationState::default(),
                documentation: DocumentationState::default(),
                motion: MotionPreference::default(),
                generation: SurfaceStatus::Checking,
                generated: None,
                package_journey: None,
                adaptive: AdaptiveProjection::Checking,
                execution: ExecutionProjection::Checking,
                index: SurfaceStatus::Checking,
                graph: SurfaceStatus::Checking,
                vector: SurfaceStatus::Checking,
                health: HealthProjection::Checking,
                notification_epoch: 0,
                last_reply: None,
                pages: PageSnapshots {
                    home: HomePage {
                        generation: ProjectionState::Checking,
                        generated: None,
                        package_journey: None,
                        execution: ProjectionState::Checking,
                        health: ProjectionState::Checking,
                    },
                    libraries: LibrariesPage {
                        index: ProjectionState::Checking,
                        graph: ProjectionState::Checking,
                        vector: ProjectionState::Checking,
                    },
                    search: SearchPage {
                        exact: ProjectionState::Checking,
                        lexical: ProjectionState::Checking,
                        graph: ProjectionState::Checking,
                        vector: ProjectionState::Checking,
                    },
                    connections: ConnectionsPage {
                        placement: ProjectionState::Checking,
                        execution: ProjectionState::Checking,
                        health: ProjectionState::Checking,
                    },
                    settings: SettingsPage {
                        health: ProjectionState::Checking,
                        diagnostic: None,
                        correlation: None,
                        notification_epoch: 0,
                    },
                },
                form: None,
                form_error: None,
                palette_error: None,
                projection_error: None,
                input_error: None,
                foreground_operation: None,
            },
        }
    }
}

fn retain_transition<Value, Error: Copy>(
    slot: &mut Option<Error>,
    result: Result<Value, Error>,
) -> Result<Value, Error> {
    match result {
        Ok(value) => {
            *slot = None;
            Ok(value)
        }
        Err(error) => {
            *slot = Some(error);
            Err(error)
        }
    }
}

impl SurfaceStatus {
    fn set_resolved(&mut self, result: ApplicationDisposition) {
        *self = Self::Resolved(result);
    }

    fn set_failed(&mut self, code: DiagnosticCode) {
        *self = Self::Failed { code };
    }
}

impl ShellState {
    pub(crate) fn retain_projection_error(&mut self, error: ApplyError) -> Result<(), ApplyError> {
        self.projection.projection_error = Some(error);
        Err(error)
    }
}

/// Result of one shell state boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchReceipt {
    /// Number of core replies projected.
    pub applied_replies: usize,
    /// Number of GPUI notifications requested for this boundary.
    pub notifications: u8,
    /// Resulting coalesced notification epoch.
    pub notification_epoch: u64,
}

/// A bounded projection error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyError {
    /// The caller supplied more replies than one UI boundary accepts.
    BatchTooLarge {
        /// Maximum accepted replies.
        limit: usize,
        /// Supplied replies.
        actual: usize,
    },
    /// The monotonically increasing notification epoch reached its finite bound.
    NotificationEpochExhausted,
    /// A phase after `Locate` arrived without the journey's first fact.
    PackagePhaseWithoutLocate {
        /// Exact request correlation.
        correlation: CorrelationId,
        /// First phase incorrectly observed.
        observed: PackageCompilePhase,
    },
    /// A second request attempted to interleave with one active package journey.
    PackagePhaseCorrelation {
        /// Correlation currently owning the journey.
        active: CorrelationId,
        /// Correlation that attempted to enter a phase.
        observed: CorrelationId,
    },
    /// One package phase skipped, repeated, or moved backward.
    PackagePhaseOrder {
        /// Exact journey correlation.
        correlation: CorrelationId,
        /// Last valid phase.
        preceding: PackageCompilePhase,
        /// Invalid next phase.
        observed: PackageCompilePhase,
    },
    /// A second package request was submitted while the first retained task remained active.
    PackageRequestInFlight {
        /// Correlation already owned by the compiler task.
        active: CorrelationId,
        /// Newly submitted correlation that was not admitted.
        observed: CorrelationId,
    },
}

const fn package_phase_ordinal(phase: PackageCompilePhase) -> u8 {
    match phase {
        PackageCompilePhase::Locate => 0,
        PackageCompilePhase::EnterSource => 1,
        PackageCompilePhase::Authority => 2,
        PackageCompilePhase::Lower => 3,
        PackageCompilePhase::Publish => 4,
        PackageCompilePhase::Reopen => 5,
        PackageCompilePhase::Discover => 6,
        PackageCompilePhase::Render => 7,
    }
}

const fn projection_from_disposition(disposition: ApplicationDisposition) -> ProjectionState {
    match disposition {
        ApplicationDisposition::Accepted { .. } => ProjectionState::Accepted,
        ApplicationDisposition::Complete { .. } => ProjectionState::Ready,
        ApplicationDisposition::Partial { unavailable, .. }
        | ApplicationDisposition::Degraded { unavailable, .. } => {
            ProjectionState::Degraded(unavailable)
        }
        ApplicationDisposition::Cancelled { .. } => ProjectionState::Cancelled,
    }
}

const fn first_unavailable(facts: [CapabilityHealth; 6]) -> Option<Capability> {
    let mut index = 0;
    while index < facts.len() {
        if let CapabilityHealth::Unavailable(capability) = facts[index] {
            return Some(capability);
        }
        index += 1;
    }
    None
}
