//! The concrete, fixture-free application behavior owner.

use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{Language, Stage};
use nudox_observe::Probe;

use crate::{
    APPLICATION_OPERATION, ApplicationEvent, ApplicationInput, ApplicationReply, Capability,
    CapabilityHealth, Diagnostic, DiagnosticCode, DiagnosticDetail, InputText, MAX_REPLY_ROWS,
    MAX_SEMANTIC_TEXT_BYTES, OperationKey, ProgressCursor, ProgressEvent, ProgressEvents,
    ProgressPage, ReplyBody, Terminal,
};

/// One concrete service with exactly one mutable, bounded application-owned operation state.
///
/// The service uses the real compiler registry today. Immutable index, graph, and vector providers
/// have no accepted production seam yet, so their commands return typed degraded dependency facts
/// rather than fixture data or claimed lower-plane completion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationService {
    operation: OperationPhase,
}

/// The private operation owner prevents cancellation from becoming completion later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationPhase {
    Idle,
    Active,
    Cancelled,
}

/// A private progress computation avoids an oversized `Result` error payload.
enum ProgressOutcome {
    Page(ProgressPage),
    Rejected(Diagnostic),
}

impl ApplicationService {
    /// Creates an empty service with no fabricated lower-plane facts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            operation: OperationPhase::Idle,
        }
    }

    /// Executes one request with disabled typed tracing.
    #[must_use]
    pub fn execute(&mut self, input: &ApplicationInput) -> ApplicationReply {
        let mut disabled = ();
        self.execute_observed(input, &mut disabled)
    }

    /// Executes one request and lazily emits one typed event only when the supplied probe retains it.
    #[must_use]
    pub fn execute_observed<Observer>(
        &mut self,
        input: &ApplicationInput,
        probe: &mut Observer,
    ) -> ApplicationReply
    where
        Observer: Probe<ApplicationEvent>,
    {
        let reply = self.dispatch(input);
        probe.record_with(|| ApplicationEvent {
            correlation: reply.correlation,
            terminal: reply.terminal,
        });
        reply
    }

    fn dispatch(&mut self, input: &ApplicationInput) -> ApplicationReply {
        match *input {
            ApplicationInput::Generate {
                correlation,
                language,
                stage,
                package,
                source,
            } => Self::generate(correlation, language, stage, package, source),
            ApplicationInput::SnapshotStatus {
                correlation,
                snapshot,
            }
            | ApplicationInput::Locality {
                correlation,
                snapshot,
            } => Self::index_unavailable(correlation, snapshot),
            ApplicationInput::Search {
                correlation,
                snapshot,
                query,
                limit,
            } => Self::search_unavailable(correlation, snapshot, query, limit),
            ApplicationInput::Graph {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(correlation, snapshot, limit, Capability::Graph),
            ApplicationInput::Vector {
                correlation,
                snapshot,
                limit,
            } => Self::remote_unavailable(correlation, snapshot, limit, Capability::Vector),
            ApplicationInput::Health { correlation } => Self::health(correlation),
            ApplicationInput::BeginProgress { correlation } => self.begin_progress(correlation),
            ApplicationInput::Progress {
                correlation,
                cursor,
            } => self.progress(correlation, cursor),
            ApplicationInput::Cancel {
                correlation,
                operation,
            } => self.cancel(correlation, operation),
        }
    }

    fn generate(
        correlation: crate::CorrelationId,
        language: InputText,
        stage: InputText,
        package: InputText,
        source: InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(package) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(source) {
            return Self::rejected(correlation, diagnostic);
        }
        let Some(language) = Self::language(language) else {
            return Self::rejected(correlation, Self::unknown_language(language));
        };
        let Some(stage) = Self::stage(stage) else {
            return Self::rejected(correlation, Self::unknown_stage(stage));
        };
        match FullRegistry.dispatch(language, stage, source.as_bytes()) {
            Ok(compiled) => match InputText::from_compiler_bytes(compiled) {
                Some(output) => ApplicationReply {
                    correlation,
                    body: ReplyBody::Generated {
                        package,
                        language,
                        stage,
                        output,
                    },
                    terminal: Terminal::Complete { emitted: 1 },
                    diagnostic: None,
                },
                None => Self::rejected(
                    correlation,
                    Diagnostic {
                        code: DiagnosticCode::CompilerOutputUnrepresentable,
                        detail: DiagnosticDetail::Text(source),
                    },
                ),
            },
            Err(source) => Self::rejected(
                correlation,
                Diagnostic {
                    code: DiagnosticCode::UnsupportedCompilerStage,
                    detail: DiagnosticDetail::Frontend(source),
                },
            ),
        }
    }

    fn index_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, Capability::Index)
    }

    fn search_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
        query: InputText,
        limit: u8,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::limit(limit) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(query) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, Capability::Index)
    }

    fn remote_unavailable(
        correlation: crate::CorrelationId,
        snapshot: InputText,
        limit: u8,
        capability: Capability,
    ) -> ApplicationReply {
        if let Some(diagnostic) = Self::limit(limit) {
            return Self::rejected(correlation, diagnostic);
        }
        if let Some(diagnostic) = Self::text_bound(snapshot) {
            return Self::rejected(correlation, diagnostic);
        }
        Self::dependency_unavailable(correlation, capability)
    }

    fn dependency_unavailable(
        correlation: crate::CorrelationId,
        capability: Capability,
    ) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::DependencyUnavailable { capability },
            terminal: Terminal::Degraded {
                emitted: 0,
                unavailable: capability,
            },
            diagnostic: Some(Diagnostic {
                code: DiagnosticCode::DependencyUnavailable,
                detail: DiagnosticDetail::Capability(capability),
            }),
        }
    }

    fn health(correlation: crate::CorrelationId) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::Health([
                CapabilityHealth::LocalReady(Capability::Compiler),
                CapabilityHealth::Unavailable(Capability::Index),
                CapabilityHealth::Unavailable(Capability::Graph),
                CapabilityHealth::Unavailable(Capability::Vector),
            ]),
            terminal: Terminal::Complete { emitted: 4 },
            diagnostic: None,
        }
    }

    fn begin_progress(&mut self, correlation: crate::CorrelationId) -> ApplicationReply {
        if self.operation != OperationPhase::Idle {
            return Self::rejected(
                correlation,
                Self::operation_unavailable(APPLICATION_OPERATION),
            );
        }
        self.operation = OperationPhase::Active;
        ApplicationReply {
            correlation,
            body: ReplyBody::ProgressStarted {
                operation: APPLICATION_OPERATION,
            },
            terminal: Terminal::Complete { emitted: 1 },
            diagnostic: None,
        }
    }

    fn progress(
        &self,
        correlation: crate::CorrelationId,
        cursor: ProgressCursor,
    ) -> ApplicationReply {
        let outcome = match cursor {
            ProgressCursor::Finished => ProgressOutcome::Page(ProgressPage::Finished),
            ProgressCursor::Start => self.progress_from(0),
            ProgressCursor::Offset(offset) => self.progress_from(offset),
        };
        match outcome {
            ProgressOutcome::Rejected(diagnostic) => Self::rejected(correlation, diagnostic),
            ProgressOutcome::Page(page) => ApplicationReply {
                correlation,
                body: ReplyBody::Progress(page),
                terminal: Terminal::Complete { emitted: 1 },
                diagnostic: None,
            },
        }
    }

    fn progress_from(&self, offset: u8) -> ProgressOutcome {
        if self.operation == OperationPhase::Cancelled {
            return ProgressOutcome::Page(ProgressPage::Terminal {
                terminal: Terminal::Cancelled { emitted: 0 },
                next: ProgressCursor::Finished,
            });
        }
        if self.operation == OperationPhase::Idle {
            if offset == 0 {
                return ProgressOutcome::Page(ProgressPage::Terminal {
                    terminal: Terminal::Complete { emitted: 0 },
                    next: ProgressCursor::Finished,
                });
            }
            return Self::cursor_rejected(offset, 0);
        }
        match offset {
            0 => {
                let mut events = ProgressEvents::empty();
                events.push(ProgressEvent {
                    sequence: 0,
                    operation: APPLICATION_OPERATION,
                    completed_units: 0,
                });
                ProgressOutcome::Page(ProgressPage::Events {
                    events,
                    next: ProgressCursor::Offset(1),
                })
            }
            1 => ProgressOutcome::Page(ProgressPage::Pending {
                cursor: ProgressCursor::Offset(1),
            }),
            _ => Self::cursor_rejected(offset, 1),
        }
    }

    fn cancel(
        &mut self,
        correlation: crate::CorrelationId,
        operation: OperationKey,
    ) -> ApplicationReply {
        if operation != APPLICATION_OPERATION || self.operation != OperationPhase::Active {
            return Self::rejected(correlation, Self::operation_unavailable(operation));
        }
        self.operation = OperationPhase::Cancelled;
        ApplicationReply {
            correlation,
            body: ReplyBody::Cancelled { operation },
            terminal: Terminal::Cancelled { emitted: 0 },
            diagnostic: None,
        }
    }

    fn language(value: InputText) -> Option<Language> {
        if value.is("rust") {
            Some(Language::RustSubset)
        } else if value.is("typescript") {
            Some(Language::TypeScriptSubset)
        } else {
            None
        }
    }

    fn stage(value: InputText) -> Option<Stage> {
        if value.is("parse") {
            Some(Stage::Parse)
        } else if value.is("lower-ir") {
            Some(Stage::LowerIr)
        } else {
            None
        }
    }

    fn text_bound(value: InputText) -> Option<Diagnostic> {
        if value.len() > MAX_SEMANTIC_TEXT_BYTES {
            Some(Diagnostic {
                code: DiagnosticCode::SemanticTextTooLong,
                detail: DiagnosticDetail::TextLength {
                    actual: value.len(),
                    maximum: MAX_SEMANTIC_TEXT_BYTES,
                    rejected: value,
                },
            })
        } else {
            None
        }
    }

    fn limit(requested: u8) -> Option<Diagnostic> {
        if requested > MAX_REPLY_ROWS {
            Some(Diagnostic {
                code: DiagnosticCode::ResultLimitExceeded,
                detail: DiagnosticDetail::Limit {
                    requested,
                    maximum: MAX_REPLY_ROWS,
                },
            })
        } else {
            None
        }
    }

    fn unknown_language(value: InputText) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::UnknownLanguage,
            detail: DiagnosticDetail::Text(value),
        }
    }

    fn unknown_stage(value: InputText) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::UnknownStage,
            detail: DiagnosticDetail::Text(value),
        }
    }

    fn cursor_rejected(observed: u8, maximum: u8) -> ProgressOutcome {
        ProgressOutcome::Rejected(Diagnostic {
            code: DiagnosticCode::ProgressCursorOutOfRange,
            detail: DiagnosticDetail::Cursor { observed, maximum },
        })
    }

    fn operation_unavailable(operation: OperationKey) -> Diagnostic {
        Diagnostic {
            code: DiagnosticCode::OperationUnavailable,
            detail: DiagnosticDetail::Operation(operation),
        }
    }

    fn rejected(correlation: crate::CorrelationId, diagnostic: Diagnostic) -> ApplicationReply {
        ApplicationReply {
            correlation,
            body: ReplyBody::Rejected,
            terminal: Terminal::Failed,
            diagnostic: Some(diagnostic),
        }
    }
}

impl Default for ApplicationService {
    fn default() -> Self {
        Self::new()
    }
}
