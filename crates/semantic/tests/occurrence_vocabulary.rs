//! Proves the occurrence-plane vocabulary: the confidence lattice ordering,
//! the frozen wire discriminants, and the owner-relative span algebra.
//! Assertions retain exact typed causes so regressions cannot pass lossily.

use backend_semantic::ir_vocabulary::{Confidence, ReferenceKind, RelSpan, RelSpanFault};
use thiserror::Error;

/// Typed propagation keeps every assertion exact without panicking seams.
#[derive(Debug, Error)]
enum TestFailure {
    #[error("relative span rejected: {0:?}")]
    Span(RelSpanFault),
}

#[test]
fn confidence_lattice_ordering_is_load_bearing() {
    // Total order from surface facts to oracle resolutions.
    assert!(Confidence::Oracle > Confidence::Syntactic);
    assert!(Confidence::Oracle > Confidence::Import);
    assert!(Confidence::Import > Confidence::Index);
    assert!(Confidence::Index > Confidence::Suffix);
    assert!(Confidence::Suffix > Confidence::Syntactic);
    // The graph floor is exactly Index: suffix and surface facts never
    // project into the graph.
    assert!(!Confidence::Syntactic.is_graph_worthy());
    assert!(!Confidence::Suffix.is_graph_worthy());
    assert!(Confidence::Index.is_graph_worthy());
    assert!(Confidence::Import.is_graph_worthy());
    assert!(Confidence::Oracle.is_graph_worthy());
    assert_eq!(Confidence::GRAPH_FLOOR, Confidence::Index);
}

#[test]
fn confidence_discriminants_round_trip_and_reject_unknowns() {
    for (confidence, code) in [
        (Confidence::Syntactic, 0_u8),
        (Confidence::Suffix, 1),
        (Confidence::Index, 2),
        (Confidence::Import, 3),
        (Confidence::Oracle, 4),
    ] {
        assert_eq!(u8::from(confidence), code);
        assert_eq!(Confidence::try_from(code), Ok(confidence));
    }
    assert_eq!(
        Confidence::try_from(5),
        Err(backend_semantic::ir_vocabulary::ConfidenceCodeError { actual: 5 })
    );
    assert_eq!(
        Confidence::try_from(255),
        Err(backend_semantic::ir_vocabulary::ConfidenceCodeError { actual: 255 })
    );
}

#[test]
fn reference_kind_discriminants_round_trip_and_reject_unknowns() {
    for (kind, code) in [
        (ReferenceKind::FunctionCall, 0_u8),
        (ReferenceKind::MethodCall, 1),
        (ReferenceKind::TypeReference, 2),
        (ReferenceKind::VariableUse, 3),
        (ReferenceKind::MacroInvocation, 4),
        (ReferenceKind::FieldAccess, 5),
        (ReferenceKind::Import, 6),
        (ReferenceKind::Overrides, 7),
    ] {
        assert_eq!(u8::from(kind), code);
        assert_eq!(ReferenceKind::try_from(code), Ok(kind));
    }
    assert_eq!(
        ReferenceKind::try_from(8),
        Err(backend_semantic::ir_vocabulary::ReferenceKindCodeError { actual: 8 })
    );
    assert_eq!(
        ReferenceKind::try_from(255),
        Err(backend_semantic::ir_vocabulary::ReferenceKindCodeError { actual: 255 })
    );
}

#[test]
fn relative_spans_stay_ordered_and_compose() -> Result<(), TestFailure> {
    assert_eq!(
        RelSpan::new(5, 4),
        Err(RelSpanFault::Inverted { start: 5, end: 4 })
    );
    let span = RelSpan::new(2, 5).map_err(TestFailure::Span)?;
    assert_eq!(span.len(), 3);
    assert!(!span.is_empty());
    // Touching spans do not overlap; sharing one byte does.
    assert!(span.overlaps(RelSpan::new_trusted(4, 9)));
    assert!(!span.overlaps(RelSpan::new_trusted(5, 9)));
    assert!(span.contains(RelSpan::new_trusted(3, 5)));
    assert!(!span.contains(RelSpan::new_trusted(3, 6)));
    // Zero-width spans: two of them never overlap — not even identical
    // spans at the same offset — but a point strictly inside a non-empty
    // span overlaps and is contained by it.
    let zero = RelSpan::new_trusted(3, 3);
    assert!(zero.is_empty());
    assert!(!zero.overlaps(zero));
    assert!(!zero.overlaps(RelSpan::new_trusted(4, 4)));
    assert!(zero.overlaps(RelSpan::new_trusted(0, 4)));
    assert!(zero.contains(zero));
    Ok(())
}
