//! Fixed-capacity state projection for core application replies.

use core::ops::Deref;

use wave_application_core::{
    AdaptiveDisposition, ApplicationInput, ApplicationReply, Capability, CapabilityHealth,
    CorrelationId, Diagnostic, ExecutionState, OperationKey, ReplyBody, Terminal,
};

use crate::{
    CommandId, FormError, FormField, FormState, NavigationState, PaletteDirection,
    PaletteEditError, ResultLimit, Route, ServiceAction,
};
use wave_application_core::InputText;

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

/// Coarse state label derived from the core terminal or capability facts.
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
    /// A separate bounded operation was accepted.
    Accepted {
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
    /// A complete terminal was projected.
    Ready {
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
    /// A named capability was unavailable.
    Degraded {
        /// Core terminal retained unchanged.
        terminal: Terminal,
        /// Core capability retained unchanged.
        capability: Capability,
    },
    /// The core cancelled the operation.
    Cancelled {
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
    /// The core rejected the operation.
    Failed {
        /// Core terminal retained unchanged.
        terminal: Terminal,
        /// Optional source-preserving typed diagnostic.
        diagnostic: Option<Diagnostic>,
    },
}

impl SurfaceStatus {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Accepted { .. } => ProjectionState::Accepted,
            Self::Ready { .. } => ProjectionState::Ready,
            Self::Degraded { capability, .. } => ProjectionState::Degraded(capability),
            Self::Cancelled { .. } => ProjectionState::Cancelled,
            Self::Failed { .. } => ProjectionState::Failed,
        }
    }
}

/// Direct projection of a pure adaptive policy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveProjection {
    /// No adaptive decision has reached the shell yet.
    Checking,
    /// The exact C6 decision and terminal retained without conversion.
    Reported {
        /// Pure policy disposition.
        disposition: AdaptiveDisposition,
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
}

impl AdaptiveProjection {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Reported { terminal, .. } => terminal_projection(terminal),
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
        transition: wave_application_core::CapabilityTransition,
        /// Core accepted terminal retained unchanged.
        terminal: Terminal,
    },
    /// The exact service-owned execution state and terminal.
    Reported {
        /// Finite execution observation.
        state: ExecutionState,
        /// Core terminal retained unchanged.
        terminal: Terminal,
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
    /// The exact core health array and terminal.
    Reported {
        /// Core capability facts retained without conversion.
        facts: [CapabilityHealth; 6],
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
}

impl HealthProjection {
    const fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Reported { facts, terminal } => {
                if let Some(capability) = first_unavailable(facts) {
                    ProjectionState::Degraded(capability)
                } else {
                    terminal_projection(terminal)
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
    /// Last source-preserving diagnostic, if a core reply supplied one.
    pub diagnostic: Option<Diagnostic>,
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

/// Source-preserving metadata from the most recently projected core reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplyProjection {
    /// Correlation retained directly from the core reply.
    pub correlation: CorrelationId,
    /// Terminal retained directly from the core reply.
    pub terminal: Terminal,
    /// Full typed diagnostic retained without dropping its detail/cause.
    pub diagnostic: Option<Diagnostic>,
}

/// Fixed-capacity presentation state for one application window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellState {
    projection: ShellProjection,
}

/// Immutable public projection facts for one application window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellProjection {
    /// Stable product information architecture and visible-only palette state.
    pub navigation: NavigationState,
    /// Explicit user motion preference, projected without platform sniffing.
    pub motion: MotionPreference,
    /// Compiler-generation surface state.
    pub generation: SurfaceStatus,
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
    /// Last source-preserving core reply facts, if one reached this view.
    pub last_reply: Option<ReplyProjection>,
    /// Route-specific visible state derived from the same core facts.
    pub pages: PageSnapshots,
    /// The active closed typed action form, if the user explicitly opened one.
    pub form: Option<FormState>,
    /// Most recent closed form rejection, rendered beside the form without string conversion.
    pub form_error: Option<FormError>,
}

impl Deref for ShellState {
    type Target = ShellProjection;

    fn deref(&self) -> &Self::Target {
        &self.projection
    }
}

impl ShellState {
    /// Selects a product route without changing application-service facts.
    pub fn select_route(&mut self, route: Route) {
        self.projection.navigation.select_route(route);
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
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .replace_text(field, value)
    }

    /// Replaces the active graph/vector/search typed result limit.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if this form has no bounded result limit.
    pub fn replace_form_limit(&mut self, value: ResultLimit) -> Result<(), FormError> {
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .replace_limit(value)
    }

    /// Selects one visible field in the active closed form.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if no form or matching field is active.
    pub fn select_form_field(&mut self, field: FormField) -> Result<(), FormError> {
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .select_field(field)
    }

    /// Moves keyboard focus to the next or previous field in the active closed form.
    ///
    /// # Errors
    ///
    /// Returns a closed form error when the active form has no editable field.
    pub fn move_form_field(&mut self, forward: bool) -> Result<(), FormError> {
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .move_field_focus(forward)
    }

    /// Appends keyboard text to the focused bounded form field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if the focused field cannot accept the text.
    pub fn append_form_text(&mut self, text: &str) -> Result<(), FormError> {
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .append_focused_text(text)
    }

    /// Erases one character from the focused bounded form field.
    ///
    /// # Errors
    ///
    /// Returns a closed form error if the field is unavailable or empty.
    pub fn erase_form_text(&mut self) -> Result<(), FormError> {
        self.projection
            .form
            .as_mut()
            .ok_or(FormError::NoActiveForm)?
            .erase_focused_text()
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
        self.projection.form_error = result.as_ref().err().copied();
        result
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
        self.projection.navigation.palette.open();
    }

    /// Dismisses the command palette and clears its transient query.
    pub fn dismiss_palette(&mut self) {
        self.projection.navigation.palette.dismiss();
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
    }

    /// Appends input typed while the command palette is visible.
    ///
    /// # Errors
    ///
    /// Returns the palette's closed edit rejection when the bounded query cannot retain the
    /// requested input.
    pub fn append_palette_text(&mut self, text: &str) -> Result<(), PaletteEditError> {
        self.projection.navigation.palette.append_text(text)
    }

    /// Erases one UTF-8 character from the visible command-palette query.
    ///
    /// # Errors
    ///
    /// Returns the palette's closed edit rejection when its canonical query is empty.
    pub fn erase_palette_character(&mut self) -> Result<(), PaletteEditError> {
        self.projection.navigation.palette.erase_last_character()
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
    /// Applies a caller-owned bounded reply slice and emits one coalesced notification epoch.
    ///
    /// The length check happens before any projection mutation. The state contains only fixed
    /// arrays, enums, and core copy types; no `Vec`, `String`, task, timer, or background polling
    /// state is created here.
    ///
    /// # Errors
    ///
    /// Returns [`ApplyError::BatchTooLarge`] when the supplied slice exceeds the fixed boundary,
    /// or [`ApplyError::NotificationEpochExhausted`] when the notification counter cannot advance.
    pub fn apply_batch(
        &mut self,
        replies: &[ApplicationReply],
    ) -> Result<BatchReceipt, ApplyError> {
        if replies.len() > MAX_BATCH_REPLIES {
            return Err(ApplyError::BatchTooLarge {
                limit: MAX_BATCH_REPLIES,
                actual: replies.len(),
            });
        }
        if replies.is_empty() {
            return Ok(BatchReceipt {
                applied_replies: 0,
                notifications: 0,
                notification_epoch: self.projection.notification_epoch,
            });
        }

        let epoch = self
            .projection
            .notification_epoch
            .checked_add(1)
            .ok_or(ApplyError::NotificationEpochExhausted)?;
        for reply in replies {
            self.apply_reply(reply);
        }
        self.projection.notification_epoch = epoch;
        self.refresh_pages();
        Ok(BatchReceipt {
            applied_replies: replies.len(),
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
        self.projection.last_reply = Some(ReplyProjection {
            correlation: reply.correlation,
            terminal: reply.terminal,
            diagnostic: reply.diagnostic,
        });

        match reply.body {
            ReplyBody::DependencyUnavailable { capability } => {
                if let Some(status) = self.status_for(capability) {
                    status.set_degraded(reply.terminal, capability);
                }
            }
            ReplyBody::Health(facts) => {
                self.projection.health = HealthProjection::Reported {
                    facts,
                    terminal: reply.terminal,
                };
                self.project_health_status(facts, reply.terminal);
            }
            ReplyBody::Adaptive(disposition) => {
                self.projection.adaptive = AdaptiveProjection::Reported {
                    disposition,
                    terminal: reply.terminal,
                };
            }
            ReplyBody::ExecutionStarted {
                operation,
                transition,
            } => {
                self.projection.execution = ExecutionProjection::Started {
                    operation,
                    transition,
                    terminal: reply.terminal,
                };
            }
            ReplyBody::Execution(state) => {
                self.projection.execution = ExecutionProjection::Reported {
                    state,
                    terminal: reply.terminal,
                };
            }
            ReplyBody::Rejected => {}
        }
    }

    fn project_health_status(&mut self, facts: [CapabilityHealth; 6], terminal: Terminal) {
        for fact in facts {
            match fact {
                CapabilityHealth::LocalReady(capability) => {
                    if let Some(status) = self.status_for(capability) {
                        status.set_ready(terminal);
                    }
                }
                CapabilityHealth::Unavailable(capability) => {
                    if let Some(status) = self.status_for(capability) {
                        status.set_degraded(terminal, capability);
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
                diagnostic: self
                    .projection
                    .last_reply
                    .and_then(|reply| reply.diagnostic),
                correlation: self.projection.last_reply.map(|reply| reply.correlation),
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
                motion: MotionPreference::default(),
                generation: SurfaceStatus::Checking,
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
            },
        }
    }
}

impl SurfaceStatus {
    fn set_ready(&mut self, terminal: Terminal) {
        *self = SurfaceStatus::Ready { terminal };
    }

    fn set_degraded(&mut self, terminal: Terminal, capability: Capability) {
        *self = SurfaceStatus::Degraded {
            terminal,
            capability,
        };
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
}

const fn status_from_terminal(terminal: Terminal, diagnostic: Option<Diagnostic>) -> SurfaceStatus {
    match terminal {
        Terminal::Accepted { .. } => SurfaceStatus::Accepted { terminal },
        Terminal::Complete { .. } => SurfaceStatus::Ready { terminal },
        Terminal::Partial { unavailable, .. } | Terminal::Degraded { unavailable, .. } => {
            SurfaceStatus::Degraded {
                terminal,
                capability: unavailable,
            }
        }
        Terminal::Cancelled { .. } => SurfaceStatus::Cancelled { terminal },
        Terminal::Failed => SurfaceStatus::Failed {
            terminal,
            diagnostic,
        },
    }
}

const fn terminal_projection(terminal: Terminal) -> ProjectionState {
    match status_from_terminal(terminal, None) {
        SurfaceStatus::Checking => ProjectionState::Checking,
        SurfaceStatus::Accepted { .. } => ProjectionState::Accepted,
        SurfaceStatus::Ready { .. } => ProjectionState::Ready,
        SurfaceStatus::Degraded { capability, .. } => ProjectionState::Degraded(capability),
        SurfaceStatus::Cancelled { .. } => ProjectionState::Cancelled,
        SurfaceStatus::Failed { .. } => ProjectionState::Failed,
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
