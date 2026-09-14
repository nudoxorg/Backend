//! Public journeys for lazy Trustfall traversal over reopened semantic-image bytes.
#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
};

use backend_semantic::ir::{
    AtomId, BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
    EntityId, EntityVersion, FactAvailability, IrBuilder, ItemKind, LinkKind,
    OccurrenceAuthorityFacts, ParentageAuthority, SemanticImageView, SourceSpan, TreeEntityId,
    TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use futures_core::Stream;
use backend_semantic::graph_vector::Cancellation;
use backend_extension_trustfall::server::{SemanticTrustfallGraph, TrustfallGraphError};

fn version(seed: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    }
}

fn authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn reopened_image() -> Result<(Vec<u8>, EntityId), Box<dyn std::error::Error>> {
    let versions = [version(1), version(2)];
    let items = [b"source".as_slice(), b"target".as_slice()].map(|name| TreeItemInput {
        name,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    });
    let links = [
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(AtomId::new(0), 10, 12),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Indexed,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(AtomId::new(0), 30, 34),
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    Ok((bytes, EntityId::new(0)))
}

fn single_occurrence_image(
    path: &[u8],
    source_availability: FactAvailability,
    source: Option<SourceSpan>,
) -> Result<(Vec<u8>, EntityId), Box<dyn std::error::Error>> {
    let versions = [version(11), version(12)];
    let items = [b"source".as_slice(), b"target".as_slice()].map(|name| TreeItemInput {
        name,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    });
    let mut builder = IrBuilder::new();
    let file = builder.intern_atom(path)?;
    let link = TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        kind: LinkKind::Calls,
        confidence: Confidence::Compiler,
        authority: OccurrenceAuthorityFacts {
            source: source_availability,
        },
        source: source.and_then(|span| SourceSpan::new(file, span.start(), span.end())),
    };
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[link],
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    Ok((bytes, EntityId::new(0)))
}

#[test]
fn async_query_streams_links_from_reopened_bytes_without_graph_reconstruction()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut stream = graph.neighbors(source)?;
    let mut context = Context::from_waker(Waker::noop());

    let Poll::Ready(Some(Ok(hit))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("semantic Trustfall stream did not yield its local link".into());
    };
    assert_eq!(hit.entity, EntityId::new(1));
    assert_eq!(hit.kind, LinkKind::Calls);
    assert_eq!(hit.confidence, Confidence::Compiler);
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(None)
    ));
    Ok(())
}

#[test]
fn cancellation_is_a_typed_terminal_before_query_work() -> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);

    assert!(matches!(
        graph.neighbors(source),
        Err(TrustfallGraphError::Cancelled)
    ));
    assert!(matches!(
        graph.occurrence_neighbors(source),
        Err(TrustfallGraphError::Cancelled)
    ));
    assert!(matches!(
        graph.incoming_occurrence_neighbors(source),
        Err(TrustfallGraphError::Cancelled)
    ));
    Ok(())
}

#[test]
fn incoming_occurrence_traversal_keeps_the_actual_owner_and_site_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, _) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut stream = graph.incoming_occurrence_neighbors(EntityId::new(1))?;
    let mut context = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(first))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("missing first incoming occurrence".into());
    };
    let Poll::Ready(Some(Ok(second))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("missing second incoming occurrence".into());
    };
    assert_eq!(first.entity(), EntityId::new(0));
    assert_eq!(second.entity(), EntityId::new(0));
    assert_eq!(first.kind(), LinkKind::Calls);
    assert_eq!(second.kind(), LinkKind::Calls);
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(None)
    ));
    Ok(())
}

#[test]
fn occurrence_traversal_is_exhaustive_and_keeps_site_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut stream = graph.occurrence_neighbors(source)?;
    let mut context = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(first))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("missing first occurrence".into());
    };
    let Poll::Ready(Some(Ok(second))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("missing second occurrence".into());
    };
    assert_eq!(first.entity(), EntityId::new(1));
    assert_eq!(second.entity(), EntityId::new(1));
    let observed = [first, second].map(|hit| {
        (
            hit.occurrence().raw,
            hit.link().raw,
            hit.confidence(),
            hit.provenance()
                .source()
                .map(|span| (span.start(), span.end())),
        )
    });
    assert!(observed.iter().any(|(_, link, confidence, source)| {
        *link == 0 && *confidence == Confidence::Compiler && *source == Some((10, 12))
    }));
    assert!(observed.iter().any(|(_, link, confidence, source)| {
        *link == 0 && *confidence == Confidence::Indexed && *source == Some((30, 34))
    }));
    assert!(observed.iter().all(|(_, _, _, source)| source.is_some()));
    assert_eq!(
        first.provenance().availability(),
        FactAvailability::Captured
    );
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(None)
    ));
    Ok(())
}

#[test]
fn occurrence_traversal_cancellation_fuses_without_complete_terminal()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut stream = graph.occurrence_neighbors(source)?;
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(Some(Ok(_)))
    ));
    cancellation.cancel();
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(Some(Err(TrustfallGraphError::Cancelled)))
    ));
    assert!(matches!(
        Pin::new(&mut stream).poll_next(&mut context),
        Poll::Ready(None)
    ));
    Ok(())
}

#[test]
fn occurrence_source_borrows_non_utf8_path_after_stream_container_expires()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = single_occurrence_image(
        b"src/\xff-occurrence.rs",
        FactAvailability::Captured,
        SourceSpan::new(AtomId::new(0), 5, 5),
    )?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut context = Context::from_waker(Waker::noop());
    let hit = {
        let mut selected = graph.occurrence_neighbors(source)?;
        let Poll::Ready(Some(Ok(hit))) = Pin::new(&mut selected).poll_next(&mut context) else {
            return Err("missing captured occurrence".into());
        };
        hit
    };
    let evidence = hit.provenance();
    assert_eq!(evidence.path(), Some(&b"src/\xff-occurrence.rs"[..]));
    assert_eq!(
        evidence.source().map(|span| (span.start(), span.end())),
        Some((5, 5))
    );
    assert_eq!(hit.image().as_ref(), image.as_ref());
    Ok(())
}

#[test]
fn occurrence_source_unavailable_is_closed_and_has_no_path()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) =
        single_occurrence_image(b"not-retained.rs", FactAvailability::Unavailable, None)?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);
    let mut stream = graph.occurrence_neighbors(source)?;
    let mut context = Context::from_waker(Waker::noop());
    let Poll::Ready(Some(Ok(hit))) = Pin::new(&mut stream).poll_next(&mut context) else {
        return Err("missing unavailable occurrence".into());
    };
    assert_eq!(
        hit.provenance().availability(),
        FactAvailability::Unavailable
    );
    assert_eq!(hit.provenance().source(), None);
    assert_eq!(hit.provenance().path(), None);
    Ok(())
}
