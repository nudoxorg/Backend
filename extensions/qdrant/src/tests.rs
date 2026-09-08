//! Adversarial vector extension tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_version::{
    Coverage, CoverageWitness, RelationState, ScopeRoot, WorkspaceManifest, WorkspaceRoot,
};

fn workspace() -> WorkspaceRoot {
    WorkspaceManifest::new(
        1,
        Vec::new(),
        Vec::new(),
        [1; 32],
        complete_coverage(ScopeRoot::from_bytes([3; 32])).expect("complete scope"),
    )
    .expect("valid workspace")
    .root()
}

fn binding(ids: &[(u64, Vec<u8>)]) -> (Binding, CoverageWitness) {
    let coverage = complete_coverage(ScopeRoot::from_bytes([7; 32])).expect("coverage");
    let state = RelationState::<CandidateRelation>::from_entries(ids.iter().cloned(), coverage)
        .expect("state");
    let recipe = Recipe::from_value(&[1; 32]);
    let authority = Authority::from_value(&[2; 32]);
    let read_manifest = ReadManifest::from_value(b"reads".as_slice());
    (
        Binding::new(workspace(), state.root(), recipe, authority, read_manifest),
        coverage,
    )
}

#[test]
fn stale_root_is_rejected_before_rerank() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let accepted = accept_remote(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        Tombstones(Vec::new()),
        vec![CandidateId(1)],
    )
    .expect("accepted");
    let other = RelationState::<CandidateRelation>::from_entries([(1, vec![2])], coverage)
        .expect("other state")
        .root();
    assert_eq!(rerank(&accepted, other, |_| 1), Err(Error::StaleRoot));
}

#[test]
fn delete_and_readd_are_distinct_exact_transitions() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), vec![1])],
        Limits::default(),
    )
    .expect("state");
    let deletion = state
        .prepare_delta(vec![CandidateChange::Delete { id: CandidateId(1) }])
        .expect("delete");
    let deleted = state.apply_delta(&deletion).expect("apply delete");
    assert_eq!(deleted.iter().count(), 0);
    let readd = deleted
        .prepare_delta(vec![CandidateChange::Upsert {
            id: CandidateId(1),
            payload: vec![9],
        }])
        .expect("readd");
    let restored = deleted.apply_delta(&readd).expect("apply readd");
    assert_eq!(
        restored.iter().collect::<Vec<_>>(),
        vec![(CandidateId(1), &[9][..])]
    );
    assert_ne!(deletion.delta.id(), readd.delta.id());
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let (binding, coverage) = binding(&[(1, vec![1])]);
    let state = CandidateState::new(
        binding,
        coverage,
        vec![(CandidateId(1), vec![1])],
        Limits::default(),
    )
    .expect("state");
    let mut delta = state
        .prepare_delta(vec![CandidateChange::Delete { id: CandidateId(1) }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_payload_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_payload_bytes: 2,
        ..Limits::default()
    };
    let state = CandidateState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            CandidateChange::Upsert {
                id: CandidateId(1),
                payload: vec![1, 2],
            },
            CandidateChange::Upsert {
                id: CandidateId(2),
                payload: vec![3],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn paging_rejects_stale_cursor_and_orders_ids() {
    let (binding, coverage) = binding(&[]);
    let source = MemorySource::new(
        binding,
        coverage,
        vec![CandidateId(3), CandidateId(1), CandidateId(2)],
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("source");
    let adapter = Adapter::new(
        source,
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("adapter");
    let first = adapter
        .search(SearchRequest::first(binding, 2))
        .expect("first page");
    assert_eq!(first.ids, vec![CandidateId(1), CandidateId(2)]);
    let cursor = first.next.expect("next cursor");
    let mut stale = binding;
    stale.root = RelationState::<CandidateRelation>::from_entries([(1, vec![4])], coverage)
        .expect("stale state")
        .root();
    assert!(matches!(
        adapter.search(SearchRequest {
            binding: stale,
            cursor: Some(cursor),
            limit: 2,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_and_oversized_inputs_are_rejected() {
    assert_eq!(CandidateId::new(0), Err(Error::MalformedInput));
    assert_eq!(
        Limits::default().validate().expect("valid limits").max_page,
        256
    );
    assert_eq!(
        Tombstones::new(
            vec![CandidateId(1), CandidateId(1)],
            Limits {
                max_tombstones: 0,
                ..Limits::default()
            },
        ),
        Err(Error::InvalidLimits)
    );
    let coverage = incomplete_coverage(1, Coverage::Partial);
    let (binding, _) = binding(&[]);
    assert_eq!(
        MemorySource::new(binding, coverage, Vec::new(), Limits::default()),
        Err(Error::IncompleteCoverage)
    );
}

struct SubstitutingSource {
    page: CandidatePage,
}

impl CandidateSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &SearchRequest) -> Result<CandidatePage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let mut wrong = binding;
    wrong.root = RelationState::<CandidateRelation>::from_entries([(1, vec![9])], coverage)
        .expect("wrong root")
        .root();
    let page = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        ids: vec![CandidateId(1)],
        next: None,
        coverage,
        quality: SearchQuality::Exact,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = SearchRequest::first(binding, 1);
    assert!(matches!(
        adapter.search(request),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));

    let incomplete = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding,
        ids: vec![CandidateId(1)],
        next: None,
        coverage: incomplete_coverage(7, Coverage::Partial),
        quality: SearchQuality::Exact,
    };
    let adapter =
        Adapter::new(SubstitutingSource { page: incomplete }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.search(request),
        Err(AdapterError::Extension(Error::IncompleteCoverage))
    ));
}

#[test]
fn approximate_results_keep_recipe_model_recall_and_never_become_exact() {
    let (binding, coverage) = binding(&[]);
    let recall = RecallMetadata::new(900_000, Some(875_000)).expect("recall");
    let metadata =
        ApproximationMetadata::new(binding.recipe, ModelVersion::from_value(&[4; 32]), recall);
    let candidates = accept_remote_approximate(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        Tombstones(Vec::new()),
        vec![CandidateId(1), CandidateId(2)],
        metadata,
    )
    .expect("approximate candidates");
    assert_eq!(candidates.quality, SearchQuality::Approximate(metadata));
    assert!(!candidates.quality.is_exact());
    let reranked = rerank(&candidates, binding.root, |id| id.0).expect("rerank");
    assert_eq!(reranked.coverage, coverage);
    assert_eq!(reranked.quality, candidates.quality);
    assert!(!reranked.quality.is_exact());

    let wrong = ApproximationMetadata::new(
        Recipe::from_value(&[9; 32]),
        ModelVersion::from_value(&[4; 32]),
        recall,
    );
    assert_eq!(
        accept_remote_approximate(
            binding.root,
            binding.recipe,
            binding.authority,
            coverage,
            Tombstones(Vec::new()),
            vec![CandidateId(1)],
            wrong,
        ),
        Err(Error::ApproximationMismatch)
    );
    let forged = Candidates {
        quality: SearchQuality::Approximate(wrong),
        ..candidates
    };
    assert_eq!(
        rerank(&forged, binding.root, |_| 1),
        Err(Error::ApproximationMismatch)
    );
}

#[test]
fn provider_page_cannot_exceed_requested_bound() {
    let (binding, coverage) = binding(&[]);
    let page = CandidatePage {
        schema: SchemaVersion::CURRENT,
        binding,
        ids: vec![CandidateId(1), CandidateId(2)],
        next: None,
        coverage,
        quality: SearchQuality::Exact,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.search(SearchRequest::first(binding, 1)),
        Err(AdapterError::Extension(Error::SizeLimit))
    ));
}
