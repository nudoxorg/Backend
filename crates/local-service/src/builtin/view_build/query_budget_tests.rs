//! The corpus budget follows from measured layout cost, not from a guess.

use super::{MAX_SEMANTIC_CORPUS_BYTES, MAX_SEMANTIC_DOCUMENT_BYTES, MAX_SEMANTIC_QUERY_ROWS};
use backend_extension_trustfall::{
    Limits, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
    StructuralFallbackEvidence, measure_semantic_query_facts_with_quota_controlled,
};

/// Characters of a stable row id: `symbol:` and 64 lowercase hex digits.
const STABLE_ID_BYTES: usize = 71;

fn stable_id(index: usize) -> String {
    backend_library::RowId::Symbol(backend_library::symbol_key(&format!("budget::{index}")))
        .stable_key()
}

/// Rows shaped like a large native declaration: a path-qualified coordinate, a
/// signature and a short document, each linking `related` other rows.
fn facts(rows: usize, related: usize) -> Vec<SemanticQueryFact> {
    let package = backend_library::package_key("budget");
    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2021,
    );
    let ids = (0..rows.max(related)).map(stable_id).collect::<Vec<_>>();
    (0..rows)
        .map(|index| {
            SemanticQueryFact::new(
                SemanticQueryEvidence::StructuralFallback(StructuralFallbackEvidence::new(
                    package,
                    profile,
                    [index as u8; 32],
                    [8; 32],
                )),
                SemanticQueryPresentation {
                    id: ids[index].clone(),
                    kind: "function".to_owned(),
                    coordinate: format!(
                        "/project/src/module/file_{index}.cpp::ns::Type::method_{index}"
                    ),
                    name: format!("method_{index}"),
                    signature: Some(format!(
                        "void Type::method_{index}(const char* name, int flags)"
                    )),
                    documentation: "Loads the buffer and reports the parse status.".to_owned(),
                    score: None,
                    project: Some(stable_id(usize::MAX)),
                    parent: None,
                    related: (0..related)
                        .map(|edge| ids[(index + edge + 1) % ids.len()].clone())
                        .collect(),
                },
            )
        })
        .collect()
}

fn measured(facts: &[SemanticQueryFact]) -> usize {
    let limits = Limits {
        max_rows: MAX_SEMANTIC_QUERY_ROWS,
        max_fields_per_row: MAX_SEMANTIC_QUERY_ROWS,
        max_field_bytes: MAX_SEMANTIC_DOCUMENT_BYTES,
        max_total_bytes: usize::MAX / 2,
        ..Limits::default()
    };
    // A pushed `Vec` may retain up to twice its length in capacity.
    measure_semantic_query_facts_with_quota_controlled(facts, facts.len() * 2, limits, 64, &|| {
        false
    })
    .expect("a bounded corpus measures")
}

/// Everything the row bound and the 64 MiB wire envelope admitted must still
/// admit: the worst corpus is the row bound's fixed per-row layout plus the
/// envelope's worth of tiny related ids, each at its measured retained cost.
#[test]
fn corpus_budget_admits_everything_the_row_and_envelope_bounds_admitted() {
    const ROWS: usize = 2_048;
    const EDGES: usize = 8;
    let bare = measured(&facts(ROWS, 0));
    let linked = measured(&facts(ROWS, EDGES));
    let per_row = bare.div_ceil(ROWS);
    let per_edge = (linked - bare).div_ceil(ROWS * EDGES);
    // One related id costs an envelope charge of its text; a corpus at the old
    // envelope bound could hold at most this many of them.
    let envelope_edges = super::super::super::MAX_REBUILD_BYTES / STABLE_ID_BYTES;
    let worst = MAX_SEMANTIC_QUERY_ROWS * per_row + envelope_edges * per_edge;
    assert!(
        worst <= MAX_SEMANTIC_CORPUS_BYTES,
        "per row {per_row} bytes, per edge {per_edge} bytes: {worst} exceeds {MAX_SEMANTIC_CORPUS_BYTES}"
    );
    // The budget is not a rounding of the worst case to infinity: it stays
    // within a small multiple of it.
    assert!(MAX_SEMANTIC_CORPUS_BYTES <= 4 * worst);
}

/// A project the size of the largest real corpus (25,228 view rows, each
/// linking a dozen others) fits with room to spare.
#[test]
fn a_large_native_project_fits_the_corpus_budget() {
    const ROWS: usize = 25_228;
    let bytes = measured(&facts(ROWS, 12));
    assert!(bytes <= MAX_SEMANTIC_CORPUS_BYTES / 2, "{bytes} bytes");
}
