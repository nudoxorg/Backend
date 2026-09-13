//! Hostile source-provenance checks over real reopened semantic and Tantivy images.
//!
//! These tests intentionally construct durable hits through the public projection
//! store.  A document-only legacy Tantivy observation cannot enter this fixture.

use std::{
    error::Error,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use compiler_ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityId,
    EntityVersion, FactAvailability, IrBuilder, ItemKind, LinkKind, OccurrenceAuthorityFacts,
    ParentageAuthority, SemanticImageIdentity, SemanticImageView, SourceSpan, TreeEntityId,
    TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
};
use futures_core::Stream;
use backend_version::{
    ArtifactId, CompilePublicationDomain, ContentId, GenerationId, IrFragmentDomain,
    IrFragmentEncoding,
};
use interface_protocol::{UntrustedDocumentId, UntrustedSourceSpan};
use server_index_core::{
    EntityArtifactIdentity, EntityDocumentId, IndexSnapshot, LexicalOperation, LexicalRow,
    LexicalScore, LexicalSegment,
};
use server_index_graph_vector::Cancellation;
use server_index_retrieval::{
    CanonicalOccurrenceSource, CanonicalSource, CanonicalSourceError,
    OwnedCanonicalOccurrenceSource, VerifiedSourceImage, resolve_occurrence_source,
    resolve_occurrence_sources, resolve_tantivy_source, resolve_tantivy_sources,
};
use server_index_tantivy::{TantivySegment, TantivySegmentHit, TantivySegmentStore};
use server_index_trustfall::SemanticTrustfallGraph;
use server_index_vocabulary::{
    IndexLocatorFacts, IndexSnapshotId, SemanticImageExtent, SemanticImageLocator,
    VerifiedSemanticPublication,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!(
            "nudox-retrieval-source-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    const fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct DurableFixture {
    _root: TestRoot,
    store: TantivySegmentStore,
    segment: TantivySegment,
    snapshot: IndexSnapshotId,
}

impl DurableFixture {
    fn new(document: EntityDocumentId) -> TestResult<Self> {
        let root = TestRoot::new()?;
        let store = TantivySegmentStore::open(root.path())?;
        let rows = [LexicalRow::new(b"needle", document, LexicalScore::from(1))];
        let lexical = LexicalSegment::new(&rows)
            .map_err(|_| std::io::Error::other("source fixture lexical segment was rejected"))?;
        let segment = store.project(lexical)?;
        let selection = [segment.id()];
        let snapshot = IndexSnapshot::new(
            GenerationId::from_canonical_bytes(b"source-resolution-generation"),
            &[],
            &selection,
        )?;
        Ok(Self {
            _root: root,
            store,
            segment,
            snapshot: snapshot.id,
        })
    }

    fn hit(&self) -> TestResult<TantivySegmentHit<'_>> {
        let selection = [self.segment.id()];
        let snapshot = IndexSnapshot::new(
            GenerationId::from_canonical_bytes(b"source-resolution-generation"),
            &[],
            &selection,
        )?;
        if snapshot.id != self.snapshot {
            return Err("durable fixture changed its pinned snapshot".into());
        }
        let segments = [&self.segment];
        let pinned = self.store.compose(snapshot, &segments)?;
        let mut output = [None];
        let mut candidates = [None; 8];
        let written = pinned.search(
            LexicalOperation::new(b"needle"),
            1,
            &mut output,
            &mut candidates,
        )?;
        if written != 1 {
            return Err("durable fixture did not return its membership hit".into());
        }
        output
            .first()
            .copied()
            .flatten()
            .ok_or_else(|| std::io::Error::other("durable fixture left its hit slot empty").into())
    }
}

struct ImageFixture {
    bytes: Vec<u8>,
    locator: SemanticImageLocator,
}

fn image_fixture(path: &[u8], source: Option<(u32, u32)>, seed: u8) -> TestResult<ImageFixture> {
    let mut builder = IrBuilder::new();
    let file = builder.intern_atom(path)?;
    let version = EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    };
    let source = source.and_then(|(start, end)| SourceSpan::new(file, start, end));
    let availability = if source.is_some() {
        FactAvailability::Captured
    } else {
        FactAvailability::Unavailable
    };
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        source: availability,
        source_file: availability,
        ..EntityAuthorityFacts::default()
    };
    let item = TreeItemInput {
        name: b"entry",
        kind: ItemKind::Module,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source,
        extension: None,
    };
    let mut builder = builder;
    builder.add_borrowed_tree(BorrowedTree {
        versions: &[version],
        items: &[item],
        links: &[],
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    let image = SemanticImageView::reopen(&bytes)?;
    let byte_length = u32::try_from(bytes.len())?;
    Ok(ImageFixture {
        locator: SemanticImageLocator::new(
            SemanticImageIdentity::from_encoded_bytes(image.as_ref()),
            SemanticImageExtent::new(0, byte_length)
                .map_err(|_| std::io::Error::other("source fixture extent was rejected"))?,
        ),
        bytes,
    })
}

fn occurrence_image_fixture(source_availability: FactAvailability) -> TestResult<ImageFixture> {
    let mut builder = IrBuilder::new();
    let file = builder.intern_atom(b"occurrence/\xff-source.rs")?;
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let versions = [
        EntityVersion {
            family: DeclarationFamilyId::from_raw([21; 16]),
            variant: VariantFingerprint::from_raw([22; 16]),
            core_payload: CorePayloadHash::from_raw([23; 16]),
        },
        EntityVersion {
            family: DeclarationFamilyId::from_raw([24; 16]),
            variant: VariantFingerprint::from_raw([25; 16]),
            core_payload: CorePayloadHash::from_raw([26; 16]),
        },
    ];
    let items = [b"from".as_slice(), b"to".as_slice()].map(|name| TreeItemInput {
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
    let links = [
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: compiler_ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: source_availability,
            },
            source: if source_availability == FactAvailability::Captured {
                SourceSpan::new(file, 4, 7)
            } else {
                None
            },
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: compiler_ir::Confidence::Indexed,
            authority: OccurrenceAuthorityFacts {
                source: source_availability,
            },
            source: if source_availability == FactAvailability::Captured {
                SourceSpan::new(file, 9, 12)
            } else {
                None
            },
        },
    ];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    let image = SemanticImageView::reopen(&bytes)?;
    Ok(ImageFixture {
        locator: SemanticImageLocator::new(
            SemanticImageIdentity::from_encoded_bytes(image.as_ref()),
            SemanticImageExtent::new(0, u32::try_from(bytes.len())?)
                .map_err(|_| std::io::Error::other("occurrence fixture extent was rejected"))?,
        ),
        bytes,
    })
}

const fn document(image: &ImageFixture, entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Semantic(image.locator.identity),
        entity: EntityId::new(entity),
    }
}

fn compact_document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        artifact: EntityArtifactIdentity::Compact(
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                b"compact-source-fixture",
            ),
        ),
        entity: EntityId::new(entity),
    }
}

fn publication(
    image: &SemanticImageView<'_>,
    fixture: &ImageFixture,
    snapshot: IndexSnapshotId,
) -> TestResult<VerifiedSemanticPublication> {
    let facts = IndexLocatorFacts::new(
        GenerationId::from_canonical_bytes(b"source-resolution-generation"),
        snapshot,
        ContentId::<CompilePublicationDomain>::from_canonical_bytes(
            b"source-resolution-publication",
        ),
    );
    VerifiedSemanticPublication::verify_reopened(facts, fixture.locator, image)
        .map_err(|_| std::io::Error::other("source fixture publication was rejected").into())
}

const fn source_stamp(
    source: &CanonicalSource<'_, '_>,
) -> (EntityDocumentId, u32, u32, *const u8, usize) {
    (
        source.document(),
        source.span().start(),
        source.span().end(),
        source.path().as_ptr(),
        source.path().len(),
    )
}

fn source_pair<'image, 'bytes>(
    publication: VerifiedSemanticPublication,
    image: &'image SemanticImageView<'bytes>,
    hit: TantivySegmentHit<'_>,
) -> Result<[Option<CanonicalSource<'image, 'bytes>>; 2], CanonicalSourceError> {
    Ok([
        Some(resolve_tantivy_source(publication, image, hit)?),
        Some(resolve_tantivy_source(publication, image, hit)?),
    ])
}

fn source_stamps<const LENGTH: usize>(
    sources: &[Option<CanonicalSource<'_, '_>>; LENGTH],
) -> [Option<(EntityDocumentId, u32, u32, *const u8, usize)>; LENGTH] {
    sources
        .each_ref()
        .map(|slot| slot.as_ref().map(source_stamp))
}

#[test]
fn resolves_durable_hit_against_reopened_non_utf8_source_and_retains_provenance() -> TestResult {
    let fixture = image_fixture(b"src/\xff-entry.rs", Some((3, 7)), 1)?;
    let durable = DurableFixture::new(document(&fixture, 0))?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = publication(&image, &fixture, durable.snapshot)?;
    let hit = durable.hit()?;

    let source = resolve_tantivy_source(publication, &image, hit)?;
    assert_eq!(source.path(), b"src/\xff-entry.rs");
    assert_eq!((source.span().start(), source.span().end()), (3, 7));
    assert_eq!(source.document(), hit.provenance().document());
    assert_eq!(source.provenance(), hit.provenance());
    assert_eq!(source.publication(), publication);
    Ok(())
}

#[test]
fn captured_empty_span_remains_captured_not_unavailable() -> TestResult {
    let fixture = image_fixture(b"empty.rs", Some((9, 9)), 2)?;
    let durable = DurableFixture::new(document(&fixture, 0))?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = publication(&image, &fixture, durable.snapshot)?;

    let source = resolve_tantivy_source(publication, &image, durable.hit()?)?;
    assert_eq!(source.span().start(), 9);
    assert_eq!(source.span().end(), 9);
    assert_eq!(source.path(), b"empty.rs");
    Ok(())
}

#[test]
fn only_a_canonical_source_can_materialize_owned_client_reply_facts() -> TestResult {
    let fixture = image_fixture(b"src/\xff-owned.rs", Some((5, 11)), 8)?;
    let durable = DurableFixture::new(document(&fixture, 0))?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = publication(&image, &fixture, durable.snapshot)?;
    let source = resolve_tantivy_source(publication, &image, durable.hit()?)?;
    let expected_document = source.document();
    let expected_snapshot = source.publication().authority().snapshot;

    let owned = source.into_owned_canonical_source()?;
    assert_eq!(owned.path(), b"src/\xff-owned.rs");
    assert_eq!((owned.start(), owned.end()), (5, 11));
    assert_eq!(owned.snapshot(), expected_snapshot);
    assert_eq!(owned.document(), expected_document);
    assert_eq!(owned.publication(), publication);
    assert_eq!(owned.image(), fixture.locator.identity);
    let (path, start, end, snapshot, document) = owned.into_transport_parts();
    let document: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = document.into();
    let wire = UntrustedSourceSpan::new(
        path,
        start,
        end,
        snapshot,
        UntrustedDocumentId::from_fixed_bytes(document),
    )?;
    let encoded = serde_json::to_vec(&wire)?;
    let round_trip: UntrustedSourceSpan = serde_json::from_slice(&encoded)?;
    assert_eq!(round_trip.path(), b"src/\xff-owned.rs");
    assert_eq!((round_trip.start(), round_trip.end()), (5, 11));
    assert_eq!(round_trip.snapshot(), expected_snapshot);
    let document: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = expected_document.into();
    assert_eq!(round_trip.document().as_bytes(), &document);
    Ok(())
}

#[test]
fn occurrence_candidate_is_borrowed_and_bound_to_verified_publication() -> TestResult {
    let fixture = occurrence_image_fixture(FactAvailability::Captured)?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"occurrence-source-snapshot");
    let publication = publication(&image, &fixture, snapshot)?;
    let source_image = VerifiedSourceImage::verify(publication, &image)?;
    let [first, second] = {
        let cancellation = Cancellation::new();
        let graph = SemanticTrustfallGraph::new(&image, &cancellation);
        let mut occurrences = graph.occurrence_neighbors(EntityId::new(0))?;
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(first))) = Pin::new(&mut occurrences).poll_next(&mut context)
        else {
            return Err("occurrence fixture did not emit its first candidate".into());
        };
        let Poll::Ready(Some(Ok(second))) = Pin::new(&mut occurrences).poll_next(&mut context)
        else {
            return Err("occurrence fixture did not emit its second candidate".into());
        };
        [first, second]
    };
    let resolved = [
        resolve_occurrence_source(publication, first)?,
        resolve_occurrence_source(publication, second)?,
    ];
    assert!(resolved.iter().all(|source| {
        source.publication() == publication
            && source.entity() == EntityId::new(1)
            && source.provenance().path() == Some(&b"occurrence/\xff-source.rs"[..])
    }));
    let [first_resolved, second_resolved] = resolved;
    assert_eq!(first_resolved.link(), second_resolved.link());
    assert_ne!(first_resolved.occurrence(), second_resolved.occurrence());
    let spans = resolved.map(|source| {
        source
            .provenance()
            .source()
            .map(|span| (span.start(), span.end()))
    });
    assert!(spans.contains(&Some((4, 7))));
    assert!(spans.contains(&Some((9, 12))));

    let owned = first_resolved.into_owned_canonical_occurrence_source()?;
    assert!(matches!(
        owned,
        OwnedCanonicalOccurrenceSource::Captured(ref span)
            if span.path() == b"occurrence/\xff-source.rs"
                && [(4, 7), (9, 12)].contains(&(span.start(), span.end()))
                && span.snapshot() == snapshot
    ));

    let candidates = [first, second];
    let mut scratch = [None; 2];
    let mut output = [None; 2];
    assert_eq!(
        resolve_occurrence_sources(source_image, &candidates, &mut scratch, &mut output)?,
        2
    );
    let output_spans = output.map(|source| {
        source.and_then(|source| {
            source
                .provenance()
                .source()
                .map(|span| (span.start(), span.end()))
        })
    });
    assert!(output_spans.contains(&Some((4, 7))));
    assert!(output_spans.contains(&Some((9, 12))));

    let foreign_image = SemanticImageView::reopen(&fixture.bytes)?;
    let foreign = {
        let cancellation = Cancellation::new();
        let graph = SemanticTrustfallGraph::new(&foreign_image, &cancellation);
        let mut occurrences = graph.occurrence_neighbors(EntityId::new(0))?;
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(hit))) = Pin::new(&mut occurrences).poll_next(&mut context) else {
            return Err("foreign occurrence fixture did not emit a candidate".into());
        };
        hit
    };
    let sentinel = source_image.resolve_occurrence(first)?;
    let mut failed_output = [Some(sentinel), Some(sentinel)];
    let before = failed_output.map(|source| source.map(CanonicalOccurrenceSource::occurrence));
    let foreign_candidates = [foreign];
    let mut failure_scratch = [None];
    assert!(matches!(
        resolve_occurrence_sources(
            source_image,
            &foreign_candidates,
            &mut failure_scratch,
            &mut failed_output,
        ),
        Err(CanonicalSourceError::OccurrenceMismatch { .. })
    ));
    assert_eq!(
        failed_output.map(|source| source.map(CanonicalOccurrenceSource::occurrence)),
        before
    );
    Ok(())
}

#[test]
fn owned_occurrence_source_retains_proven_unavailable_state() -> TestResult {
    let fixture = occurrence_image_fixture(FactAvailability::Unavailable)?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"occurrence-source-unavailable");
    let publication = publication(&image, &fixture, snapshot)?;
    let candidate = {
        let cancellation = Cancellation::new();
        let graph = SemanticTrustfallGraph::new(&image, &cancellation);
        let mut occurrences = graph.occurrence_neighbors(EntityId::new(0))?;
        let mut context = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(candidate))) = Pin::new(&mut occurrences).poll_next(&mut context)
        else {
            return Err("unavailable occurrence fixture did not emit a candidate".into());
        };
        candidate
    };

    let owned = resolve_occurrence_source(publication, candidate)?
        .into_owned_canonical_occurrence_source()?;
    assert!(matches!(
        owned,
        OwnedCanonicalOccurrenceSource::Unavailable(ref unavailable)
            if unavailable.snapshot() == snapshot
                && unavailable.publication() == publication
                && unavailable.entity() == EntityId::new(1)
    ));
    Ok(())
}

#[test]
fn selection_container_can_expire_while_reopened_image_source_stays_borrowed() -> TestResult {
    let fixture = image_fixture(b"borrow.rs", Some((1, 2)), 3)?;
    let durable = DurableFixture::new(document(&fixture, 0))?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = publication(&image, &fixture, durable.snapshot)?;
    let source = {
        let hit = durable.hit()?;
        let selected = [Some(hit)];
        let mut scratch = [None];
        let mut output = [None];
        assert_eq!(
            resolve_tantivy_sources(publication, &image, &selected, &mut scratch, &mut output)?,
            1
        );
        output
            .into_iter()
            .next()
            .flatten()
            .ok_or_else(|| std::io::Error::other("source output was unexpectedly absent"))?
    };
    assert_eq!(source.path(), b"borrow.rs");
    assert_eq!((source.span().start(), source.span().end()), (1, 2));
    Ok(())
}

#[test]
fn every_preflight_failure_preserves_all_output_slots() -> TestResult {
    let fixture = image_fixture(b"valid.rs", Some((1, 4)), 4)?;
    let durable = DurableFixture::new(document(&fixture, 0))?;
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = publication(&image, &fixture, durable.snapshot)?;
    let hit = durable.hit()?;
    let mut output = source_pair(publication, &image, hit)?;
    let original = source_stamps(&output);
    let mut scratch = [const { None }; 2];
    let missing = [Some(hit), None];
    assert_eq!(
        resolve_tantivy_sources(publication, &image, &missing, &mut scratch, &mut output),
        Err(CanonicalSourceError::MissingHit { slot: 1 })
    );
    assert_eq!(source_stamps(&output), original);

    let mut output = source_pair(publication, &image, hit)?;
    let original = source_stamps(&output);
    let mut short_scratch = [];
    assert_eq!(
        resolve_tantivy_sources(
            publication,
            &image,
            &[Some(hit)],
            &mut short_scratch,
            &mut output
        ),
        Err(CanonicalSourceError::ScratchTooSmall {
            required: 1,
            available: 0,
        })
    );
    assert_eq!(source_stamps(&output), original);

    let mut output = [];
    let mut scratch = [None];
    assert_eq!(
        resolve_tantivy_sources(publication, &image, &[Some(hit)], &mut scratch, &mut output),
        Err(CanonicalSourceError::InsufficientOutput {
            required: 1,
            available: 0,
        })
    );
    Ok(())
}

#[test]
fn rejects_wrong_publication_image_entity_and_unavailable_source_without_output_mutation()
-> TestResult {
    let valid_fixture = image_fixture(b"valid.rs", Some((2, 5)), 5)?;
    let durable = DurableFixture::new(document(&valid_fixture, 0))?;
    let valid_image = SemanticImageView::reopen(&valid_fixture.bytes)?;
    let valid_publication = publication(&valid_image, &valid_fixture, durable.snapshot)?;
    let hit = durable.hit()?;
    let mut output = source_pair(valid_publication, &valid_image, hit)?;
    let original = source_stamps(&output);
    let mut scratch = [None];
    let wrong_snapshot = IndexSnapshotId::from_canonical_bytes(b"another-source-snapshot");
    let wrong_publication = publication(&valid_image, &valid_fixture, wrong_snapshot)?;
    assert_eq!(
        resolve_tantivy_sources(
            wrong_publication,
            &valid_image,
            &[Some(hit)],
            &mut scratch,
            &mut output,
        ),
        Err(CanonicalSourceError::WrongPublication)
    );
    assert_eq!(source_stamps(&output), original);

    let foreign_fixture = image_fixture(b"foreign.rs", Some((2, 5)), 6)?;
    let foreign_image = SemanticImageView::reopen(&foreign_fixture.bytes)?;
    let foreign_publication = publication(&foreign_image, &foreign_fixture, durable.snapshot)?;
    let mut scratch = [None];
    assert_eq!(
        resolve_tantivy_sources(
            foreign_publication,
            &foreign_image,
            &[Some(hit)],
            &mut scratch,
            &mut output,
        ),
        Err(CanonicalSourceError::WrongPublication)
    );
    assert_eq!(source_stamps(&output), original);

    let absent_durable = DurableFixture::new(document(&valid_fixture, 1))?;
    let absent_hit = absent_durable.hit()?;
    let absent_publication = publication(&valid_image, &valid_fixture, absent_durable.snapshot)?;
    let mut scratch = [None];
    assert_eq!(
        resolve_tantivy_sources(
            absent_publication,
            &valid_image,
            &[Some(absent_hit)],
            &mut scratch,
            &mut output,
        ),
        Err(CanonicalSourceError::EntityAbsent { entity: 1 })
    );
    assert_eq!(source_stamps(&output), original);

    let compact_durable = DurableFixture::new(compact_document(0))?;
    let compact_hit = compact_durable.hit()?;
    let compact_publication = publication(&valid_image, &valid_fixture, compact_durable.snapshot)?;
    let mut compact_scratch = [None];
    assert_eq!(
        resolve_tantivy_sources(
            compact_publication,
            &valid_image,
            &[Some(compact_hit)],
            &mut compact_scratch,
            &mut output,
        ),
        Err(CanonicalSourceError::CompactArtifact)
    );
    assert_eq!(source_stamps(&output), original);

    let unavailable_fixture = image_fixture(b"unavailable.rs", None, 7)?;
    let unavailable_durable = DurableFixture::new(document(&unavailable_fixture, 0))?;
    let unavailable_image = SemanticImageView::reopen(&unavailable_fixture.bytes)?;
    let unavailable_publication = publication(
        &unavailable_image,
        &unavailable_fixture,
        unavailable_durable.snapshot,
    )?;
    let unavailable_hit = unavailable_durable.hit()?;
    let unavailable =
        resolve_tantivy_source(unavailable_publication, &unavailable_image, unavailable_hit);
    assert!(matches!(
        unavailable,
        Err(CanonicalSourceError::SourceUnavailable { entity: 0 })
    ));
    let mut unavailable_scratch = [None];
    assert_eq!(
        resolve_tantivy_sources(
            unavailable_publication,
            &unavailable_image,
            &[Some(unavailable_hit)],
            &mut unavailable_scratch,
            &mut output,
        ),
        Err(CanonicalSourceError::SourceUnavailable { entity: 0 })
    );
    assert_eq!(source_stamps(&output), original);
    Ok(())
}
