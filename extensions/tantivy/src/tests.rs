//! Adversarial lexical extension tests.

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

struct SubstitutingSource {
    page: LexicalPage,
}

impl LexicalSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &QueryRequest) -> Result<LexicalPage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let mut wrong = binding;
    wrong.root = RelationState::<IndexRelation>::from_entries(
        [(1, vec![("body".into(), "other".into())])],
        coverage,
    )
    .expect("wrong root")
    .root();
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        query: query.version,
        ids: vec![1],
        next: None,
        coverage,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = QueryRequest {
        binding,
        query,
        cursor: None,
        limit: 1,
    };
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));

    let incomplete = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: request.query.version,
        ids: vec![1],
        next: None,
        coverage: incomplete_coverage(7, Coverage::Partial),
    };
    let adapter =
        Adapter::new(SubstitutingSource { page: incomplete }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::IncompleteCoverage))
    ));
}

#[test]
fn provider_page_cannot_exceed_requested_bound() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let page = LexicalPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: query.version,
        ids: vec![1, 2],
        next: None,
        coverage,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = QueryRequest {
        binding,
        query,
        cursor: None,
        limit: 1,
    };
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::SizeLimit))
    ));
}

fn binding(documents: &[(u64, Vec<(String, String)>)]) -> (Binding, CoverageWitness) {
    let coverage = complete_coverage(ScopeRoot::from_bytes([7; 32])).expect("coverage");
    let state = RelationState::<IndexRelation>::from_entries(documents.iter().cloned(), coverage)
        .expect("state");
    (
        Binding::new(
            workspace(),
            state.root(),
            Recipe::from_value(&[1; 32]),
            Authority::from_value(&[2; 32]),
            ReadManifest::from_value(b"reads".as_slice()),
        ),
        coverage,
    )
}

#[test]
fn stale_root_is_rejected() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let materialized = materialize(
        binding.root,
        binding.recipe,
        binding.authority,
        coverage,
        vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "alpha".into())],
        }],
    )
    .expect("materialization");
    let stale = RelationState::<IndexRelation>::from_entries(
        [(1, vec![("body".into(), "beta".into())])],
        coverage,
    )
    .expect("stale state")
    .root();
    assert_eq!(
        query(&materialized, stale, &["alpha"]),
        Err(Error::StaleRoot)
    );
}

#[test]
fn delete_and_readd_preserve_exact_delta_roots() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(1, vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let deleted = state
        .prepare_delta(vec![DocumentChange::Delete { id: 1 }])
        .expect("delete");
    let empty = state.apply_delta(&deleted).expect("apply delete");
    let added = empty
        .prepare_delta(vec![DocumentChange::Add {
            id: 1,
            fields: vec![("body".into(), "beta".into())],
        }])
        .expect("readd");
    let restored = empty.apply_delta(&added).expect("apply readd");
    assert_eq!(restored.iter().count(), 1);
    assert_ne!(deleted.delta.id(), added.delta.id());
    assert_ne!(deleted.binding.root, added.binding.root);
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let (binding, coverage) = binding(&[(1, vec![("body".into(), "alpha".into())])]);
    let state = DocumentState::new(
        binding,
        coverage,
        vec![(1, vec![("body".into(), "alpha".into())])],
        Limits::default(),
    )
    .expect("state");
    let mut delta = state
        .prepare_delta(vec![DocumentChange::Delete { id: 1 }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_field_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_text_bytes: 6,
        ..Limits::default()
    };
    let state = DocumentState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            DocumentChange::Add {
                id: 1,
                fields: vec![("a".into(), "abc".into())],
            },
            DocumentChange::Add {
                id: 2,
                fields: vec![("b".into(), "abc".into())],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn cursor_is_bound_to_root_and_terms() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(vec!["alpha".into()], Limits::default()).expect("query");
    let source = MemorySource::new(
        binding,
        query.version,
        coverage,
        vec![3, 1, 2],
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
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 2,
        })
        .expect("first page");
    assert_eq!(first.ids, vec![1, 2]);
    let cursor = first.next.expect("next cursor");
    let other_query = Query::new(vec!["beta".into()], Limits::default()).expect("query");
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding,
            query: other_query,
            cursor: Some(cursor),
            limit: 1,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_and_oversized_inputs_are_rejected() {
    assert_eq!(
        Query::new(vec![String::new()], Limits::default()),
        Err(Error::MalformedInput)
    );
    assert_eq!(
        Limits {
            max_page: 0,
            ..Limits::default()
        }
        .validate(),
        Err(Error::InvalidLimits)
    );
    let (binding, coverage) = binding(&[]);
    assert_eq!(
        materialize_with_limits(
            binding.root,
            binding.recipe,
            binding.authority,
            incomplete_coverage(1, Coverage::Partial),
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
    assert_eq!(
        DocumentState::new(
            binding,
            coverage,
            vec![(1, vec![("body".into(), "x".into()); 2])],
            Limits {
                max_fields_per_document: 1,
                ..Limits::default()
            },
        ),
        Err(Error::SizeLimit)
    );
}
