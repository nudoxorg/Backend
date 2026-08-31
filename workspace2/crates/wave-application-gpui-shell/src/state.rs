//! Fixed-capacity state projection for core application replies.

use core::ops::Deref;

use wave_application_core::{
    AdaptiveDisposition, ApplicationReply, Capability, CapabilityHealth, CorrelationId,
    DiagnosticCode, ExecutionState, OperationKey, ReplyBody, Terminal,
};

use crate::{CommandId, NavigationState, PaletteDirection, Route};
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

impl Surface {
    /// Returns the stable visible label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Generation => "Generation",
            Self::Adaptive => "Adaptive",
            Self::Execution => "Execution",
            Self::Index => "Index",
            Self::Graph => "Graph",
            Self::Vector => "Vector",
            Self::Health => "Health",
        }
    }

    /// Returns the stable element identity used by the GPUI view.
    #[must_use]
    pub const fn element_id(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Adaptive => "adaptive",
            Self::Execution => "execution",
            Self::Index => "index",
            Self::Graph => "graph",
            Self::Vector => "vector",
            Self::Health => "health",
        }
    }
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

impl ProjectionState {
    /// Returns the stable visible label without allocating.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking",
            Self::Accepted => "Accepted",
            Self::Ready => "Ready",
            Self::Degraded(_) => "Degraded",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
            Self::Active => "Active",
        }
    }
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
        /// Optional source-preserving diagnostic code.
        diagnostic: Option<DiagnosticCode>,
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
    /// Returns the retained adaptive disposition, if present.
    #[must_use]
    pub const fn disposition(self) -> Option<AdaptiveDisposition> {
        match self {
            Self::Checking => None,
            Self::Reported { disposition, .. } => Some(disposition),
        }
    }

    /// Returns the visible state of the adaptive surface.
    #[must_use]
    pub const fn projection(self) -> ProjectionState {
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
    /// Returns the retained execution observation, if polling has happened.
    #[must_use]
    pub const fn state(self) -> Option<ExecutionState> {
        match self {
            Self::Checking | Self::Started { .. } => None,
            Self::Reported { state, .. } => Some(state),
        }
    }

    /// Returns the visible state of the execution surface.
    #[must_use]
    pub const fn projection(self) -> ProjectionState {
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
    /// Returns the retained core health facts, if present.
    #[must_use]
    pub const fn facts(self) -> Option<[CapabilityHealth; 6]> {
        match self {
            Self::Checking => None,
            Self::Reported { facts, .. } => Some(facts),
        }
    }

    /// Returns the health surface's visible state.
    #[must_use]
    pub const fn projection(self) -> ProjectionState {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LastReply {
    correlation: CorrelationId,
    terminal: Terminal,
    diagnostic: Option<DiagnosticCode>,
}

/// Fixed-capacity presentation state for one application window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellState {
    projection: ShellProjection,
    last_reply: Option<LastReply>,
}

/// Immutable public projection facts for one application window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellProjection {
    /// Stable product information architecture and visible-only palette state.
    pub navigation: NavigationState,
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

    /// Confirms the palette selection, routing through the same navigation state as the rail.
    #[must_use]
    pub fn confirm_palette(&mut self) -> Option<Route> {
        let route = self.projection.navigation.palette.confirm()?;
        self.select_route(route);
        Some(route)
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

    /// Returns the last core correlation, if a reply was applied.
    #[must_use]
    pub const fn last_correlation(&self) -> Option<CorrelationId> {
        match self.last_reply {
            Some(reply) => Some(reply.correlation),
            None => None,
        }
    }

    /// Returns the last core terminal, if a reply was applied.
    #[must_use]
    pub const fn last_terminal(&self) -> Option<Terminal> {
        match self.last_reply {
            Some(reply) => Some(reply.terminal),
            None => None,
        }
    }

    /// Returns the last source-preserving diagnostic code, if present.
    #[must_use]
    pub const fn last_diagnostic_code(&self) -> Option<DiagnosticCode> {
        match self.last_reply {
            Some(reply) => reply.diagnostic,
            None => None,
        }
    }

    fn apply_reply(&mut self, reply: &ApplicationReply) {
        self.last_reply = Some(LastReply {
            correlation: reply.correlation,
            terminal: reply.terminal,
            diagnostic: reply.diagnostic.map(|diagnostic| diagnostic.code),
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
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            projection: ShellProjection {
                navigation: NavigationState::default(),
                generation: SurfaceStatus::Checking,
                adaptive: AdaptiveProjection::Checking,
                execution: ExecutionProjection::Checking,
                index: SurfaceStatus::Checking,
                graph: SurfaceStatus::Checking,
                vector: SurfaceStatus::Checking,
                health: HealthProjection::Checking,
                notification_epoch: 0,
            },
            last_reply: None,
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

const fn status_from_terminal(
    terminal: Terminal,
    diagnostic: Option<wave_application_core::Diagnostic>,
) -> SurfaceStatus {
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
            diagnostic: match diagnostic {
                Some(value) => Some(value.code),
                None => None,
            },
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
