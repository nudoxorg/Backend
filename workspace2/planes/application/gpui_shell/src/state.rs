//! Fixed-capacity state projection for core application replies.

use wave_application_core::{
    ApplicationReply, Capability, CapabilityHealth, CorrelationId, DiagnosticCode, OperationKey,
    ProgressPage, ReplyBody, Terminal,
};

/// Maximum number of replies accepted at one UI boundary.
pub const MAX_BATCH_REPLIES: usize = 8;

/// Number of stable rows rendered by the shell.
pub const SURFACE_COUNT: usize = 6;

/// A stable presentation surface in the application shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Surface {
    /// Compiler generation results.
    Generation,
    /// Snapshot/index-backed results.
    Index,
    /// Graph-backed results.
    Graph,
    /// Vector-backed results.
    Vector,
    /// Capability health facts.
    Health,
    /// Cursor-based progress facts.
    Progress,
}

impl Surface {
    /// Returns the stable visible label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Generation => "Generation",
            Self::Index => "Index",
            Self::Graph => "Graph",
            Self::Vector => "Vector",
            Self::Health => "Health",
            Self::Progress => "Progress",
        }
    }

    /// Returns the stable element identity used by the GPUI view.
    #[must_use]
    pub const fn element_id(self) -> &'static str {
        match self {
            Self::Generation => "generation",
            Self::Index => "index",
            Self::Graph => "graph",
            Self::Vector => "vector",
            Self::Health => "health",
            Self::Progress => "progress",
        }
    }
}

/// Coarse state label derived from the core terminal or capability facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionState {
    /// No core reply has reached this surface yet.
    Checking,
    /// The surface has usable local facts.
    Ready,
    /// The surface has useful facts but a named capability is unavailable.
    Degraded(Capability),
    /// The core cancelled this surface's operation.
    Cancelled,
    /// The core rejected or failed the operation.
    Failed,
    /// The surface has observed a nonterminal progress page.
    Active,
}

impl ProjectionState {
    /// Returns the stable visible label without allocating.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking",
            Self::Ready => "Ready",
            Self::Degraded(_) => "Degraded",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
            Self::Active => "Active",
        }
    }
}

/// Presentation status for a non-health, non-progress surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceStatus {
    /// No reply has reached this surface yet.
    Checking,
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
            Self::Ready { .. } => ProjectionState::Ready,
            Self::Degraded { capability, .. } => ProjectionState::Degraded(capability),
            Self::Cancelled { .. } => ProjectionState::Cancelled,
            Self::Failed { .. } => ProjectionState::Failed,
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
        facts: [CapabilityHealth; 4],
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
}

impl HealthProjection {
    /// Returns the retained core health facts, if present.
    #[must_use]
    pub const fn facts(self) -> Option<[CapabilityHealth; 4]> {
        match self {
            Self::Checking => None,
            Self::Reported { facts, .. } => Some(facts),
        }
    }

    /// Returns the health surface's visible state.
    #[must_use]
    pub fn projection(self) -> ProjectionState {
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

/// Direct projection of the core progress page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressProjection {
    /// No progress reply has reached the shell yet.
    Checking,
    /// A core-owned progress operation was admitted.
    Started {
        /// Core operation handle retained unchanged.
        operation: OperationKey,
    },
    /// The exact core cursor page retained without conversion.
    Reported(ProgressPage),
    /// A core-owned progress operation was cancelled.
    Cancelled {
        /// Core operation handle retained unchanged.
        operation: OperationKey,
        /// Core terminal retained unchanged.
        terminal: Terminal,
    },
}

impl ProgressProjection {
    /// Returns the retained core progress page, if present.
    #[must_use]
    pub const fn page(self) -> Option<ProgressPage> {
        match self {
            Self::Checking | Self::Started { .. } | Self::Cancelled { .. } => None,
            Self::Reported(page) => Some(page),
        }
    }

    fn projection(self) -> ProjectionState {
        match self {
            Self::Checking => ProjectionState::Checking,
            Self::Started { .. }
            | Self::Reported(ProgressPage::Events { .. } | ProgressPage::Pending { .. }) => {
                ProjectionState::Active
            }
            Self::Reported(ProgressPage::Terminal { terminal, .. }) => {
                terminal_projection(terminal)
            }
            Self::Reported(ProgressPage::Finished) => ProjectionState::Ready,
            Self::Cancelled { .. } => ProjectionState::Cancelled,
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
    generation: SurfaceStatus,
    index: SurfaceStatus,
    graph: SurfaceStatus,
    vector: SurfaceStatus,
    health: HealthProjection,
    progress: ProgressProjection,
    last_reply: Option<LastReply>,
    notification_epoch: u64,
}

impl ShellState {
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
                notification_epoch: self.notification_epoch,
            });
        }

        let epoch = self
            .notification_epoch
            .checked_add(1)
            .ok_or(ApplyError::NotificationEpochExhausted)?;
        for reply in replies {
            self.apply_reply(reply);
        }
        self.notification_epoch = epoch;
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
                state: self.generation.projection(),
            },
            SurfaceSummary {
                surface: Surface::Index,
                state: self.index.projection(),
            },
            SurfaceSummary {
                surface: Surface::Graph,
                state: self.graph.projection(),
            },
            SurfaceSummary {
                surface: Surface::Vector,
                state: self.vector.projection(),
            },
            SurfaceSummary {
                surface: Surface::Health,
                state: self.health.projection(),
            },
            SurfaceSummary {
                surface: Surface::Progress,
                state: self.progress.projection(),
            },
        ]
    }

    /// Returns the projected health body.
    #[must_use]
    pub const fn health(&self) -> HealthProjection {
        self.health
    }

    /// Returns the projected progress body.
    #[must_use]
    pub const fn progress(&self) -> ProgressProjection {
        self.progress
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

    /// Returns the number of coalesced notification epochs.
    #[must_use]
    pub const fn notification_epoch(&self) -> u64 {
        self.notification_epoch
    }

    fn apply_reply(&mut self, reply: &ApplicationReply) {
        self.last_reply = Some(LastReply {
            correlation: reply.correlation,
            terminal: reply.terminal,
            diagnostic: reply.diagnostic.map(|diagnostic| diagnostic.code),
        });

        match reply.body {
            ReplyBody::Generated { .. } => {
                self.generation = status_from_terminal(reply.terminal, reply.diagnostic);
            }
            ReplyBody::DependencyUnavailable { capability } => {
                self.status_for(capability)
                    .set_degraded(reply.terminal, capability);
            }
            ReplyBody::Health(facts) => {
                self.health = HealthProjection::Reported {
                    facts,
                    terminal: reply.terminal,
                };
                self.project_health_status(facts, reply.terminal);
            }
            ReplyBody::Progress(page) => {
                self.progress = ProgressProjection::Reported(page);
            }
            ReplyBody::ProgressStarted { operation } => {
                self.progress = ProgressProjection::Started { operation };
            }
            ReplyBody::Cancelled { operation } => {
                self.progress = ProgressProjection::Cancelled {
                    operation,
                    terminal: reply.terminal,
                };
            }
            ReplyBody::Rejected => {}
        }
    }

    fn project_health_status(&mut self, facts: [CapabilityHealth; 4], terminal: Terminal) {
        for fact in facts {
            match fact {
                CapabilityHealth::LocalReady(capability) => {
                    self.status_for(capability).set_ready(terminal);
                }
                CapabilityHealth::Unavailable(capability) => {
                    self.status_for(capability)
                        .set_degraded(terminal, capability);
                }
            }
        }
    }

    fn status_for(&mut self, capability: Capability) -> &mut SurfaceStatus {
        match capability {
            Capability::Compiler => &mut self.generation,
            Capability::Index => &mut self.index,
            Capability::Graph => &mut self.graph,
            Capability::Vector => &mut self.vector,
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            generation: SurfaceStatus::Checking,
            index: SurfaceStatus::Checking,
            graph: SurfaceStatus::Checking,
            vector: SurfaceStatus::Checking,
            health: HealthProjection::Checking,
            progress: ProgressProjection::Checking,
            last_reply: None,
            notification_epoch: 0,
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
    applied_replies: usize,
    notifications: u8,
    notification_epoch: u64,
}

impl BatchReceipt {
    /// Returns the number of core replies projected.
    #[must_use]
    pub const fn applied_replies(self) -> usize {
        self.applied_replies
    }

    /// Returns the number of GPUI notifications requested for this boundary.
    #[must_use]
    pub const fn notifications(self) -> u8 {
        self.notifications
    }

    /// Returns the resulting coalesced notification epoch.
    #[must_use]
    pub const fn notification_epoch(self) -> u64 {
        self.notification_epoch
    }
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

fn status_from_terminal(
    terminal: Terminal,
    diagnostic: Option<wave_application_core::Diagnostic>,
) -> SurfaceStatus {
    match terminal {
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
            diagnostic: diagnostic.map(|value| value.code),
        },
    }
}

fn terminal_projection(terminal: Terminal) -> ProjectionState {
    match status_from_terminal(terminal, None) {
        SurfaceStatus::Checking => ProjectionState::Checking,
        SurfaceStatus::Ready { .. } => ProjectionState::Ready,
        SurfaceStatus::Degraded { capability, .. } => ProjectionState::Degraded(capability),
        SurfaceStatus::Cancelled { .. } => ProjectionState::Cancelled,
        SurfaceStatus::Failed { .. } => ProjectionState::Failed,
    }
}

fn first_unavailable(facts: [CapabilityHealth; 4]) -> Option<Capability> {
    facts.into_iter().find_map(|fact| match fact {
        CapabilityHealth::LocalReady(_) => None,
        CapabilityHealth::Unavailable(capability) => Some(capability),
    })
}
