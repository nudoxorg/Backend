//! Hostile public journeys for the durable Turso locator catalog.
#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use core::fmt::{Display, Formatter};
use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};

use compiler_ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
    FactAvailability, IrBuilder, ItemKind, PackageLineage, ParentageAuthority,
    SemanticImageIdentity, SemanticImageView, SemanticReader, TreeItemInput, VariantFingerprint,
    Visibility, encode_full_semantic_image, full_semantic_image_len,
};
use futures_executor::block_on;
use heart_identity::{CompilePublicationDomain, ContentId, GenerationId};
use server_index_catalog::{
    CatalogError, CatalogPageError, CatalogPageOperation, CatalogPublication,
    CatalogPublishOutcome, FeedCheckpoint, FeedContentChecksum, FeedIdentity, FeedObservation,
    TursoCatalog,
};
use server_index_ingest::Checkpoint;
use server_index_vocabulary::{
    CanonicalEntityLocator, IndexLocatorFacts, IndexSnapshotId, PackageCoordinate, PackageVersion,
    SemanticImageExtent, SemanticImageLocator, VerifiedCanonicalEntityLocator,
    VerifiedSemanticPublication,
};

#[derive(Debug)]
struct TestFailure(&'static str);
impl Display for TestFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

#[test]
fn feed_checkpoints_are_independent_and_survive_reopen() -> TestResult {
    block_on(async {
        let database = temp_catalog("independent-feeds")?;
        let left = FeedIdentity::new("crates.io/sparse/left")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let right = FeedIdentity::new("crates.io/sparse/right")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let left_current = FeedCheckpoint::initial(left.clone());
        let right_current = FeedCheckpoint::initial(right.clone());
        let left_next = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            ..left_current.clone()
        };
        let right_next = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            ..right_current.clone()
        };
        {
            let mut catalog = TursoCatalog::open(&database.path).await?;
            assert_eq!(
                catalog
                    .apply_feed_page(&left_current, &left_next, &[])
                    .await?,
                left_next
            );
            assert_eq!(catalog.feed_checkpoint(right.clone()).await?, right_current);
            assert_eq!(
                catalog
                    .apply_feed_page(&right_current, &right_next, &[])
                    .await?,
                right_next
            );
        }
        let catalog = TursoCatalog::open(&database.path).await?;
        assert_eq!(catalog.feed_checkpoint(left).await?, left_next);
        assert_eq!(catalog.feed_checkpoint(right).await?, right_next);
        Ok(())
    })
}
impl Error for TestFailure {}
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn unexpected(error: impl Error + 'static) -> Box<dyn Error> {
    Box::new(error)
}

struct ImageFixture {
    bytes: Vec<u8>,
    locator: SemanticImageLocator,
    entities: Vec<VerifiedCanonicalEntityLocator>,
}
struct TempCatalog {
    path: String,
}
impl Drop for TempCatalog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path, suffix));
        }
    }
}
fn temp_catalog(label: &str) -> Result<TempCatalog, TestFailure> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "nudox-index-catalog-{label}-{}-{nonce}.db",
        std::process::id()
    ));
    Ok(TempCatalog {
        path: path
            .to_str()
            .ok_or(TestFailure("temporary catalog path was not UTF-8"))?
            .to_owned(),
    })
}
fn lineage() -> Result<PackageLineage<'static>, TestFailure> {
    PackageLineage::new("cargo", "catalog-fixture")
        .map_err(|_| TestFailure("fixture package lineage was rejected"))
}
fn package(version: &'static str) -> Result<PackageCoordinate<'static>, TestFailure> {
    Ok(PackageCoordinate::new(
        lineage()?,
        PackageVersion::new(version)
            .map_err(|_| TestFailure("fixture package version was rejected"))?,
    ))
}
fn authority(seed: u8) -> IndexLocatorFacts {
    IndexLocatorFacts::new(
        GenerationId::from_digest([seed; 32]),
        IndexSnapshotId::from_canonical_bytes(&[seed; 32]),
        ContentId::<CompilePublicationDomain>::from_canonical_bytes(&[seed.wrapping_add(1); 32]),
    )
}
fn version(seed: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    }
}
fn entity_authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}
fn image_fixture(seed: u8) -> TestResult<ImageFixture> {
    let versions = [version(seed), version(seed.wrapping_add(1))];
    let authority = entity_authority();
    let items = [b"alpha".as_slice(), b"beta".as_slice()].map(|name| TreeItemInput {
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
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    let image = SemanticImageView::reopen(&bytes)?;
    let extent = SemanticImageExtent::new(
        0,
        u32::try_from(bytes.len()).map_err(|_| TestFailure("fixture image is too large"))?,
    )
    .map_err(|_| TestFailure("fixture image extent was rejected"))?;
    let locator =
        SemanticImageLocator::new(SemanticImageIdentity::from_encoded_bytes(&bytes), extent);
    let mut entities = Vec::new();
    for (ordinal, entity) in image.canonical_entities().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| TestFailure("fixture entity ordinal is too large"))?;
        let locator = CanonicalEntityLocator::new(locator, ordinal, entity.version.identity());
        entities.push(
            locator
                .verify_reopened(&image)
                .map_err(|_| TestFailure("fixture entity locator was not verified"))?,
        );
    }
    Ok(ImageFixture {
        bytes,
        locator,
        entities,
    })
}

fn entity_pair(
    fixture: &ImageFixture,
) -> Result<
    (
        VerifiedCanonicalEntityLocator,
        VerifiedCanonicalEntityLocator,
    ),
    TestFailure,
> {
    let mut entities = fixture.entities.iter().copied();
    let first = entities
        .next()
        .ok_or(TestFailure("fixture did not contain a first entity"))?;
    let second = entities
        .next()
        .ok_or(TestFailure("fixture did not contain a second entity"))?;
    if entities.next().is_some() {
        return Err(TestFailure("fixture contained more than two entities"));
    }
    Ok((first, second))
}

fn verify_returned_entities(
    fixture: &ImageFixture,
    entities: &[CanonicalEntityLocator],
) -> TestResult<Vec<VerifiedCanonicalEntityLocator>> {
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    entities
        .iter()
        .copied()
        .map(|locator| {
            locator.verify_reopened(&image).map_err(|_| {
                TestFailure("returned entity locator did not verify against bytes").into()
            })
        })
        .collect()
}
fn publication<'a>(
    package: PackageCoordinate<'a>,
    fixture: &'a ImageFixture,
    facts: IndexLocatorFacts,
) -> TestResult<CatalogPublication<'a>> {
    let image = SemanticImageView::reopen(&fixture.bytes)?;
    let publication = VerifiedSemanticPublication::verify_reopened(facts, fixture.locator, &image)
        .map_err(|_| TestFailure("fixture semantic publication did not verify"))?;
    Ok(CatalogPublication {
        package,
        publication,
        entities: &fixture.entities,
    })
}

#[test]
fn persistence_survives_close_and_reopen() -> TestResult {
    block_on(async {
        let database = temp_catalog("persistence")?;
        let fixture = image_fixture(11)?;
        let coordinate = package("1.0.0")?;
        {
            let mut catalog = TursoCatalog::open(&database.path).await?;
            assert!(matches!(
                catalog
                    .publish(publication(coordinate, &fixture, authority(11))?)
                    .await?,
                CatalogPublishOutcome::Inserted { .. }
            ));
        }
        let mut reopened = TursoCatalog::open(&database.path).await?;
        let record = reopened
            .resolve(coordinate.lineage, Some(coordinate.version))
            .await?;
        let verified = verify_returned_entities(&fixture, &record.entities)?;
        assert_eq!(verified, fixture.entities);
        Ok(())
    })
}

#[test]
fn two_versions_make_unversioned_resolution_ambiguous() -> TestResult {
    block_on(async {
        let database = temp_catalog("ambiguity")?;
        let first = package("1.0.0")?;
        let second = package("2.0.0")?;
        let first_image = image_fixture(21)?;
        let second_image = image_fixture(22)?;
        let mut catalog = TursoCatalog::open(&database.path).await?;
        catalog
            .publish(publication(first, &first_image, authority(21))?)
            .await?;
        catalog
            .publish(publication(second, &second_image, authority(22))?)
            .await?;
        match catalog.resolve(first.lineage, None).await {
            Err(CatalogError::Ambiguous) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("ambiguous lineage resolved").into()),
        }
        let _versioned = catalog.resolve(first.lineage, Some(first.version)).await?;
        Ok(())
    })
}

#[test]
fn identical_image_is_unchanged_but_changed_image_appends_history() -> TestResult {
    block_on(async {
        let database = temp_catalog("history")?;
        let coordinate = package("3.0.0")?;
        let original = image_fixture(31)?;
        let changed = image_fixture(32)?;
        let mut catalog = TursoCatalog::open(&database.path).await?;
        let first_sequence = match catalog
            .publish(publication(coordinate, &original, authority(31))?)
            .await?
        {
            CatalogPublishOutcome::Inserted { sequence } => sequence,
            CatalogPublishOutcome::Unchanged { .. } => {
                return Err(TestFailure("first catalog publication was unchanged").into());
            }
        };
        match catalog
            .publish(publication(coordinate, &original, authority(31))?)
            .await?
        {
            CatalogPublishOutcome::Unchanged { sequence } => assert_eq!(sequence, first_sequence),
            CatalogPublishOutcome::Inserted { .. } => {
                return Err(TestFailure("identical catalog publication was inserted").into());
            }
        }
        match catalog
            .publish(publication(coordinate, &changed, authority(32))?)
            .await?
        {
            CatalogPublishOutcome::Inserted { sequence } => assert_ne!(sequence, first_sequence),
            CatalogPublishOutcome::Unchanged { .. } => {
                return Err(TestFailure("changed catalog image was unchanged").into());
            }
        }
        assert_eq!(catalog.history(coordinate).await?.len(), 2);
        Ok(())
    })
}

#[test]
fn returning_to_an_earlier_image_appends_an_immutable_head() -> TestResult {
    block_on(async {
        let database = temp_catalog("rollback-image")?;
        let coordinate = package("3.0.1")?;
        let first = image_fixture(71)?;
        let second = image_fixture(72)?;
        let mut catalog = TursoCatalog::open(&database.path).await?;
        for (fixture, facts) in [
            (&first, authority(71)),
            (&second, authority(72)),
            (&first, authority(71)),
        ] {
            match catalog
                .publish(publication(coordinate, fixture, facts)?)
                .await?
            {
                CatalogPublishOutcome::Inserted { .. } => {}
                CatalogPublishOutcome::Unchanged { .. } => {
                    return Err(TestFailure("non-current image was unchanged").into());
                }
            }
        }
        let history = catalog.history(coordinate).await?;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].image, history[2].image);
        assert_ne!(history[0].sequence, history[2].sequence);
        Ok(())
    })
}

#[test]
fn authority_only_rebind_is_history_and_image_immutable() -> TestResult {
    block_on(async {
        let database = temp_catalog("rebind")?;
        let coordinate = package("3.1.0")?;
        let fixture = image_fixture(33)?;
        let mut catalog = TursoCatalog::open(&database.path).await?;
        catalog
            .publish(publication(coordinate, &fixture, authority(33))?)
            .await?;
        assert!(matches!(
            catalog
                .publish(publication(coordinate, &fixture, authority(34))?)
                .await?,
            CatalogPublishOutcome::Inserted { .. }
        ));
        let history = catalog.history(coordinate).await?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].image, history[1].image);
        assert_ne!(history[0].authority, history[1].authority);
        Ok(())
    })
}

#[test]
fn nonempty_reopened_image_rejects_an_incomplete_entity_set() -> TestResult {
    block_on(async {
        let database = temp_catalog("entity-rebind")?;
        let coordinate = package("3.1.1")?;
        let fixture = image_fixture(73)?;
        let entity = fixture
            .entities
            .first()
            .copied()
            .ok_or(TestFailure("fixture did not contain an entity"))?;
        let narrowed = [entity];
        let mut catalog = TursoCatalog::open(&database.path).await?;
        catalog
            .publish(publication(coordinate, &fixture, authority(73))?)
            .await?;
        match catalog
            .publish(CatalogPublication {
                entities: &narrowed,
                ..publication(coordinate, &fixture, authority(73))?
            })
            .await
        {
            Err(CatalogError::EntityCountMismatch {
                expected: 2,
                observed: 1,
            }) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("incomplete entity set was persisted").into()),
        }
        assert_eq!(catalog.history(coordinate).await?.len(), 1);
        Ok(())
    })
}

#[test]
fn invalid_entity_image_and_duplicate_entity_roll_back() -> TestResult {
    block_on(async {
        let database = temp_catalog("preflight")?;
        let coordinate = package("4.0.0")?;
        let fixture = image_fixture(41)?;
        let mut catalog = TursoCatalog::open(&database.path).await?;
        catalog
            .publish(publication(coordinate, &fixture, authority(41))?)
            .await?;
        let before = catalog.history(coordinate).await?;
        let (entity, second_entity) = entity_pair(&fixture)?;
        let duplicate_entities = [entity, entity];
        let duplicate = CatalogPublication {
            entities: &duplicate_entities,
            ..publication(coordinate, &fixture, authority(41))?
        };
        match catalog.publish(duplicate).await {
            Err(CatalogError::DuplicateEntity) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("duplicate entity was persisted").into()),
        }
        let wrong_image = image_fixture(42)?;
        let mismatch = CatalogPublication {
            entities: &fixture.entities,
            ..publication(coordinate, &wrong_image, authority(41))?
        };
        match catalog.publish(mismatch).await {
            Err(CatalogError::EntityImageMismatch) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("cross-image entity was persisted").into()),
        }
        let reversed = [second_entity, entity];
        match catalog
            .publish(CatalogPublication {
                entities: &reversed,
                ..publication(coordinate, &fixture, authority(41))?
            })
            .await
        {
            Err(CatalogError::EntityOutOfOrder { .. }) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("out-of-order entities were persisted").into()),
        }
        assert_eq!(catalog.history(coordinate).await?, before);
        Ok(())
    })
}

#[test]
fn corrupt_reopen_is_a_typed_failure_and_locator_facts_survive() -> TestResult {
    block_on(async {
        let database = temp_catalog("corrupt")?;
        let coordinate = package("5.0.0")?;
        let fixture = image_fixture(51)?;
        let facts = authority(51);
        {
            let mut catalog = TursoCatalog::open(&database.path).await?;
            catalog
                .publish(publication(coordinate, &fixture, facts)?)
                .await?;
            let record = catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await?;
            assert_eq!(record.authority, facts);
            assert_eq!(record.image, fixture.locator);
            let expected: Vec<_> = fixture
                .entities
                .iter()
                .copied()
                .map(VerifiedCanonicalEntityLocator::as_locator)
                .collect();
            assert_eq!(record.entities, expected);
        }
        std::fs::write(&database.path, b"not a sqlite database")?;
        match TursoCatalog::open(&database.path).await {
            Err(CatalogError::Database(_)) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("corrupted database reopened").into()),
        }
        Ok(())
    })
}

#[test]
fn a_second_writer_is_not_silently_admitted() -> TestResult {
    block_on(async {
        let database = temp_catalog("writer")?;
        let mut first = TursoCatalog::open(&database.path).await?;
        let mut second = TursoCatalog::open(&database.path).await?;
        let fixture = image_fixture(61)?;
        let coordinate = package("6.0.0")?;
        match first
            .publish(publication(package("6.0.0")?, &fixture, authority(61))?)
            .await?
        {
            CatalogPublishOutcome::Inserted { .. } => {}
            CatalogPublishOutcome::Unchanged { .. } => {
                return Err(TestFailure("first writer was unchanged").into());
            }
        }
        match second
            .publish(publication(coordinate, &fixture, authority(61))?)
            .await?
        {
            CatalogPublishOutcome::Unchanged { .. } => {}
            CatalogPublishOutcome::Inserted { .. } => {
                return Err(TestFailure("second writer appended duplicate facts").into());
            }
        }
        Ok(())
    })
}

#[test]
fn checkpointed_page_is_atomic_stale_safe_and_persists() -> TestResult {
    block_on(async {
        let database = temp_catalog("checkpoint")?;
        let coordinate = package("7.0.0")?;
        let fixture = image_fixture(81)?;
        let initial = Checkpoint {
            sequence: 0,
            page: 0,
        };
        let next = Checkpoint {
            sequence: 0,
            page: 1,
        };
        {
            let mut catalog = TursoCatalog::open(&database.path).await?;
            let operations = [CatalogPageOperation::Publish(publication(
                coordinate,
                &fixture,
                authority(81),
            )?)];
            assert_eq!(catalog.apply_page(initial, next, &operations).await?, next);
            assert_eq!(catalog.checkpoint().await?, next);
            match catalog
                .apply_page(
                    initial,
                    Checkpoint {
                        sequence: 0,
                        page: 2,
                    },
                    &[],
                )
                .await
            {
                Err(CatalogPageError::Stale { stored, current }) => {
                    assert_eq!(stored, next);
                    assert_eq!(current, initial);
                }
                Err(error) => return Err(unexpected(error)),
                Ok(_) => return Err(TestFailure("stale page advanced").into()),
            }
            assert_eq!(
                catalog
                    .resolve(coordinate.lineage, Some(coordinate.version))
                    .await?
                    .image,
                fixture.locator
            );
            let removal = [CatalogPageOperation::Remove(coordinate)];
            let removed = Checkpoint {
                sequence: 1,
                page: 0,
            };
            assert_eq!(catalog.apply_page(next, removed, &removal).await?, removed);
            match catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await
            {
                Err(CatalogError::NotFound) => {}
                Err(error) => return Err(unexpected(error)),
                Ok(_) => return Err(TestFailure("removed current row resolved").into()),
            }
        }
        let reopened = TursoCatalog::open(&database.path).await?;
        assert_eq!(
            reopened.checkpoint().await?,
            Checkpoint {
                sequence: 1,
                page: 0
            }
        );
        drop(reopened);
        let mut catalog = TursoCatalog::open(&database.path).await?;
        let restored = Checkpoint {
            sequence: 1,
            page: 1,
        };
        let restore = [CatalogPageOperation::Publish(publication(
            coordinate,
            &fixture,
            authority(81),
        )?)];
        assert_eq!(
            catalog
                .apply_page(
                    Checkpoint {
                        sequence: 1,
                        page: 0,
                    },
                    restored,
                    &restore,
                )
                .await?,
            restored
        );
        assert_eq!(
            catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await?
                .image,
            fixture.locator
        );
        Ok(())
    })
}

#[test]
fn completed_snapshot_tombstones_stale_observation_and_removes_current() -> TestResult {
    block_on(async {
        let database = temp_catalog("snapshot-stale-removal")?;
        let feed = FeedIdentity::new("crates.io/sparse/target")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let coordinate = package("7.1.0")?;
        let fixture = image_fixture(82)?;
        let initial = FeedCheckpoint::initial(feed.clone());
        let observed = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            cycle: 1,
            snapshot: None,
            offset: 0,
            ..initial.clone()
        };
        let completed = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 2,
            },
            cycle: 2,
            snapshot: None,
            offset: 0,
            ..observed.clone()
        };
        let observation = FeedObservation {
            coordinate,
            checksum: FeedContentChecksum::from_bytes([82; 32]),
            active: true,
        };

        let mut catalog = TursoCatalog::open(&database.path).await?;
        let publish = [CatalogPageOperation::Publish(publication(
            coordinate,
            &fixture,
            authority(82),
        )?)];
        assert_eq!(
            catalog
                .apply_feed_snapshot_page(&initial, &observed, &publish, &[observation], true)
                .await?,
            observed
        );
        assert!(
            catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await
                .is_ok()
        );

        assert_eq!(
            catalog
                .apply_feed_snapshot_page(&observed, &completed, &[], &[], true)
                .await?,
            completed
        );
        match catalog
            .resolve(coordinate.lineage, Some(coordinate.version))
            .await
        {
            Err(CatalogError::NotFound) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("stale snapshot retained current row").into()),
        }
        let observations = catalog.feed_observations(&feed).await?;
        assert_eq!(observations.len(), 1);
        assert!(!observations[0].active);
        assert_eq!(observations[0].cycle, 1);
        Ok(())
    })
}

#[test]
fn forged_snapshot_completion_or_cycle_jump_rolls_back() -> TestResult {
    block_on(async {
        let database = temp_catalog("forged-feed-transition")?;
        let feed = FeedIdentity::new("crates.io/sparse/forged")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let coordinate = package("7.1.1")?;
        let fixture = image_fixture(84)?;
        let initial = FeedCheckpoint::initial(feed.clone());
        let current = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            cycle: 1,
            ..initial.clone()
        };
        let observation = FeedObservation {
            coordinate,
            checksum: FeedContentChecksum::from_bytes([84; 32]),
            active: true,
        };
        let publish = [CatalogPageOperation::Publish(publication(
            coordinate,
            &fixture,
            authority(84),
        )?)];
        let mut catalog = TursoCatalog::open(&database.path).await?;
        catalog
            .apply_feed_snapshot_page(&initial, &current, &publish, &[observation], true)
            .await?;
        let forged = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 2,
            },
            cycle: 9,
            ..current.clone()
        };
        match catalog
            .apply_feed_snapshot_page(&current, &forged, &[], &[], true)
            .await
        {
            Err(CatalogPageError::FeedTransition(_)) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("forged completion was committed").into()),
        }
        let malformed_shape = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 2,
            },
            cycle: 2,
            offset: 1,
            ..current.clone()
        };
        match catalog
            .apply_feed_snapshot_page(&current, &malformed_shape, &[], &[], false)
            .await
        {
            Err(CatalogPageError::FeedTransition(_)) => {}
            Err(error) => return Err(unexpected(error)),
            Ok(_) => return Err(TestFailure("nonterminal empty snapshot was committed").into()),
        }
        assert_eq!(catalog.feed_checkpoint(feed).await?, current);
        assert!(
            catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await
                .is_ok()
        );
        Ok(())
    })
}

#[test]
fn active_observation_on_another_feed_preserves_current_row() -> TestResult {
    block_on(async {
        let database = temp_catalog("snapshot-active-peer")?;
        let target = FeedIdentity::new("crates.io/sparse/target")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let peer = FeedIdentity::new("crates.io/sparse/peer")
            .map_err(|_| TestFailure("feed identity was rejected"))?;
        let coordinate = package("7.2.0")?;
        let fixture = image_fixture(83)?;
        let target_initial = FeedCheckpoint::initial(target.clone());
        let target_observed = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            cycle: 1,
            snapshot: None,
            offset: 0,
            ..target_initial.clone()
        };
        let target_completed = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 2,
            },
            cycle: 2,
            snapshot: None,
            offset: 0,
            ..target_observed.clone()
        };
        let peer_initial = FeedCheckpoint::initial(peer.clone());
        let peer_observed = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            cycle: 1,
            snapshot: None,
            offset: 0,
            ..peer_initial.clone()
        };
        let observation = FeedObservation {
            coordinate,
            checksum: FeedContentChecksum::from_bytes([83; 32]),
            active: true,
        };

        let mut catalog = TursoCatalog::open(&database.path).await?;
        let publish = [CatalogPageOperation::Publish(publication(
            coordinate,
            &fixture,
            authority(83),
        )?)];
        catalog
            .apply_feed_snapshot_page(
                &target_initial,
                &target_observed,
                &publish,
                &[observation],
                true,
            )
            .await?;
        catalog
            .apply_feed_snapshot_page(&peer_initial, &peer_observed, &[], &[observation], true)
            .await?;

        catalog
            .apply_feed_snapshot_page(&target_observed, &target_completed, &[], &[], true)
            .await?;
        assert!(
            catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await
                .is_ok()
        );
        let target_observations = catalog.feed_observations(&target).await?;
        assert_eq!(target_observations.len(), 1);
        assert!(!target_observations[0].active);
        let peer_observations = catalog.feed_observations(&peer).await?;
        assert_eq!(peer_observations.len(), 1);
        assert!(peer_observations[0].active);
        Ok(())
    })
}

#[test]
fn page_preflight_and_late_failure_leave_no_durable_prefix() -> TestResult {
    block_on(async {
        let database = temp_catalog("page-rollback")?;
        let coordinate = package("8.0.0")?;
        let fixture = image_fixture(91)?;
        let other = image_fixture(92)?;
        let current = Checkpoint {
            sequence: 0,
            page: 0,
        };
        let next = Checkpoint {
            sequence: 0,
            page: 1,
        };
        let valid =
            CatalogPageOperation::Publish(publication(coordinate, &fixture, authority(91))?);
        let invalid = CatalogPageOperation::Publish(CatalogPublication {
            entities: &fixture.entities,
            ..publication(package("8.0.1")?, &other, authority(92))?
        });
        {
            let mut catalog = TursoCatalog::open(&database.path).await?;
            match catalog.apply_page(current, next, &[valid, invalid]).await {
                Err(CatalogPageError::Catalog(CatalogError::EntityImageMismatch)) => {}
                Err(error) => return Err(unexpected(error)),
                Ok(_) => return Err(TestFailure("invalid page committed").into()),
            }
            assert_eq!(catalog.checkpoint().await?, current);
            match catalog
                .resolve(coordinate.lineage, Some(coordinate.version))
                .await
            {
                Err(CatalogError::NotFound) => {}
                Err(error) => return Err(unexpected(error)),
                Ok(_) => return Err(TestFailure("rolled-back prefix resolved").into()),
            }
            let duplicate = [
                CatalogPageOperation::Publish(publication(coordinate, &fixture, authority(91))?),
                CatalogPageOperation::Remove(coordinate),
            ];
            match catalog.apply_page(current, next, &duplicate).await {
                Err(CatalogPageError::DuplicateCoordinate) => {}
                Err(error) => return Err(unexpected(error)),
                Ok(_) => return Err(TestFailure("duplicate coordinate page committed").into()),
            }
            assert_eq!(catalog.checkpoint().await?, current);
        }
        let mut reopened = TursoCatalog::open(&database.path).await?;
        assert_eq!(reopened.checkpoint().await?, current);
        assert_eq!(reopened.history(coordinate).await?.len(), 0);
        match reopened
            .resolve(coordinate.lineage, Some(coordinate.version))
            .await
        {
            Err(CatalogError::NotFound) => Ok(()),
            Err(error) => Err(unexpected(error)),
            Ok(_) => Err(TestFailure("reopened rolled-back prefix resolved").into()),
        }
    })
}
