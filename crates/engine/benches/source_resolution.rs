//! Measures proof-preserving source resolution for a high-ordinal reopened image.
//!
//! The comparison keeps hit cardinality and the complete source checksum equal:
//! individual resolution repeats the public single-hit preflight, while batch
//! resolution proves reopened publication/image facts once and then directly
//! looks up each canonical entity and path.

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityId,
    EntityVersion, FactAvailability, IrBuilder, ItemKind, ParentageAuthority,
    SemanticImageIdentity, SemanticImageView, SourceSpan, TreeItemInput, VariantFingerprint,
    Visibility, encode_full_semantic_image, full_semantic_image_len,
};
use backend_version::{CompilePublicationDomain, ContentId, GenerationId};
use backend_semantic::index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalOperation, LexicalRow,
    LexicalScore, LexicalSegment,
};
use backend_engine::retrieval::{CanonicalSource, resolve_tantivy_source, resolve_tantivy_sources};
use backend_extension_tantivy::server::{TantivySegmentHit, TantivySegmentStore};
use backend_semantic::index_vocabulary::{
    IndexLocatorFacts, SemanticImageExtent, SemanticImageLocator, VerifiedSemanticPublication,
};

const ENTITY_COUNT: usize = 128;
const WARMUPS: usize = 5;
const SAMPLES: usize = 20;

struct Fixture {
    root: std::path::PathBuf,
    store: TantivySegmentStore,
    segment: backend_extension_tantivy::server::TantivySegment,
    bytes: Vec<u8>,
    locator: SemanticImageLocator,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let mut builder = IrBuilder::new();
    let file = builder.intern_atom(b"bench/\xff-source.rs")?;
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let mut versions = Vec::with_capacity(ENTITY_COUNT);
    let mut items = Vec::with_capacity(ENTITY_COUNT);
    for ordinal in 0..ENTITY_COUNT {
        let seed = u8::try_from(ordinal)?;
        versions.push(EntityVersion {
            family: DeclarationFamilyId::from_raw([seed; 16]),
            variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
            core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
        });
        let start = u32::try_from(ordinal.checked_mul(3).ok_or("benchmark span overflow")?)?;
        items.push(TreeItemInput {
            name: b"entry",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(file, start, start + 2),
            extension: None,
        });
    }
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    let image = SemanticImageView::reopen(&bytes)?;
    let byte_length = u32::try_from(bytes.len())?;
    let locator = SemanticImageLocator::new(
        SemanticImageIdentity::from_encoded_bytes(image.as_ref()),
        SemanticImageExtent::new(0, byte_length)
            .map_err(|_| std::io::Error::other("benchmark image extent was rejected"))?,
    );

    let root = std::env::temp_dir().join(format!(
        "nudox-source-resolution-bench-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let store = TantivySegmentStore::open(&root)?;
    let rows = (0..ENTITY_COUNT)
        .map(|ordinal| {
            Ok(LexicalRow::new(
                b"needle",
                EntityDocumentId {
                    artifact: EntityArtifactIdentity::Semantic(locator.identity),
                    entity: EntityId::new(u32::try_from(ordinal)?),
                },
                LexicalScore::from(1),
            ))
        })
        .collect::<Result<Vec<_>, std::num::TryFromIntError>>()?;
    let lexical = LexicalSegment::new(&rows)
        .map_err(|_| std::io::Error::other("benchmark lexical segment was rejected"))?;
    let segment = store.project(lexical)?;
    Ok(Fixture {
        root,
        store,
        segment,
        bytes,
        locator,
    })
}

fn checksum(
    sources: &[Option<CanonicalSource<'_, '_>>],
) -> Result<u64, Box<dyn std::error::Error>> {
    sources.iter().try_fold(0_u64, |sum, source| {
        let source = source
            .as_ref()
            .ok_or_else(|| std::io::Error::other("benchmark source missing"))?;
        let path = source.path().iter().fold(0_u64, |path_sum, byte| {
            path_sum.wrapping_add(u64::from(*byte))
        });
        Ok(sum
            .wrapping_add(u64::from(source.document().entity.raw))
            .wrapping_add(u64::try_from(source.provenance().row().as_usize())?)
            .wrapping_add(u64::from(source.span().start()))
            .wrapping_add(u64::from(source.span().end()))
            .wrapping_add(path))
    })
}

fn individual<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
    hits: &[Option<TantivySegmentHit<'_>>],
    output: &mut [Option<CanonicalSource<'image, 'bytes>>],
) -> Result<u64, Box<dyn std::error::Error>> {
    if output.len() != hits.len() {
        return Err("benchmark output cardinality mismatch".into());
    }
    for (slot, hit) in output.iter_mut().zip(hits) {
        *slot = Some(resolve_tantivy_source(
            publication,
            image,
            hit.ok_or_else(|| std::io::Error::other("benchmark hit missing"))?,
        )?);
    }
    checksum(output)
}

fn batch<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
    hits: &[Option<TantivySegmentHit<'_>>],
    scratch: &mut [Option<CanonicalSource<'image, 'bytes>>],
    output: &mut [Option<CanonicalSource<'image, 'bytes>>],
) -> Result<u64, Box<dyn std::error::Error>> {
    resolve_tantivy_sources(publication, image, hits, scratch, output)?;
    checksum(output)
}

fn percentile(
    samples: &mut [Duration],
    rank: usize,
) -> Result<Duration, Box<dyn std::error::Error>> {
    samples.sort_unstable();
    samples
        .get(rank.min(samples.len().saturating_sub(1)))
        .copied()
        .ok_or_else(|| std::io::Error::other("benchmark sample set was empty").into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture()?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let selection = [fixture.segment.id()];
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"source-resolution-bench-generation"),
        &[],
        &selection,
    )?;
    let facts = IndexLocatorFacts::new(
        GenerationId::from_canonical_bytes(b"source-resolution-bench-generation"),
        snapshot.id,
        ContentId::<CompilePublicationDomain>::from_canonical_bytes(
            b"source-resolution-bench-publication",
        ),
    );
    let publication = VerifiedSemanticPublication::verify_reopened(facts, fixture.locator, &image)
        .map_err(|_| std::io::Error::other("benchmark publication was rejected"))?;
    let segments = [&fixture.segment];
    let pinned = fixture.store.compose(snapshot, &segments)?;
    let mut hits = vec![None; ENTITY_COUNT];
    let mut candidates = vec![None; ENTITY_COUNT * 2];
    let written = pinned.search(
        LexicalOperation::new(b"needle"),
        ENTITY_COUNT,
        &mut hits,
        &mut candidates,
    )?;
    if written != ENTITY_COUNT {
        return Err("benchmark durable hit cardinality mismatch".into());
    }
    let mut individual_output = std::iter::repeat_with(|| None)
        .take(ENTITY_COUNT)
        .collect::<Vec<_>>();
    let mut batch_scratch = std::iter::repeat_with(|| None)
        .take(ENTITY_COUNT)
        .collect::<Vec<_>>();
    let mut batch_output = std::iter::repeat_with(|| None)
        .take(ENTITY_COUNT)
        .collect::<Vec<_>>();
    let expected = individual(publication, &image, &hits, &mut individual_output)?;
    if batch(
        publication,
        &image,
        &hits,
        &mut batch_scratch,
        &mut batch_output,
    )? != expected
    {
        return Err("benchmark batch checksum disagrees with individual resolution".into());
    }
    for _ in 0..WARMUPS {
        black_box(individual(
            publication,
            &image,
            &hits,
            &mut individual_output,
        )?);
        black_box(batch(
            publication,
            &image,
            &hits,
            &mut batch_scratch,
            &mut batch_output,
        )?);
    }
    let mut individual_samples = Vec::with_capacity(SAMPLES);
    let mut batch_samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        black_box(individual(
            publication,
            &image,
            &hits,
            &mut individual_output,
        )?);
        individual_samples.push(start.elapsed());
        let start = Instant::now();
        black_box(batch(
            publication,
            &image,
            &hits,
            &mut batch_scratch,
            &mut batch_output,
        )?);
        batch_samples.push(start.elapsed());
    }
    let individual_median = percentile(&mut individual_samples, SAMPLES / 2)?;
    let individual_p95 = percentile(&mut individual_samples, SAMPLES - 1)?;
    let batch_median = percentile(&mut batch_samples, SAMPLES / 2)?;
    let batch_p95 = percentile(&mut batch_samples, SAMPLES - 1)?;
    println!(
        "source_resolution count={ENTITY_COUNT} samples={SAMPLES} checksum={expected} individual_median_ns={} individual_p95_ns={} batch_median_ns={} batch_p95_ns={}",
        individual_median.as_nanos(),
        individual_p95.as_nanos(),
        batch_median.as_nanos(),
        batch_p95.as_nanos(),
    );
    Ok(())
}
