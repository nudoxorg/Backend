//! Tests for `heart::access` — the multi-source federation vocabulary.
//! Access control types (Tenant, Visibility, AccessContext, etc.) were removed;
//! only Source and Federation remain.

use heart::{
    Id,
    access::{Federation, Source, SourceId, SourceRole},
    error::BackendKind,
};

/// Every external provider is addressed through a `Source`, so the rest of the
/// system is provider-agnostic.
#[test]
fn providers_are_addressed_through_sources() {
    for backend in
        [BackendKind::Terminus, BackendKind::Qdrant, BackendKind::Tantivy, BackendKind::Catalog]
    {
        let source = Source::new("provider".into(), backend);
        assert_eq!(source.backend, backend);
    }
}

/// A definitive base plus self-hosted overlays coexist, and resolve in
/// precedence order: overlays first (override), then the base (extend).
#[test]
fn multiple_sources_can_coexist() {
    let base: SourceId = Id::new_random();
    let overlay_a: SourceId = Id::new_random();
    let overlay_b: SourceId = Id::new_random();

    // Handles are irrelevant here — federation is generic over them.
    let fed = Federation::new(base, ()).with_overlay(overlay_a, ()).with_overlay(overlay_b, ());

    let order: Vec<(_, SourceRole)> =
        fed.in_precedence().map(|s| (s.source, s.role)).collect();
    // Overlays first (in declared order), then the definitive base last.
    assert_eq!(
        order,
        vec![
            (overlay_a, SourceRole::Overlay),
            (overlay_b, SourceRole::Overlay),
            (base, SourceRole::Definitive),
        ]
    );
}
