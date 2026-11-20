//! Native allocation and authority controls for corpus admission.

use super::*;
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;

fn limits_for(facts: &[SemanticQueryFact]) -> Limits {
    Limits {
        max_rows: facts.len().max(Limits::default().max_rows),
        ..Limits::default()
    }
}

#[test]
fn corpus_admission_workspace_fixture_allocations_are_separate_input_work() {
    let measured = allocation_counter::measure(|| {
        std::hint::black_box(workspace());
    });
    eprintln!(
        "workspace input fixture allocations: {}",
        measured.count_total
    );
    assert!(
        measured.count_total > 0,
        "the input fixture must be constructed before measuring corpus admission"
    );
}

fn exact_admission_moves_buffer(facts: Vec<SemanticQueryFact>) {
    let selected_workspace = workspace();
    let limits = limits_for(&facts);
    let pointer = facts.as_ptr();
    let count = facts.len();
    let expected_order: Vec<_> = facts
        .iter()
        .map(|fact| fact.presentation().id.clone())
        .collect();
    let expected_digest =
        SemanticQueryCorpus::admit_with_limits(selected_workspace, facts.clone(), limits)
            .expect("legacy envelope")
            .evidence_digest();
    let retained = measure_semantic_query_facts_with_quota_controlled(
        &facts,
        facts.capacity(),
        limits,
        64,
        &|| false,
    )
    .expect("retained facts");
    let extra = semantic_query_corpus_admission_overhead(count, 64).expect("admission layout");
    let mut admitted = None;
    let measured = allocation_counter::measure(|| {
        admitted = Some(
            SemanticQueryCorpus::admit_with_budget_controlled(
                selected_workspace,
                facts,
                limits,
                SemanticQueryAdmissionBudget {
                    maximum_requested_bytes: retained + extra,
                    allocation_quota_per_allocation: 64,
                },
                &|| false,
            )
            .expect("exact requested-layout boundary"),
        );
    });
    let corpus = admitted.expect("corpus");
    assert_eq!(
        corpus.facts().as_ptr(),
        pointer,
        "fact owners moved without a second buffer"
    );
    assert_eq!(corpus.evidence_digest(), expected_digest);
    assert_eq!(
        corpus
            .facts()
            .iter()
            .map(|fact| &fact.presentation().id)
            .collect::<Vec<_>>(),
        expected_order.iter().collect::<Vec<_>>()
    );
    let owner_bytes = 2 * size_of::<AtomicUsize>() + size_of::<Vec<SemanticQueryFact>>();
    assert_eq!(measured.count_total, 1 + u64::from(count != 0));
    assert_eq!(
        measured.bytes_total,
        (count * size_of::<usize>() + owner_bytes) as u64
    );
    assert_eq!(measured.count_current, 1, "the admission index was retired");
    assert_eq!(measured.bytes_current, owner_bytes as i64);
    assert!(measured.bytes_max <= extra as u64);
    let cloned = allocation_counter::measure(|| {
        let shared = corpus.clone();
        assert_eq!(shared.facts().as_ptr(), pointer);
    });
    assert_eq!(cloned.count_total, 0);
}

fn sql_facts(count: usize, path: &str) -> Vec<SemanticQueryFact> {
    let mut facts = structural_facts(0);
    let SemanticQueryEvidence::Package(scope) = facts[0].evidence() else {
        unreachable!()
    };
    let package = scope.package();
    let project = facts[0].presentation().id.clone();
    for index in 0..count {
        let ordinal = if index + 1 == count {
            u32::MAX
        } else {
            index as u32
        };
        let coordinate = format!("sql-project::{path}::books{index}");
        let preimage = format!("{coordinate}\0sql:{ordinal}");
        let id =
            backend_library::RowId::Symbol(backend_library::symbol_key(&preimage)).stable_key();
        facts.push(SemanticQueryFact::new(
            SemanticQueryEvidence::SqlNative(SqlNativeQueryEvidence::new(
                package, [1; 32], [2; 32], [3; 32], [4; 32], [5; 32], [6; 32], [7; 32], [8; 32],
                ordinal, preimage,
            )),
            SemanticQueryPresentation {
                id,
                kind: "unknown".into(),
                coordinate,
                name: format!("books{index}"),
                signature: Some(format!("SQL table books{index}")),
                documentation: format!("SQL table in {path}"),
                score: None,
                project: Some(project.clone()),
                parent: None,
                related: Box::new([]),
            },
        ));
    }
    facts
}

#[test]
fn corpus_admission_moves_many_small_facts_without_per_row_allocations() {
    exact_admission_moves_buffer(Vec::new());
    for count in [0, 1, 127, 4096] {
        exact_admission_moves_buffer(structural_facts(count));
    }
}

#[test]
fn corpus_admission_preserves_sql_long_paths_unicode_and_maximum_ordinal() {
    exact_admission_moves_buffer(sql_facts(
        48,
        &format!("{}/α.sql", "long-path/".repeat(512)),
    ));
}

#[test]
fn corpus_admission_validates_all_compiler_identities_without_scratch_strings() {
    let package = backend_library::package_key("compiler-corpus");
    let corpus = compiler_external_corpus(package, [4; 32], [4; 32]).expect("compiler corpus");
    exact_admission_moves_buffer(corpus.facts().to_vec());
}

#[test]
fn corpus_admission_one_byte_short_refuses_before_new_allocations() {
    let selected_workspace = workspace();
    let facts = sql_facts(32, "migrations/schema.sql");
    let limits = limits_for(&facts);
    let required = measure_semantic_query_facts_with_quota_controlled(
        &facts,
        facts.capacity(),
        limits,
        64,
        &|| false,
    )
    .expect("facts")
        + semantic_query_corpus_admission_overhead(facts.len(), 64).expect("index");
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            SemanticQueryCorpus::admit_with_budget_controlled(
                selected_workspace,
                facts,
                limits,
                SemanticQueryAdmissionBudget {
                    maximum_requested_bytes: required - 1,
                    allocation_quota_per_allocation: 64
                },
                &|| false,
            ),
            Err(SemanticQueryError::InvalidLimit)
        ));
    });
    assert_eq!(measured.count_total, 0);
}

#[test]
fn corpus_admission_zero_credit_malformed_fields_do_not_allocate_error_text() {
    let selected_workspace = workspace();
    for empty_coordinate in [false, true] {
        let mut facts = structural_facts(0);
        let mut presentation = facts[0].presentation().clone();
        if empty_coordinate {
            presentation.coordinate.clear();
        } else {
            presentation.id.clear();
        }
        facts[0] = SemanticQueryFact::new(facts[0].evidence().clone(), presentation);
        let limits = limits_for(&facts);
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                SemanticQueryCorpus::admit_with_budget_controlled(
                    selected_workspace,
                    facts,
                    limits,
                    SemanticQueryAdmissionBudget {
                        maximum_requested_bytes: 0,
                        allocation_quota_per_allocation: 0
                    },
                    &|| false,
                ),
                Err(SemanticQueryError::InvalidLimit)
            ));
        });
        assert_eq!(measured.count_total, 0);
    }
}

#[test]
fn corpus_admission_zero_credit_alias_envelope_is_checked_without_allocation() {
    let selected_workspace = workspace();
    use backend_library::{
        CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1, CargoPackageAliasEvidenceV1,
        CargoPackageAliasObservationV1, CargoPackageAliasV1, CargoTargetNameV1,
    };
    let aliases = CargoPackageAliasEvidenceV1::from_wire_parts(
        vec![CargoPackageAliasV1::CargoTargetName(
            CargoTargetNameV1::new("a".repeat(512)).expect("alias"),
        )],
        vec![
            CargoPackageAliasObservationV1::from_wire_parts(
                LanguageProfile::Rust(RustEdition::Rust2021),
                [1; 32],
                Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                    [2; 32], [3; 32], [4; 32],
                )),
                vec![0],
                CargoPackageAliasCoverageV1::Complete,
            )
            .expect("alias observation"),
        ],
    )
    .expect("admitted alias evidence");
    let mut facts = structural_facts(0);
    let SemanticQueryEvidence::Package(scope) = facts[0].evidence() else {
        unreachable!()
    };
    let package = scope.package();
    let presentation = facts[0].presentation().clone();
    facts[0] = SemanticQueryFact::new(
        SemanticQueryEvidence::Package(PackageScopeEvidence::with_cargo_aliases(package, aliases)),
        presentation,
    );
    let ordinary_limits = limits_for(&facts);
    let retained = measure_semantic_query_facts(&facts, facts.capacity(), ordinary_limits)
        .expect("retained aliases");
    let with_policy = measure_semantic_query_facts_with_quota_controlled(
        &facts,
        facts.capacity(),
        ordinary_limits,
        64,
        &|| false,
    )
    .expect("alias policy");
    // One fact buffer, four presentation strings, alias/observation boxes,
    // one alias text box and one observation's index box.
    assert_eq!(with_policy - retained, 9 * 64);
    let limits = Limits {
        max_field_bytes: 256,
        ..limits_for(&facts)
    };
    for maximum in [0, usize::MAX] {
        let input = facts.clone();
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                SemanticQueryCorpus::admit_with_budget_controlled(
                    selected_workspace,
                    input,
                    limits,
                    SemanticQueryAdmissionBudget {
                        maximum_requested_bytes: maximum,
                        allocation_quota_per_allocation: 0
                    },
                    &|| false,
                ),
                Err(SemanticQueryError::InvalidLimit)
            ));
        });
        assert_eq!(
            measured.count_total, 0,
            "alias envelope rejected with credit {maximum}"
        );
    }
}

#[test]
fn corpus_admission_cancellation_is_observed_early_and_late() {
    let selected_workspace = workspace();
    let facts = structural_facts(256);
    let limits = limits_for(&facts);
    let budget = SemanticQueryAdmissionBudget {
        maximum_requested_bytes: usize::MAX,
        allocation_quota_per_allocation: 64,
    };
    let probes = Cell::new(0usize);
    SemanticQueryCorpus::admit_with_budget_controlled(
        selected_workspace,
        facts.clone(),
        limits,
        budget,
        &|| {
            probes.set(probes.get() + 1);
            false
        },
    )
    .expect("probe baseline");
    let total = probes.get();
    assert!(
        total > facts.len() * 4,
        "measurement, sort, validation and digest must all observe cancellation"
    );
    for threshold in [1, total / 3, total * 2 / 3, total] {
        let mut cancelled_result = None;
        let input = facts.clone();
        probes.set(0);
        let measured = allocation_counter::measure(|| {
            cancelled_result = Some(SemanticQueryCorpus::admit_with_budget_controlled(
                selected_workspace,
                input,
                limits,
                budget,
                &|| {
                    probes.set(probes.get() + 1);
                    probes.get() >= threshold
                },
            ));
        });
        assert!(
            matches!(cancelled_result, Some(Err(SemanticQueryError::Cancelled))),
            "cancellation threshold {threshold}/{total}"
        );
        assert_eq!(probes.get(), threshold);
        assert!(
            measured.count_total <= 1,
            "cancelled admission never allocates the shared owner"
        );
    }
}

#[test]
fn corpus_admission_sorted_index_retains_identity_and_scope_refusals() {
    let selected_workspace = workspace();
    let budget = SemanticQueryAdmissionBudget {
        maximum_requested_bytes: usize::MAX,
        allocation_quota_per_allocation: 0,
    };
    for mutation in 0..5 {
        let mut facts = sql_facts(2, "schema.sql");
        let mut presentation = facts[1].presentation().clone();
        match mutation {
            0 => presentation.id = facts[2].presentation().id.clone(),
            1 => presentation.project = Some("package:missing".into()),
            2 => presentation.coordinate.push_str("::forged"),
            3 => presentation
                .id
                .replace_range("symbol:".len().., &"0".repeat(64)),
            _ => presentation.related = vec![facts[2].presentation().id.clone()].into_boxed_slice(),
        }
        facts[1] = SemanticQueryFact::new(facts[1].evidence().clone(), presentation);
        let limits = limits_for(&facts);
        assert!(
            matches!(
                SemanticQueryCorpus::admit_with_budget_controlled(
                    selected_workspace,
                    facts,
                    limits,
                    budget,
                    &|| false
                ),
                Err(SemanticQueryError::Evidence(_))
            ),
            "mutation {mutation}"
        );
    }
}

#[test]
fn corpus_admission_overhead_checks_layout_and_policy_overflow() {
    assert!(semantic_query_corpus_admission_overhead(usize::MAX, 0).is_err());
    assert!(semantic_query_corpus_admission_overhead(1, usize::MAX).is_err());
}

#[test]
fn corpus_admission_retained_policy_counts_owners_instead_of_vec_cells() {
    let facts = sql_facts(3, "schema.sql");
    let limits = limits_for(&facts);
    let base = measure_semantic_query_facts(&facts, facts.capacity(), limits).expect("facts");
    let with_policy = measure_semantic_query_facts_with_quota_controlled(
        &facts,
        facts.capacity(),
        limits,
        64,
        &|| false,
    )
    .expect("facts with policy");
    // One vector allocation, four project strings and eight owned text
    // allocations per SQL fact. Vec slots are already included by capacity.
    assert_eq!(with_policy - base, (1 + 4 + 3 * 8) * 64);
}
