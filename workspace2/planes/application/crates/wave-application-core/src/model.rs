//! Closed application vocabulary and bounded reply storage.

use crate::text::InputText;
use nudox_compile_vocab::{FrontendError, Language, Stage};

/// Largest result list accepted by the concrete service.
pub const MAX_REPLY_ROWS: u8 = 4;
/// Largest progress event page returned by one cursor poll.
pub const MAX_PROGRESS_ROWS: usize = 2;
/// Longest accepted semantic query or package name.
pub const MAX_SEMANTIC_TEXT_BYTES: usize = 32;

/// Application-wide correlation carried unchanged into replies and observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrelationId(pub u64);

/// One stable operation identity for bounded progress and cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationKey(pub u64);

/// The service-owned generation operation exposed after an accepted compiler request.
pub const APPLICATION_OPERATION: OperationKey = OperationKey(1);

/// A replay cursor that carries all client-specific progression state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressCursor {
    /// Beginning of the immutable bounded history.
    Start,
    /// The next unobserved immutable event ordinal.
    Offset(u8),
    /// The caller already observed the sole terminal fact.
    Finished,
}

/// Closed typed input accepted by the in-process service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationInput {
    /// Generate one package through an accepted compiler row.
    Generate {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated compiler language token.
        language: InputText,
        /// Unvalidated compiler stage token.
        stage: InputText,
        /// Unvalidated package name.
        package: InputText,
        /// Caller-bounded source bytes forwarded to the real compiler registry.
        source: InputText,
    },
    /// Inspect one immutable snapshot.
    SnapshotStatus {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
    },
    /// Search locally available snapshot facts.
    Search {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Unvalidated lexical query.
        query: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Request graph results from an absent upstream graph seam.
    Graph {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Request vector results from an absent upstream vector seam.
    Vector {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Inspect local snapshot residency.
    Locality {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
    },
    /// Inspect local and unavailable capability health without inventing backend success.
    Health {
        /// Request correlation.
        correlation: CorrelationId,
    },
    /// Admit one service-owned bounded progress handle before a later cancel or provider completion.
    BeginProgress {
        /// Request correlation.
        correlation: CorrelationId,
    },
    /// Replay a bounded immutable progress history.
    Progress {
        /// Request correlation.
        correlation: CorrelationId,
        /// Caller-owned cursor state.
        cursor: ProgressCursor,
    },
    /// Cancel the one admitted local operation.
    Cancel {
        /// Request correlation.
        correlation: CorrelationId,
        /// Operation to cancel.
        operation: OperationKey,
    },
}

impl ApplicationInput {
    /// Returns the unchanged caller correlation.
    #[must_use]
    pub const fn correlation(self) -> CorrelationId {
        match self {
            Self::Generate { correlation, .. }
            | Self::SnapshotStatus { correlation, .. }
            | Self::Search { correlation, .. }
            | Self::Graph { correlation, .. }
            | Self::Vector { correlation, .. }
            | Self::Locality { correlation, .. }
            | Self::Health { correlation }
            | Self::BeginProgress { correlation }
            | Self::Progress { correlation, .. }
            | Self::Cancel { correlation, .. } => correlation,
        }
    }
}

/// The one explicit terminal state for each semantic operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Terminal {
    /// Full result coverage.
    Complete {
        /// Number of emitted rows.
        emitted: u8,
    },
    /// Partial result coverage with exact unavailable capability.
    Partial {
        /// Number of emitted rows.
        emitted: u8,
        /// Exact unavailable source.
        unavailable: Capability,
    },
    /// Useful local facts remain while a remote capability is unavailable.
    Degraded {
        /// Number of emitted rows.
        emitted: u8,
        /// Exact unavailable source.
        unavailable: Capability,
    },
    /// The named cancel action won the only operation transition.
    Cancelled {
        /// Number of emitted rows before cancellation.
        emitted: u8,
    },
    /// Semantic validation or a compiler row failed.
    Failed,
}

/// Named capability health, never an unstructured availability string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    /// The local compiler registry.
    Compiler,
    /// The immutable-index seam, which is unavailable until a real provider is injected.
    Index,
    /// The absent remote graph provider.
    Graph,
    /// The absent remote vector provider.
    Vector,
}

/// One concrete health fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityHealth {
    /// A local capability is available now.
    LocalReady(Capability),
    /// A remote capability has no accepted upstream provider yet.
    Unavailable(Capability),
}

/// One immutable progress event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgressEvent {
    /// Stable chronology number.
    pub sequence: u8,
    /// Operation that owns the event.
    pub operation: OperationKey,
    /// Bounded work units completed at this point.
    pub completed_units: u8,
}

/// Fixed-capacity progress event page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProgressEvents {
    items: [Option<ProgressEvent>; MAX_PROGRESS_ROWS],
    length: u8,
}

impl ProgressEvents {
    /// Makes an empty page.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            items: [None; MAX_PROGRESS_ROWS],
            length: 0,
        }
    }

    pub(crate) fn push(&mut self, event: ProgressEvent) {
        self.items[usize::from(self.length)] = Some(event);
        self.length += 1;
    }

    /// Returns the exact row count.
    #[must_use]
    pub const fn len(&self) -> u8 {
        self.length
    }

    /// Reports whether the page has no retained progress events.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns one event by page ordinal.
    #[must_use]
    pub fn get(&self, ordinal: u8) -> Option<ProgressEvent> {
        self.items.get(usize::from(ordinal)).copied().flatten()
    }
}

/// A bounded cursor poll that cannot invent another terminal after `Finished`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressPage {
    /// One nonterminal immutable event page.
    Events {
        /// Ordered rows.
        events: ProgressEvents,
        /// Cursor for the next page.
        next: ProgressCursor,
    },
    /// The operation remains active and has no new service event yet.
    Pending {
        /// Unchanged caller cursor.
        cursor: ProgressCursor,
    },
    /// The sole observed operation terminal.
    Terminal {
        /// Exact operation terminal.
        terminal: Terminal,
        /// Fused next cursor.
        next: ProgressCursor,
    },
    /// A previously terminal cursor has no further state to report.
    Finished,
}

/// Structured business diagnostic code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// Unknown compiler language token.
    UnknownLanguage,
    /// Unknown compiler stage token.
    UnknownStage,
    /// Semantic text exceeded the command limit.
    SemanticTextTooLong,
    /// Requested row count exceeded the fixed bound.
    ResultLimitExceeded,
    /// A lower-plane dependency is unavailable rather than counterfeited by the service.
    DependencyUnavailable,
    /// Existing compiler registry rejected the selected vocabulary row.
    UnsupportedCompilerStage,
    /// Existing compiler registry returned a result that cannot fit the bounded text reply.
    CompilerOutputUnrepresentable,
    /// Progress cursor lay beyond immutable retained history.
    ProgressCursorOutOfRange,
    /// Cancellation named an unknown operation.
    OperationUnavailable,
}

/// Exact rejected operand/cause retained by a business diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticDetail {
    /// A validated transport token that failed closed semantic validation.
    Text(InputText),
    /// Exact requested and accepted result bounds.
    Limit {
        /// Rejected requested rows.
        requested: u8,
        /// Fixed service limit.
        maximum: u8,
    },
    /// Exact text length and semantic maximum.
    TextLength {
        /// Observed bytes.
        actual: usize,
        /// Accepted semantic bytes.
        maximum: usize,
        /// Complete field retained while it fits the transport bound.
        rejected: InputText,
    },
    /// Existing typed compiler rejection.
    Frontend(FrontendError),
    /// Exact cursor and immutable history length.
    Cursor {
        /// Rejected cursor offset.
        observed: u8,
        /// Available immutable event count.
        maximum: u8,
    },
    /// Exact unavailable operation identity.
    Operation(OperationKey),
    /// Exact lower-plane dependency that the service refuses to counterfeit.
    Capability(Capability),
}

/// One source-preserving service diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Closed diagnostic code.
    pub code: DiagnosticCode,
    /// Exact rejected operand or existing source cause.
    pub detail: DiagnosticDetail,
}

/// Semantic reply body; all business behavior is represented here rather than in adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyBody {
    /// The existing compiler registry accepted the source row.
    Generated {
        /// Package named by the request.
        package: InputText,
        /// Accepted compiler language.
        language: Language,
        /// Accepted compiler stage.
        stage: Stage,
        /// Exact caller source bytes returned by the accepted registry row.
        output: InputText,
    },
    /// A lower-plane dependency is not present in this application slice.
    DependencyUnavailable {
        /// Exact missing lower-plane capability.
        capability: Capability,
    },
    /// All known local/unavailable capability facts.
    Health([CapabilityHealth; 4]),
    /// One bounded client-owned replay cursor result.
    Progress(ProgressPage),
    /// A separate service-owned operation was admitted; its terminal is observed through `Progress`.
    ProgressStarted {
        /// Stable handle for cancellation and replay.
        operation: OperationKey,
    },
    /// Cancellation was applied to the named service-owned operation.
    Cancelled {
        /// Named operation whose terminal winner is cancellation.
        operation: OperationKey,
    },
    /// A typed failure has no fabricated payload.
    Rejected,
}

/// Fully structured service result shared by direct, CLI, MCP, and GPUI consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationReply {
    /// Correlation copied unchanged from the input.
    pub correlation: CorrelationId,
    /// One semantic body.
    pub body: ReplyBody,
    /// One request terminal.
    pub terminal: Terminal,
    /// A failure has exactly one source-preserving diagnostic in this first slice.
    pub diagnostic: Option<Diagnostic>,
}

/// Coarse typed observation that service adapters may record lazily.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationEvent {
    /// Correlated reply event.
    pub correlation: CorrelationId,
    /// Exact resulting request terminal.
    pub terminal: Terminal,
}
