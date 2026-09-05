//! Public journeys for lazy Trustfall traversal over reopened semantic-image bytes.
#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use core::{pin::Pin, task::{Context, Poll, Waker}};

use compiler_ir::{
    BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
    EntityId, EntityVersion, FactAvailability, IrBuilder, ItemKind, LinkKind,
    OccurrenceAuthorityFacts, ParentageAuthority, SemanticImageView, TreeEntityId,
    TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use futures_core::Stream;
use server_index_graph_vector::Cancellation;
use server_index_trustfall::{SemanticTrustfallGraph, TrustfallGraphError};

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
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        kind: LinkKind::Calls,
        confidence: Confidence::Compiler,
        authority: OccurrenceAuthorityFacts::default(),
        source: None,
    }];
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
fn cancellation_is_a_typed_terminal_before_query_work()
-> Result<(), Box<dyn std::error::Error>> {
    let (bytes, source) = reopened_image()?;
    let image = SemanticImageView::reopen(&bytes)?;
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let graph = SemanticTrustfallGraph::new(&image, &cancellation);

    assert!(matches!(
        graph.neighbors(source),
        Err(TrustfallGraphError::Cancelled)
    ));
    Ok(())
}
