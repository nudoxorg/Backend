//! Canonical fact-emission proofs for the direct Clang semantic frontend.
//! Each mapped kind owns a source-mutation falsifier: mutating the source changes the fact
//! exactly where the mutation lands, the kind follows the source construct (never the
//! spelling), and identities derive from USR authority rather than span or appearance order.

use super::common::{Fixture, TestError, run_analysis};
use super::{ClangFact, ClangSourceLanguage, EntityFact, MAX_ANALYSIS_SCRATCH_BYTES, SemanticKind};

/// One canonical kind row: a minimal source, its dialect, and the exact entity facts the
/// real authority must admit for it.
struct KindRow {
    language: ClangSourceLanguage,
    source: &'static [u8],
    expected: &'static [(&'static str, SemanticKind)],
}

const KIND_TABLE: &[KindRow] = &[
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"int helper(void) { return 0; }\n",
        expected: &[("helper", SemanticKind::Function)],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"struct Widget { int field; };\n",
        expected: &[
            ("Widget", SemanticKind::Record),
            ("field", SemanticKind::Field),
        ],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"enum Choice { PICKED = 1 };\n",
        expected: &[
            ("Choice", SemanticKind::Enum),
            ("PICKED", SemanticKind::Variant),
        ],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"typedef int Speed;\n",
        expected: &[("Speed", SemanticKind::Alias)],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"const int limit = 1;\n",
        expected: &[("limit", SemanticKind::Constant)],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"int counter = 0;\n",
        expected: &[("counter", SemanticKind::Static)],
    },
    KindRow {
        language: ClangSourceLanguage::C,
        source: b"int with_parameter(int alpha);\n",
        expected: &[
            ("with_parameter", SemanticKind::Function),
            ("alpha", SemanticKind::Parameter),
        ],
    },
    KindRow {
        language: ClangSourceLanguage::Cxx,
        source: b"namespace NS { int inner = 3; }\n",
        expected: &[
            ("NS", SemanticKind::Module),
            ("inner", SemanticKind::Static),
        ],
    },
];

fn extension(language: ClangSourceLanguage) -> &'static str {
    match language {
        ClangSourceLanguage::C => "c",
        ClangSourceLanguage::Cxx => "cc",
    }
}

/// Runs one analysis and returns the admitted entity facts sorted by name.
fn sorted_entities(
    source: &[u8],
    language: ClangSourceLanguage,
) -> Result<Vec<EntityFact<'_>>, TestError> {
    let fixture = Fixture::new(extension(language))?;
    let analysis = run_analysis(
        &fixture,
        source,
        language,
        None,
        None,
        MAX_ANALYSIS_SCRATCH_BYTES,
        MAX_ANALYSIS_SCRATCH_BYTES,
    )?;
    let mut entities: Vec<EntityFact<'_>> = analysis
        .facts
        .iter()
        .filter_map(|fact| match fact {
            ClangFact::Entity(item) => Some(*item),
            _ => None,
        })
        .collect();
    entities.sort_by(|left, right| left.name.cmp(right.name));
    Ok(entities)
}

/// Finds the first entity fact named `name`.
fn entity_named<'a, 'fact>(
    entities: &'a [EntityFact<'fact>],
    name: &str,
) -> Option<&'a EntityFact<'fact>> {
    entities.iter().find(|item| item.name == name)
}

#[test]
fn every_mapped_source_construct_emits_its_exact_canonical_kinds() -> Result<(), TestError> {
    super::common::require_linked_library()?;
    super::common::real_clang()?;
    for row in KIND_TABLE {
        let entities = sorted_entities(row.source, row.language)?;
        let mut expected: Vec<(&str, SemanticKind)> = row.expected.to_vec();
        expected.sort_by(|left, right| left.0.cmp(right.0));
        let observed: Vec<(&str, SemanticKind)> =
            entities.iter().map(|item| (item.name, item.kind)).collect();
        assert_eq!(
            observed,
            expected,
            "kind mapping drifted for source {}",
            String::from_utf8_lossy(row.source)
        );
        for item in &entities {
            let slice = &row.source[item.span.start as usize..item.span.end as usize];
            assert_eq!(
                slice,
                item.name.as_bytes(),
                "span does not re-slice to the name"
            );
            assert!(
                item.owner.start <= item.span.start && item.owner.end >= item.span.end,
                "owner does not enclose the declaration"
            );
        }
    }
    Ok(())
}

#[test]
fn byte_prefix_mutation_shifts_every_span_exactly_and_keeps_identities() -> Result<(), TestError> {
    super::common::require_linked_library()?;
    super::common::real_clang()?;
    const DELTA: u32 = 5;
    for row in KIND_TABLE {
        let mut source = Vec::new();
        source.extend_from_slice(b"/*x*/");
        source.extend_from_slice(row.source);
        let original = sorted_entities(row.source, row.language)?;
        let shifted = sorted_entities(&source, row.language)?;
        assert_eq!(original.len(), shifted.len());
        for (before, after) in original.iter().zip(shifted.iter()) {
            assert_eq!(before.name, after.name);
            assert_eq!(after.span.start, before.span.start + DELTA);
            assert_eq!(after.span.end, before.span.end + DELTA);
            assert_eq!(after.identity, before.identity);
            assert_eq!(
                &source[after.span.start as usize..after.span.end as usize],
                after.name.as_bytes()
            );
        }
    }
    Ok(())
}

#[test]
fn const_qualification_flips_the_same_spelling_between_static_and_constant() -> Result<(), TestError>
{
    super::common::require_linked_library()?;
    super::common::real_clang()?;
    let plain = sorted_entities(b"int counter = 0;\n", ClangSourceLanguage::C)?;
    let qualified = sorted_entities(b"const int counter = 0;\n", ClangSourceLanguage::C)?;
    assert_eq!(
        entity_named(&plain, "counter").map(|item| item.kind),
        Some(SemanticKind::Static)
    );
    assert_eq!(
        entity_named(&qualified, "counter").map(|item| item.kind),
        Some(SemanticKind::Constant)
    );
    Ok(())
}

#[test]
fn construct_mutation_flips_record_field_onto_enum_variant_without_renaming()
-> Result<(), TestError> {
    super::common::require_linked_library()?;
    super::common::real_clang()?;
    let record = sorted_entities(b"struct Mood { int field; };\n", ClangSourceLanguage::C)?;
    let enumerated = sorted_entities(b"enum Mood { field };\n", ClangSourceLanguage::C)?;
    assert_eq!(
        entity_named(&record, "Mood").map(|item| item.kind),
        Some(SemanticKind::Record)
    );
    assert_eq!(
        entity_named(&record, "field").map(|item| item.kind),
        Some(SemanticKind::Field)
    );
    assert_eq!(
        entity_named(&enumerated, "Mood").map(|item| item.kind),
        Some(SemanticKind::Enum)
    );
    assert_eq!(
        entity_named(&enumerated, "field").map(|item| item.kind),
        Some(SemanticKind::Variant)
    );
    Ok(())
}

#[test]
fn forward_and_defining_declarations_share_one_usr_identity() -> Result<(), TestError> {
    super::common::require_linked_library()?;
    super::common::real_clang()?;
    let source = b"struct W;\nstruct W { int f; };\n";
    let entities = sorted_entities(source, ClangSourceLanguage::C)?;
    let records: Vec<EntityFact<'_>> = entities
        .iter()
        .filter(|item| item.name == "W")
        .copied()
        .collect();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].span, records[1].span);
    assert_eq!(records[0].identity, records[1].identity);
    let field =
        entity_named(&entities, "f").ok_or_else(|| TestError("field fact missing".to_owned()))?;
    assert_ne!(field.identity, records[0].identity);
    Ok(())
}
