use std::{
    hint::black_box,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

use backend_semantic::ir::{
    BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityId,
    EntityVersion, FactAvailability, IrBuilder, ItemKind, LinkKind, OccurrenceAuthorityFacts,
    ParentageAuthority, SemanticImageView, SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput,
    TreeLinkTarget, VariantFingerprint, Visibility, encode_full_semantic_image,
    full_semantic_image_len,
};
use futures_core::Stream;
use server_index_graph_vector::Cancellation;
use backend_extension_trustfall::server::SemanticTrustfallGraph;

fn fixture(count: u32) -> Result<(Vec<u8>, EntityId), Box<dyn std::error::Error>> {
    let mut builder = IrBuilder::new();
    let file = builder.intern_atom(b"bench/\xff-occurrence.rs")?;
    let versions = [
        EntityVersion {
            family: DeclarationFamilyId::from_raw([1; 16]),
            variant: VariantFingerprint::from_raw([2; 16]),
            core_payload: CorePayloadHash::from_raw([3; 16]),
        },
        EntityVersion {
            family: DeclarationFamilyId::from_raw([4; 16]),
            variant: VariantFingerprint::from_raw([5; 16]),
            core_payload: CorePayloadHash::from_raw([6; 16]),
        },
    ];
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let items = [b"source".as_slice(), b"target".as_slice()].map(|name| TreeItemInput {
        name,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    });
    let links = (0..count)
        .map(|index| TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: if index % 2 == 0 {
                Confidence::Compiler
            } else {
                Confidence::Indexed
            },
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(file, index * 2, index * 2 + 1),
        })
        .collect::<Vec<_>>();
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

/// Traverses one image that was reopened before timing begins.
fn direct(
    image: &SemanticImageView<'_>,
    source: EntityId,
) -> Result<u64, Box<dyn std::error::Error>> {
    let cancellation = Cancellation::new();
    let graph = SemanticTrustfallGraph::new(image, &cancellation);
    let mut stream = graph.occurrence_neighbors(source)?;
    let mut context = Context::from_waker(Waker::noop());
    let mut checksum = 0_u64;
    loop {
        match Pin::new(&mut stream).poll_next(&mut context) {
            Poll::Ready(Some(Ok(hit))) => {
                let evidence = hit.provenance();
                let path_sum = evidence
                    .path()
                    .unwrap_or_default()
                    .iter()
                    .map(|byte| u64::from(*byte))
                    .sum::<u64>();
                let span_sum = evidence
                    .source()
                    .map_or(0, |span| u64::from(span.start()) + u64::from(span.end()));
                checksum = checksum
                    .wrapping_add(u64::from(hit.entity().raw))
                    .wrapping_add(u64::from(hit.occurrence().raw))
                    .wrapping_add(u64::from(hit.link().raw))
                    .wrapping_add(path_sum)
                    .wrapping_add(span_sum);
            }
            Poll::Ready(None) => return Ok(checksum),
            Poll::Ready(Some(Err(error))) => return Err(Box::new(error)),
            Poll::Pending => return Err("unexpected pending direct stream".into()),
        }
    }
}

fn percentile(samples: &mut [Duration], rank: usize) -> Duration {
    samples.sort_unstable();
    samples[rank.min(samples.len() - 1)]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for count in [1, 8, 32, 128] {
        let (bytes, source) = fixture(count)?;
        let image = SemanticImageView::reopen(&bytes)?;
        let count = u64::from(count);
        let path_sum = b"bench/\xff-occurrence.rs"
            .iter()
            .map(|byte| u64::from(*byte))
            .sum::<u64>();
        let occurrence_sum = count.saturating_sub(1) * count / 2;
        let span_sum = 2 * count * count - count;
        let expected = count + occurrence_sum + path_sum * count + span_sum;
        if direct(&image, source)? != expected {
            return Err("benchmark correctness checksum mismatch".into());
        }
        for _ in 0..5 {
            black_box(direct(&image, source)?);
        }
        let mut samples = Vec::with_capacity(20);
        for _ in 0..20 {
            let start = Instant::now();
            black_box(direct(&image, source)?);
            samples.push(start.elapsed());
        }
        let median = percentile(&mut samples, 10);
        let p95 = percentile(&mut samples, 19);
        println!(
            "occurrence_traversal_reopened count={count} samples=20 checksum={expected} median_ns={} p95_ns={}",
            median.as_nanos(),
            p95.as_nanos()
        );
    }
    Ok(())
}
