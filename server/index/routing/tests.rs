use super::*;

use server_index_core::{EntityDocumentId, IndexSnapshot, IndexSnapshotId};
use server_index_vocabulary::LexicalSegmentId;

use compiler_ir::EntityId;
use heart_identity::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};

fn snapshot_id(value: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[value])
}

fn index_snapshot(
    segments: &[LexicalSegmentId],
) -> Result<IndexSnapshot<'_>, server_index_core::IndexSnapshotError> {
    IndexSnapshot::new(GenerationId::from_digest([9; 32]), &[], segments)
}

fn document(value: u8) -> EntityDocumentId {
    EntityDocumentId {
        artifact: server_index_core::EntityArtifactIdentity::Compact(ArtifactId::<
            IrFragmentEncoding,
            IrFragmentDomain,
        >::from_digest(
            [value; 32]
        )),
        entity: EntityId::new(u32::from(value)),
    }
}

fn segment(value: u8) -> LexicalSegmentId {
    LexicalSegmentId::from_canonical_bytes(&[value])
}

fn hit(segment: LexicalSegmentId, document_value: u8, score: u32) -> ObservedHit<'static> {
    ObservedHit {
        segment,
        term: b"term",
        document: document(document_value),
        score,
    }
}

struct MissingBackend(WorkerId);

impl WorkerBackend for MissingBackend {
    fn execute<'worker>(
        &'worker self,
        request: WorkerRequest<'_>,
    ) -> WorkerResponse<'worker, 'worker> {
        WorkerResponse::Missing {
            attempt: request.attempt,
        }
    }
}

impl IdentifiedWorker for MissingBackend {
    fn id(&self) -> WorkerId {
        self.0
    }
}

struct WrongAttemptBackend(WorkerId);

impl WorkerBackend for WrongAttemptBackend {
    fn execute<'worker>(
        &'worker self,
        request: WorkerRequest<'_>,
    ) -> WorkerResponse<'worker, 'worker> {
        WorkerResponse::Stale {
            attempt: RouteAttempt {
                ordinal: AttemptOrdinal::new(99),
                ..request.attempt
            },
            observed: None,
        }
    }
}

impl IdentifiedWorker for WrongAttemptBackend {
    fn id(&self) -> WorkerId {
        self.0
    }
}

struct ModeBackend<'hits> {
    id: WorkerId,
    missing: bool,
    hits: &'hits [ObservedHit<'hits>],
}

impl WorkerBackend for ModeBackend<'_> {
    fn execute<'worker>(
        &'worker self,
        request: WorkerRequest<'_>,
    ) -> WorkerResponse<'worker, 'worker> {
        if self.missing {
            WorkerResponse::Missing {
                attempt: request.attempt,
            }
        } else {
            WorkerResponse::Reply(WorkerReply::new(request.attempt, self.hits))
        }
    }
}

impl IdentifiedWorker for ModeBackend<'_> {
    fn id(&self) -> WorkerId {
        self.id
    }
}

fn retry(attempts: u8) -> Option<RetryPolicy> {
    RetryPolicy::new(attempts).ok()
}

fn assignment(
    plan: &RoutePlan<'_>,
    ordinal: usize,
    _segment: LexicalSegmentId,
) -> Option<SegmentAssignment> {
    plan.assignment(ordinal)
}

fn independent_reply<'rows, 'bytes>(
    attempt: RouteAttempt,
    rows: &'rows [ObservedHit<'bytes>],
) -> WorkerReply<'rows, 'bytes> {
    WorkerReply::new(attempt, rows)
}

fn plan(segments: &[LexicalSegmentId]) -> Option<RoutePlan<'_>> {
    let workers = [WorkerId::new(1), WorkerId::new(2)];
    let retry = retry(2)?;
    let snapshot = index_snapshot(segments).ok()?;
    let coordinator = Coordinator::new(snapshot, &workers, retry).ok()?;
    coordinator
        .plan(Query::new(b"q", OrderingRecipe::new(1)))
        .ok()
}

#[test]
fn rendezvous_assignments_are_permutation_stable() {
    let segments = [segment(1), segment(2), segment(3)];
    let workers_a = [WorkerId::new(1), WorkerId::new(2), WorkerId::new(3)];
    let workers_b = [WorkerId::new(3), WorkerId::new(1), WorkerId::new(2)];
    let Some(retry) = retry(2) else {
        return;
    };
    let Ok(left_snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Ok(right_snapshot) = index_snapshot(&segments) else {
        return;
    };
    let left = Coordinator::new(left_snapshot, &workers_a, retry)
        .ok()
        .and_then(|coordinator| {
            coordinator
                .plan(Query::new(b"q", OrderingRecipe::new(1)))
                .ok()
        });
    let right = Coordinator::new(right_snapshot, &workers_b, retry)
        .ok()
        .and_then(|coordinator| {
            coordinator
                .plan(Query::new(b"q", OrderingRecipe::new(1)))
                .ok()
        });
    for ordinal in 0..segments.len() {
        assert_eq!(
            left.and_then(|plan| plan.assignment(ordinal)),
            right.and_then(|plan| plan.assignment(ordinal))
        );
    }
}

#[test]
fn merge_is_reply_order_independent_and_deduplicates() {
    let segments = [segment(1), segment(2)];
    let Some(plan) = plan(&segments) else {
        return;
    };
    let Some(first) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let Some(second) = assignment(&plan, 1, segments[1]) else {
        return;
    };
    let Some(first_worker) = first.candidate(0) else {
        return;
    };
    let Some(second_worker) = second.candidate(0) else {
        return;
    };
    let left_hit = [hit(first.segment, 1, 4)];
    let right_hit = [hit(second.segment, 1, 9), hit(second.segment, 2, 8)];
    let reply_a = WorkerReply::new(
        RouteAttempt {
            route: plan.route(),
            snapshot: plan.snapshot(),
            query: plan.query(),
            segment_ordinal: first.ordinal,
            segment: first.segment,
            range: first.range,
            worker: first_worker,
            ordinal: AttemptOrdinal::new(0),
        },
        &left_hit,
    );
    let reply_b = WorkerReply::new(
        RouteAttempt {
            route: plan.route(),
            snapshot: plan.snapshot(),
            query: plan.query(),
            segment_ordinal: second.ordinal,
            segment: second.segment,
            range: second.range,
            worker: second_worker,
            ordinal: AttemptOrdinal::new(0),
        },
        &right_hit,
    );
    let mut seen = [None; 8];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 2];
    let mut output = [RoutedHitSlot::vacant(); 2];
    let first_order = merge_replies(
        &plan,
        &[reply_a, reply_b],
        &mut seen,
        &mut missing,
        &mut output,
    )
    .map(|outcome| {
        outcome
            .hits
            .iter()
            .filter_map(|row| row.get().map(MergedHit::document))
            .collect::<Vec<_>>()
    });
    let second_order = merge_replies(
        &plan,
        &[reply_b, reply_a],
        &mut seen,
        &mut missing,
        &mut output,
    )
    .map(|outcome| {
        outcome
            .hits
            .iter()
            .filter_map(|row| row.get().map(MergedHit::document))
            .collect::<Vec<_>>()
    });
    assert_eq!(first_order, second_order);
    assert_eq!(first_order.map(|rows| rows.len()), Ok(2));
}

#[test]
fn zero_top_k_accepts_rows_without_scratch_or_output() {
    let segments = [segment(1)];
    let workers = [WorkerId::new(1)];
    let Some(retry) = retry(1) else {
        return;
    };
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(zero) = TopK::new(0) else {
        return;
    };
    let query = Query::new(b"q", OrderingRecipe::new(1)).with_top_k(zero);
    let Ok(plan) = coordinator.plan(query) else {
        return;
    };
    let Some(assignment) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let rows = [hit(assignment.segment, 1, 9)];
    let Some(worker) = assignment.candidate(0) else {
        return;
    };
    let reply = WorkerReply::new(
        RouteAttempt {
            route: plan.route(),
            snapshot: plan.snapshot(),
            query: plan.query(),
            segment_ordinal: assignment.ordinal,
            segment: assignment.segment,
            range: assignment.range,
            worker,
            ordinal: AttemptOrdinal::new(0),
        },
        &rows,
    );
    let mut seen: [Option<EntityDocumentId>; 0] = [];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [];
    assert_eq!(
        merge_replies(&plan, &[reply], &mut seen, &mut missing, &mut output),
        Err(MergeError::Reply(ReplyError::HitCount))
    );
    let empty_reply = WorkerReply::new(
        RouteAttempt {
            route: plan.route(),
            snapshot: plan.snapshot(),
            query: plan.query(),
            segment_ordinal: assignment.ordinal,
            segment: assignment.segment,
            range: assignment.range,
            worker,
            ordinal: AttemptOrdinal::new(0),
        },
        &[],
    );
    let result = merge_replies(&plan, &[empty_reply], &mut seen, &mut missing, &mut output);
    assert_eq!(result.map(|outcome| outcome.hits.len()), Ok(0));
}

#[test]
fn reply_rows_borrow_is_independent_from_term_bytes() {
    let segments = [segment(1)];
    let Some(plan) = plan(&segments) else {
        return;
    };
    let Some(assignment) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let term = *b"term";
    let observed = [ObservedHit {
        segment: assignment.segment,
        term: &term,
        document: document(3),
        score: 4,
    }];
    let Some(worker) = assignment.candidate(0) else {
        return;
    };
    let attempt = RouteAttempt {
        route: plan.route(),
        snapshot: plan.snapshot(),
        query: plan.query(),
        segment_ordinal: assignment.ordinal,
        segment: assignment.segment,
        range: assignment.range,
        worker,
        ordinal: AttemptOrdinal::new(0),
    };
    {
        let rows = observed;
        let reply = independent_reply(attempt, &rows);
        let mut seen = [None; 1];
        let mut missing = [MissingAssignment {
            ordinal: SegmentOrdinal(0),
            segment: segment(0),
            range: SegmentRange::whole(),
        }; 1];
        let mut output = [RoutedHitSlot::vacant(); 1];
        assert!(merge_replies(&plan, &[reply], &mut seen, &mut missing, &mut output).is_ok());
    }
    assert_eq!(term[0], b't');
}

#[test]
fn stale_worker_is_bounded_and_produces_exact_missing_segment() {
    let first_segment = segment(1);
    let second_segment = segment(2);
    let third_segment = segment(3);
    let segments = [first_segment, second_segment, third_segment];
    let workers = [WorkerId::new(1)];
    let Some(retry) = retry(2) else {
        return;
    };
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let empty: [ObservedHit<'static>; 0] = [];
    let backend = [InMemoryWorker::new(workers[0], &empty).stale(true)];
    let cancellation = Cancellation::new();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 3];
    let mut output = [RoutedHitSlot::vacant(); 1];
    let result = coordinator.execute(
        plan,
        Query::new(b"q", OrderingRecipe::new(1)),
        &backend,
        &cancellation,
        &mut seen,
        &mut missing,
        &mut output,
    );
    assert_eq!(
        result.map(|outcome| outcome
            .missing
            .iter()
            .map(|item| (item.ordinal, item.segment, item.range))
            .collect::<Vec<_>>()),
        Ok(vec![
            (SegmentOrdinal(0), first_segment, SegmentRange::whole()),
            (SegmentOrdinal(1), second_segment, SegmentRange::whole()),
            (SegmentOrdinal(2), third_segment, SegmentRange::whole()),
        ])
    );
}

#[test]
fn stale_primary_retries_to_second_rendezvous_worker() {
    let segments = [segment(1)];
    let workers = [WorkerId::new(1), WorkerId::new(2)];
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Some(retry) = retry(2) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let Some(assignment) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let Some(primary) = assignment.candidate(0) else {
        return;
    };
    let Some(secondary) = assignment.candidate(1) else {
        return;
    };
    let rows = [hit(segments[0], 7, 12)];
    let backends = [
        InMemoryWorker::new(primary, &rows).stale(true),
        InMemoryWorker::new(secondary, &rows),
    ];
    let cancellation = Cancellation::new();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [RoutedHitSlot::vacant(); 1];
    let result = coordinator.execute(
        plan,
        Query::new(b"q", OrderingRecipe::new(1)),
        &backends,
        &cancellation,
        &mut seen,
        &mut missing,
        &mut output,
    );
    assert_eq!(result.map(|outcome| outcome.hits.len()), Ok(1));
    assert_eq!(result.map(|outcome| outcome.missing.len()), Ok(0));
}

#[test]
fn missing_primary_retries_to_second_rendezvous_worker() {
    let segments = [segment(1)];
    let workers = [WorkerId::new(1), WorkerId::new(2)];
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Some(retry) = retry(2) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let Some(assignment) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let Some(primary) = assignment.candidate(0) else {
        return;
    };
    let Some(secondary) = assignment.candidate(1) else {
        return;
    };
    let rows = [hit(segments[0], 8, 13)];
    let backends = [
        ModeBackend {
            id: primary,
            missing: true,
            hits: &rows,
        },
        ModeBackend {
            id: secondary,
            missing: false,
            hits: &rows,
        },
    ];
    let cancellation = Cancellation::new();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [RoutedHitSlot::vacant(); 1];
    let result = coordinator.execute(
        plan,
        Query::new(b"q", OrderingRecipe::new(1)),
        &backends,
        &cancellation,
        &mut seen,
        &mut missing,
        &mut output,
    );
    assert_eq!(result.map(|outcome| outcome.hits.len()), Ok(1));
    assert_eq!(result.map(|outcome| outcome.missing.len()), Ok(0));
}

#[test]
fn wrong_terminal_attempt_and_top_k_are_typed_failures() {
    let segments = [segment(1)];
    let workers = [WorkerId::new(1)];
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Some(retry) = retry(1) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let backend = [WrongAttemptBackend(workers[0])];
    let cancellation = Cancellation::new();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [RoutedHitSlot::vacant(); 1];
    assert!(matches!(
        coordinator.execute(
            plan,
            Query::new(b"q", OrderingRecipe::new(1)),
            &backend,
            &cancellation,
            &mut seen,
            &mut missing,
            &mut output,
        ),
        Err(ExecuteError::Merge(MergeError::Reply(
            ReplyError::ResponseAttempt
        )))
    ));
    let Ok(zero_top_k) = TopK::new(0) else {
        return;
    };
    let zero_query = Query::new(b"q", OrderingRecipe::new(1)).with_top_k(zero_top_k);
    let Ok(zero_plan) = coordinator.plan(zero_query) else {
        return;
    };
    assert_eq!(
        coordinator.execute(
            zero_plan,
            Query::new(b"q", OrderingRecipe::new(1)),
            &backend,
            &cancellation,
            &mut seen,
            &mut missing,
            &mut output,
        ),
        Err(ExecuteError::PlanMismatch)
    );
}

#[test]
#[allow(clippy::indexing_slicing, reason = "fixed three-segment test fixture")]
fn explicit_missing_and_down_worker_preserve_selection_order() {
    let segments = [segment(1), segment(2), segment(3)];
    let workers = [WorkerId::new(1)];
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Some(retry) = retry(2) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let backend = [MissingBackend(workers[0])];
    let cancellation = Cancellation::new();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 3];
    let mut output = [RoutedHitSlot::vacant(); 1];
    let result = coordinator.execute(
        plan,
        Query::new(b"q", OrderingRecipe::new(1)),
        &backend,
        &cancellation,
        &mut seen,
        &mut missing,
        &mut output,
    );
    assert_eq!(
        result.map(|outcome| outcome
            .missing
            .iter()
            .map(|item| item.segment)
            .collect::<Vec<_>>()),
        Ok(segments.to_vec())
    );
}

#[test]
fn cancellation_stops_before_dispatch() {
    let segments = [segment(1)];
    let workers = [WorkerId::new(1)];
    let Some(retry) = retry(1) else {
        return;
    };
    let Ok(snapshot) = index_snapshot(&segments) else {
        return;
    };
    let Ok(coordinator) = Coordinator::new(snapshot, &workers, retry) else {
        return;
    };
    let Ok(plan) = coordinator.plan(Query::new(b"q", OrderingRecipe::new(1))) else {
        return;
    };
    let empty: [ObservedHit<'static>; 0] = [];
    let backend = [InMemoryWorker::new(workers[0], &empty)];
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [RoutedHitSlot::vacant(); 1];
    assert_eq!(
        coordinator.execute(
            plan,
            Query::new(b"q", OrderingRecipe::new(1)),
            &backend,
            &cancellation,
            &mut seen,
            &mut missing,
            &mut output
        ),
        Err(ExecuteError::Cancelled)
    );
}

#[test]
fn wrong_snapshot_and_duplicate_replies_are_typed_failures() {
    let segments = [segment(1)];
    let Some(plan) = plan(&segments) else {
        return;
    };
    let Some(assignment) = assignment(&plan, 0, segments[0]) else {
        return;
    };
    let Some(worker) = assignment.candidate(0) else {
        return;
    };
    let rows = [hit(segments[0], 1, 1)];
    let attempt = RouteAttempt {
        route: plan.route(),
        snapshot: plan.snapshot(),
        query: plan.query(),
        segment_ordinal: assignment.ordinal,
        segment: assignment.segment,
        range: assignment.range,
        worker,
        ordinal: AttemptOrdinal::new(0),
    };
    let wrong = WorkerReply::new(
        RouteAttempt {
            snapshot: snapshot_id(99),
            ..attempt
        },
        &rows,
    );
    let mut seen = [None; 1];
    let mut missing = [MissingAssignment {
        ordinal: SegmentOrdinal(0),
        segment: segment(0),
        range: SegmentRange::whole(),
    }; 1];
    let mut output = [RoutedHitSlot::vacant(); 1];
    assert!(matches!(
        merge_replies(&plan, &[wrong], &mut seen, &mut missing, &mut output),
        Err(MergeError::Reply(ReplyError::AttemptSnapshot))
    ));
    let valid = WorkerReply::new(attempt, &rows);
    assert!(matches!(
        merge_replies(&plan, &[valid, valid], &mut seen, &mut missing, &mut output),
        Err(MergeError::DuplicateReply { .. })
    ));
    let duplicate_rows = [hit(segments[0], 1, 2), hit(segments[0], 1, 1)];
    let duplicate_documents = WorkerReply::new(attempt, &duplicate_rows);
    assert_eq!(
        merge_replies(
            &plan,
            &[duplicate_documents],
            &mut seen,
            &mut missing,
            &mut output
        ),
        Err(MergeError::Reply(ReplyError::DuplicateDocument))
    );

    let wrong_worker = WorkerReply::new(
        RouteAttempt {
            worker: WorkerId::new(999),
            ..attempt
        },
        &rows,
    );
    assert!(matches!(
        merge_replies(&plan, &[wrong_worker], &mut seen, &mut missing, &mut output),
        Err(MergeError::Reply(ReplyError::Worker { .. }))
    ));
    let wrong_query = WorkerReply::new(
        RouteAttempt {
            query: QueryDigest::from_bytes(b"other"),
            ..attempt
        },
        &rows,
    );
    assert!(matches!(
        merge_replies(&plan, &[wrong_query], &mut seen, &mut missing, &mut output),
        Err(MergeError::Reply(ReplyError::AttemptQuery))
    ));
    let wrong_assignment = WorkerReply::new(
        RouteAttempt {
            segment_ordinal: SegmentOrdinal(7),
            ..attempt
        },
        &rows,
    );
    assert!(matches!(
        merge_replies(
            &plan,
            &[wrong_assignment],
            &mut seen,
            &mut missing,
            &mut output
        ),
        Err(MergeError::Reply(ReplyError::Assignment))
    ));
}
