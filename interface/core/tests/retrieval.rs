//! Exercises the typed retrieval seam with a test-only adapter over `server-index-core`.
//!
//! The adapter is intentionally local to this integration test: production `interface-core`
//! owns only the capability contract, not a fixture catalogue or a server-index dependency.

use compiler_ir::EntityId;
use heart_identity::{ArtifactId, IrFragmentDomain, IrFragmentEncoding};
use interface_core::{
    ApplicationInput, ApplicationOutcome, ApplicationService, Capability, CorrelationId,
    DiagnosticCode, DiagnosticDetail, DocSection, InputText, RetrievalCapability, RetrievalCause,
    RetrievalMode, RetrievalPhase, RetrievalQueryCause, RetrievalReadiness, RetrievalRequest,
    RetrievalRow, RetrievalRows, SnapshotFacts, UnavailableCompiler, UnloadReceipt,
};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, ExactOperation, ExactRow, ExactSegment, LexicalHit,
    LexicalOperation, LexicalRow, LexicalScore, LexicalSegment, LexicalTopK,
};

fn text(value: &str) -> InputText {
    match InputText::try_from_str(value) {
        Ok(value) => value,
        Err(error) => panic!(
            "test literal unexpectedly exceeds {}: {}",
            error.maximum, error.actual
        ),
    }
}

fn document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"interface-core-retrieval-test",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

/// A private adapter that consumes exact and deterministic-prefix lexical server-index rows.
struct IndexAdapter {
    snapshot: InputText,
    resident: bool,
}

impl IndexAdapter {
    fn ready(snapshot: InputText) -> Self {
        Self {
            snapshot,
            resident: true,
        }
    }

    fn exact_segment() -> Result<ExactSegment<'static>, RetrievalCause> {
        static ROWS: [ExactRow<'static>; 1] = [ExactRow::present(b"alpha", b"exact-alpha")];
        ExactSegment::new(&ROWS).map_err(|_| RetrievalCause::Backend {
            phase: RetrievalPhase::Scan,
        })
    }

    fn lexical_rows() -> [LexicalRow<'static>; 2] {
        [
            LexicalRow::new(b"alpha", document(1), LexicalScore::from(7)),
            LexicalRow::new(b"alphabet", document(2), LexicalScore::from(5)),
        ]
    }

    fn row(document: InputText, term: InputText, score: u32, mode: RetrievalMode) -> RetrievalRow {
        RetrievalRow {
            document,
            term,
            span: None,
            score,
            mode,
            signature: None,
            section: DocSection::Members,
        }
    }
}

impl RetrievalCapability for IndexAdapter {
    fn readiness(&self) -> RetrievalReadiness {
        RetrievalReadiness::Ready
    }

    fn snapshot_status(&self, snapshot: &InputText) -> Result<SnapshotFacts, RetrievalCause> {
        Ok(SnapshotFacts {
            snapshot: *snapshot,
            resident: self.resident && *snapshot == self.snapshot,
        })
    }

    fn search(&self, request: RetrievalRequest<'_>) -> Result<RetrievalRows, RetrievalCause> {
        if !self.resident || *request.snapshot != self.snapshot {
            return Err(RetrievalCause::SnapshotUnknown {
                snapshot: *request.snapshot,
            });
        }
        if request.query.is_empty() {
            return Err(RetrievalCause::QueryRejected {
                reason: RetrievalQueryCause::Empty,
            });
        }
        let exact = Self::exact_segment()?;
        if let Some(row) = exact.lookup(ExactOperation::new(request.query.as_ref())) {
            let value = row.value_bytes().ok_or(RetrievalCause::Backend {
                phase: RetrievalPhase::Scan,
            })?;
            let value = core::str::from_utf8(value).map_err(|_| RetrievalCause::Backend {
                phase: RetrievalPhase::Scan,
            })?;
            return RetrievalRows::new()
                .push(Self::row(
                    text(value),
                    *request.query,
                    0,
                    RetrievalMode::Exact,
                ))
                .map_err(|rejected| RetrievalCause::RowTableFull {
                    rejected: Box::new(rejected),
                });
        }

        let rows = Self::lexical_rows();
        let lexical = LexicalSegment::new(&rows).map_err(|_| RetrievalCause::Backend {
            phase: RetrievalPhase::Scan,
        })?;
        let top_k =
            LexicalTopK::new(usize::from(request.limit)).map_err(|_| RetrievalCause::Backend {
                phase: RetrievalPhase::Rank,
            })?;
        let placeholder = LexicalHit::new(b"placeholder", document(99), LexicalScore::from(0));
        let mut hits = [placeholder; 4];
        let count = lexical
            .rank(
                LexicalOperation::prefix(request.query.as_ref()),
                top_k,
                &mut hits,
            )
            .map_err(|_| RetrievalCause::Backend {
                phase: RetrievalPhase::Rank,
            })?;
        let mut result = RetrievalRows::new();
        for hit in hits.into_iter().take(count) {
            let term = match hit.term {
                b"alpha" => text("alpha"),
                b"alphabet" => text("alphabet"),
                _ => {
                    return Err(RetrievalCause::Backend {
                        phase: RetrievalPhase::Rank,
                    });
                }
            };
            let document = match hit.document.entity.raw {
                1 => text("alpha-document"),
                2 => text("alphabet-document"),
                _ => {
                    return Err(RetrievalCause::Backend {
                        phase: RetrievalPhase::Rank,
                    });
                }
            };
            result = result
                .push(Self::row(
                    document,
                    term,
                    u32::from(hit.score),
                    RetrievalMode::Lexical,
                ))
                .map_err(|rejected| RetrievalCause::RowTableFull {
                    rejected: Box::new(rejected),
                })?;
        }
        Ok(result)
    }

    fn unload(&mut self, snapshot: &InputText) -> Result<UnloadReceipt, RetrievalCause> {
        let removed = self.resident && *snapshot == self.snapshot;
        self.resident &= !removed;
        Ok(UnloadReceipt {
            snapshot: *snapshot,
            removed,
        })
    }
}

#[test]
fn unavailable_default_never_fabricates_index_facts() {
    let snapshot = text("fixture");
    let mut service = ApplicationService::new();
    let reply = service.execute(&ApplicationInput::SnapshotStatus {
        correlation: CorrelationId(1),
        snapshot,
    });
    assert!(matches!(
        reply.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::DependencyUnavailable {
            capability: Capability::Index
        })
    ));
}

#[test]
fn test_only_index_adapter_preserves_exact_prefix_and_idempotent_unload_facts() {
    let snapshot = text("fixture");
    let mut service =
        ApplicationService::with_capabilities(UnavailableCompiler, IndexAdapter::ready(snapshot));

    let status = service.execute(&ApplicationInput::SnapshotStatus {
        correlation: CorrelationId(1),
        snapshot,
    });
    assert!(matches!(
        status.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::Snapshot(facts))
            if facts.snapshot == snapshot && facts.resident
    ));

    let exact = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(2),
        snapshot,
        query: text("alpha"),
        limit: 2,
    });
    assert!(matches!(
        exact.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::Retrieval(rows))
            if rows.len() == 1 && rows.iter().next().is_some_and(|row| row.mode == RetrievalMode::Exact)
    ));

    let prefix = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(3),
        snapshot,
        query: text("al"),
        limit: 2,
    });
    assert!(matches!(
        prefix.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::Retrieval(rows))
            if rows.len() == 2
                && rows.iter().all(|row| row.mode == RetrievalMode::Lexical)
                && rows.iter().next().is_some_and(|row| row.term == text("alpha"))
    ));

    let first = service.execute(&ApplicationInput::RemoveIndex {
        correlation: CorrelationId(4),
        snapshot,
    });
    assert!(matches!(
        first.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::IndexRemoved(receipt)) if receipt.removed
    ));
    let second = service.execute(&ApplicationInput::RemoveIndex {
        correlation: CorrelationId(5),
        snapshot,
    });
    assert!(matches!(
        second.outcome,
        ApplicationOutcome::Resolved(interface_core::ReplyBody::IndexRemoved(receipt)) if !receipt.removed
    ));
}

#[test]
fn retrieval_terminal_reaches_the_application_diagnostic_unchanged() {
    let snapshot = text("fixture");
    let mut service =
        ApplicationService::with_capabilities(UnavailableCompiler, IndexAdapter::ready(snapshot));
    let reply = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(6),
        snapshot,
        query: text(""),
        limit: 1,
    });
    assert!(matches!(
        reply.outcome,
        ApplicationOutcome::Failed {
            diagnostic: interface_core::Diagnostic {
                code: DiagnosticCode::RetrievalFailed,
                detail: DiagnosticDetail::Retrieval(RetrievalCause::QueryRejected {
                    reason: RetrievalQueryCause::Empty,
                }),
            },
        }
    ));
}
