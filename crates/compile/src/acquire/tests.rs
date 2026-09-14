use crate::acquire::crates_io::{PollResult, hex_digest};
use super::*;
use backend_semantic::ir::PackageLineage;
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
    FactAvailability, IrBuilder, ItemKind, ParentageAuthority, SemanticImageView, SemanticReader,
    TreeItemInput, VariantFingerprint, Visibility, encode_full_semantic_image,
    full_semantic_image_len,
};
use futures_executor::block_on;
use backend_version::{CompilePublicationDomain, ContentId, GenerationId};
use backend_semantic::catalog::{
    CatalogError, FeedCheckpoint, FeedContentChecksum, FeedCursor, FeedValidator, TursoCatalog,
};
use backend_semantic::index_ingest::Checkpoint;
use backend_semantic::index_vocabulary::{
    CanonicalEntityLocator, IndexLocatorFacts, PackageVersion, SemanticImageLocator,
};
use backend_semantic::index_vocabulary::{IndexSnapshotId, SemanticImageExtent};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

type ScriptReply = Result<(u16, Option<String>, Vec<u8>), TransportFault>;
struct ScriptTransport {
    replies: VecDeque<ScriptReply>,
    requests: Arc<AtomicU64>,
}

impl ScriptTransport {
    fn new(replies: Vec<ScriptReply>) -> Self {
        Self {
            replies: replies.into(),
            requests: Arc::new(AtomicU64::new(0)),
        }
    }
    fn counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.requests)
    }
}

impl RegistryTransport for ScriptTransport {
    fn get(
        &mut self,
        request: TransportRequest<'_>,
        output: &mut Vec<u8>,
        _: &AtomicBool,
    ) -> Result<TransportResponse, TransportFault> {
        self.requests.fetch_add(1, Ordering::AcqRel);
        let Some(reply) = self.replies.pop_front() else {
            return Err(TransportFault::Network);
        };
        let (status, etag, bytes) = reply?;
        if bytes.len() > request.maximum_bytes {
            return Err(TransportFault::TooLarge);
        }
        output.clear();
        output.extend_from_slice(&bytes);
        Ok(TransportResponse { status, etag })
    }
}

struct TestMaterializer {
    fail: bool,
}

#[derive(Debug)]
struct TestMaterializerError(&'static str);

impl core::fmt::Display for TestMaterializerError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for TestMaterializerError {}

impl From<&'static str> for TestMaterializerError {
    fn from(value: &'static str) -> Self {
        Self(value)
    }
}

impl RegistryMaterializer for TestMaterializer {
    type Error = TestMaterializerError;

    fn materialize(
        &mut self,
        _archive: VerifiedArchive<'_>,
    ) -> Result<MaterializedPublication, Self::Error> {
        if self.fail {
            return Err(TestMaterializerError(
                "fixture materializer rejected archive",
            ));
        }
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            visibility: FactAvailability::Captured,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([7; 16]),
            variant: VariantFingerprint::from_raw([8; 16]),
            core_payload: CorePayloadHash::from_raw([9; 16]),
        }];
        let items = [TreeItemInput {
            name: b"fixture",
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
        }];
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .map_err(|_| "fixture tree")?;
        let ir = builder.finish().map_err(|_| "fixture ir")?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|_| "fixture length")?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|_| "fixture encode")?;
        let reopened = SemanticImageView::reopen(&bytes).map_err(|_| "fixture reopen")?;
        let extent =
            SemanticImageExtent::new(0, u32::try_from(bytes.len()).map_err(|_| "fixture extent")?)
                .map_err(|_| "fixture extent")?;
        let facts = IndexLocatorFacts::new(
            GenerationId::from_digest([1; 32]),
            IndexSnapshotId::from_canonical_bytes(b"snapshot"),
            ContentId::<CompilePublicationDomain>::from_canonical_bytes(b"publication"),
        );
        let image = SemanticImageLocator::new(
            backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(&bytes),
            extent,
        );
        let entities = reopened
            .canonical_entities()
            .enumerate()
            .map(|(ordinal, entity)| {
                CanonicalEntityLocator::new(
                    image,
                    u32::try_from(ordinal).map_err(|_| "fixture ordinal")?,
                    entity.version.identity(),
                )
                .verify_reopened(&reopened)
                .map_err(|_| "fixture entity proof")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MaterializedPublication {
            publication: backend_semantic::index_vocabulary::VerifiedSemanticPublication::verify_reopened(
                facts, image, &reopened,
            )
            .map_err(|_| "fixture proof")?,
            entities,
        })
    }
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
fn catalog() -> Result<TempCatalog, Box<dyn Error>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "nudox-acquire-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(TempCatalog {
        path: path.to_str().ok_or("non-utf8 temp path")?.to_owned(),
    })
}
fn listing(checksum: ArchiveChecksum) -> Vec<u8> {
    let mut hex = String::new();
    for byte in checksum.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("{{\"name\":\"fixture\",\"vers\":\"1.2.3\",\"cksum\":\"{hex}\",\"yanked\":false}}\n")
        .into_bytes()
}
fn replies(archive: &[u8]) -> Vec<ScriptReply> {
    let checksum = ArchiveChecksum::from_bytes(Sha256::digest(archive).into());
    vec![
        Ok((200, Some("page-etag".to_owned()), listing(checksum))),
        Ok((200, None, archive.to_vec())),
    ]
}
fn listing_many(count: usize, checksum: ArchiveChecksum) -> Vec<u8> {
    let digest = hex_digest(checksum);
    (0..count)
            .map(|index| format!("{{\"name\":\"fixture\",\"vers\":\"1.0.{index}\",\"cksum\":\"{digest}\",\"yanked\":false}}\n"))
            .collect::<String>()
            .into_bytes()
}
fn yanked_listing_many(count: usize, checksum: ArchiveChecksum) -> Vec<u8> {
    (0..count)
        .map(|index| sparse_row(&format!("1.0.{index}"), checksum, true))
        .collect::<String>()
        .into_bytes()
}

fn sparse_row(version: &str, checksum: ArchiveChecksum, yanked: bool) -> String {
    format!(
        "{{\"name\":\"fixture\",\"vers\":\"{version}\",\"cksum\":\"{}\",\"yanked\":{yanked}}}\n",
        hex_digest(checksum)
    )
}

#[test]
fn unconditional_first_poll_304_is_a_protocol_status_failure() -> Result<(), Box<dyn Error>> {
    let transport = ScriptTransport::new(vec![Ok((304, None, Vec::new()))]);
    let counter = transport.counter();
    let mut adapter = CratesIoAdapter::with_bases(
        transport,
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    assert!(matches!(
        adapter.poll(
            &FeedCheckpoint::initial(adapter.feed().clone()),
            &AtomicBool::new(false)
        ),
        Err(RegistryError::Status { status: 304 })
    ));
    assert_eq!(counter.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn duplicate_sparse_version_is_rejected_before_archive_fetch() -> Result<(), Box<dyn Error>> {
    let checksum = ArchiveChecksum::from_bytes(Sha256::digest(b"crate").into());
    let body = format!(
        "{}{}",
        sparse_row("1.0.0", checksum, false),
        sparse_row("1.0.0", checksum, false)
    );
    let transport = ScriptTransport::new(vec![Ok((200, None, body.into_bytes()))]);
    let counter = transport.counter();
    let mut adapter = CratesIoAdapter::with_bases(
        transport,
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    assert!(matches!(
        adapter.poll(
            &FeedCheckpoint::initial(adapter.feed().clone()),
            &AtomicBool::new(false)
        ),
        Err(RegistryError::Protocol)
    ));
    assert_eq!(counter.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn checksum_representation_rejects_whitespace() {
    let digest = "00".repeat(32);
    assert!(ArchiveChecksum::parse_hex(&format!(" {digest}")).is_err());
    assert!(ArchiveChecksum::parse_hex(&format!("{digest} ")).is_err());
}

#[test]
fn shuffled_sparse_rows_admit_the_same_canonical_chunk() -> Result<(), Box<dyn Error>> {
    let checksum = ArchiveChecksum::from_bytes(Sha256::digest(b"crate").into());
    let ordered = ["1.0.0", "1.0.1", "2.0.0"];
    let body = ordered
        .iter()
        .map(|version| sparse_row(version, checksum, false))
        .collect::<String>();
    let shuffled = ["2.0.0", "1.0.0", "1.0.1"]
        .iter()
        .map(|version| sparse_row(version, checksum, false))
        .collect::<String>();
    let mut first = CratesIoAdapter::with_bases(
        ScriptTransport::new(vec![Ok((200, None, body.into_bytes()))]),
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    let mut second = CratesIoAdapter::with_bases(
        ScriptTransport::new(vec![Ok((200, None, shuffled.into_bytes()))]),
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    let state = FeedCheckpoint::initial(first.feed().clone());
    let PollResult::Page(left) = first.poll(&state, &AtomicBool::new(false))? else {
        return Err("first sparse poll was unexpectedly a no-op".into());
    };
    let PollResult::Page(right) = second.poll(&state, &AtomicBool::new(false))? else {
        return Err("second sparse poll was unexpectedly a no-op".into());
    };
    assert_eq!(
        left.entries
            .iter()
            .map(|entry| &entry.version)
            .collect::<Vec<_>>(),
        right
            .entries
            .iter()
            .map(|entry| &entry.version)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn checksum_mismatch_never_reaches_materialization() -> Result<(), Box<dyn Error>> {
    let wrong = ArchiveChecksum::from_bytes([0; 32]);
    let transport = ScriptTransport::new(vec![
        Ok((200, None, listing(wrong))),
        Ok((200, None, b"actual".to_vec())),
    ]);
    let mut adapter = CratesIoAdapter::with_bases(
        transport,
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    let state = FeedCheckpoint::initial(adapter.feed().clone());
    let PollResult::Page(page) = adapter.poll(&state, &AtomicBool::new(false))? else {
        return Err("fixture poll was unexpectedly 304".into());
    };
    let entry = page.entries.first().ok_or("fixture entry absent")?;
    match adapter.fetch_archive(entry, &AtomicBool::new(false)) {
        Err(RegistryError::ChecksumMismatch { .. }) => Ok(()),
        Err(error) => Err(Box::new(error)),
        Ok(_) => Err("checksum mismatch was admitted".into()),
    }
}

#[test]
fn malformed_and_oversized_listing_are_terminal() -> Result<(), Box<dyn Error>> {
    let malformed = ScriptTransport::new(vec![Ok((200, None, b"{truncated".to_vec()))]);
    let mut malformed = CratesIoAdapter::with_bases(
        malformed,
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    let state = FeedCheckpoint::initial(malformed.feed().clone());
    assert!(matches!(
        malformed.poll(&state, &AtomicBool::new(false)),
        Err(RegistryError::Protocol)
    ));
    let oversized = ScriptTransport::new(vec![Ok((200, None, vec![0; MAX_PAGE_BYTES + 1]))]);
    let mut oversized = CratesIoAdapter::with_bases(
        oversized,
        "https://fixture.invalid",
        "https://fixture.invalid",
        "fixture",
    )?;
    let state = FeedCheckpoint::initial(oversized.feed().clone());
    assert!(matches!(
        oversized.poll(&state, &AtomicBool::new(false)),
        Err(RegistryError::Transport(TransportFault::TooLarge))
    ));
    Ok(())
}

#[test]
fn conditional_no_op_leaves_checkpoint_initial() -> Result<(), Box<dyn Error>> {
    block_on(async {
        let database = catalog()?;
        let transport =
            ScriptTransport::new(vec![Ok((304, Some("unchanged".to_owned()), Vec::new()))]);
        let adapter = CratesIoAdapter::with_bases(
            transport,
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        let current = FeedCheckpoint::initial(feed.clone());
        let next = FeedCheckpoint {
            checkpoint: Checkpoint {
                sequence: 0,
                page: 1,
            },
            validator: Some(FeedValidator::new("known").map_err(|_| "fixture validator")?),
            ..current.clone()
        };
        durable.apply_feed_page(&current, &next, &[]).await?;
        assert_eq!(
            ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await?,
            IngestOutcome::NoChange
        );
        assert_eq!(
            durable.feed_checkpoint(feed).await?.checkpoint,
            Checkpoint {
                sequence: 0,
                page: 1
            }
        );
        Ok(())
    })
}

#[test]
fn materialization_failure_leaves_no_catalog_prefix_or_feed_advance() -> Result<(), Box<dyn Error>>
{
    block_on(async {
        let database = catalog()?;
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(replies(b"crate")),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: true });
        let mut durable = TursoCatalog::open(&database.path).await?;
        assert!(matches!(
            ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await,
            Err(IngestError::Materialization(_))
        ));
        assert_eq!(
            durable.feed_checkpoint(feed).await?.checkpoint,
            Checkpoint {
                sequence: 0,
                page: 0
            }
        );
        let lineage = PackageLineage::new("cargo", "fixture").map_err(|_| "fixture lineage")?;
        let version = PackageVersion::new("1.2.3").map_err(|_| "fixture version")?;
        assert!(matches!(
            durable.resolve(lineage, Some(version)).await,
            Err(CatalogError::NotFound)
        ));
        Ok(())
    })
}

#[test]
fn verified_page_commits_feed_cursor_and_replays_idempotently() -> Result<(), Box<dyn Error>> {
    block_on(async {
        let database = catalog()?;
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(replies(b"crate")),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        let applied = ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let IngestOutcome::Applied(state) = applied else {
            return Err("successful page was no-op".into());
        };
        assert_eq!(
            state.cursor.as_ref().map(FeedCursor::as_str),
            Some("fixture")
        );
        assert_eq!(
            state.validator.as_ref().map(FeedValidator::as_str),
            Some("page-etag")
        );
        drop(durable);
        let durable = TursoCatalog::open(&database.path).await?;
        assert_eq!(durable.feed_checkpoint(feed).await?, state);
        Ok(())
    })
}

#[test]
fn unchanged_checksum_never_requests_another_archive() -> Result<(), Box<dyn Error>> {
    block_on(async {
        let database = catalog()?;
        let archive = b"crate";
        let checksum = ArchiveChecksum::from_bytes(Sha256::digest(archive).into());
        let mut scripted = replies(archive);
        scripted.push(Ok((200, Some("same-etag".to_owned()), listing(checksum))));
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(scripted),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        let first = ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        assert!(matches!(first, IngestOutcome::Applied(_)));
        // The final scripted reply is only metadata. An archive request here would exhaust
        // the fixture and fail, proving durable checksum comparison avoided it.
        assert!(matches!(
            ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await?,
            IngestOutcome::Applied(_)
        ));
        Ok(())
    })
}

#[test]
fn over_sixty_four_versions_resume_the_same_snapshot_after_reopen() -> Result<(), Box<dyn Error>> {
    block_on(async {
        let database = catalog()?;
        let archive = b"crate";
        let checksum = ArchiveChecksum::from_bytes(Sha256::digest(archive).into());
        let mut scripted = vec![Ok((
            200,
            Some("snapshot".to_owned()),
            listing_many(65, checksum),
        ))];
        scripted.extend((0..SPARSE_CHUNK_ROWS).map(|_| Ok((200, None, archive.to_vec()))));
        scripted.push(Ok((
            200,
            Some("snapshot".to_owned()),
            listing_many(65, checksum),
        )));
        scripted.push(Ok((200, None, archive.to_vec())));
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(scripted),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let first = {
            let mut durable = TursoCatalog::open(&database.path).await?;
            let first = ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await?;
            let IngestOutcome::Applied(state) = first else {
                return Err("first chunk was no-op".into());
            };
            assert_eq!(state.offset, u64::try_from(SPARSE_CHUNK_ROWS)?);
            assert!(state.snapshot.is_some());
            state
        };
        let mut durable = TursoCatalog::open(&database.path).await?;
        assert_eq!(durable.feed_checkpoint(feed).await?, first);
        let second = ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let IngestOutcome::Applied(state) = second else {
            return Err("resume chunk was no-op".into());
        };
        assert!(state.snapshot.is_none());
        assert_eq!(state.offset, 0);
        Ok(())
    })
}

#[test]
fn checksum_change_fetches_again_and_replaces_the_durable_observation() -> Result<(), Box<dyn Error>>
{
    block_on(async {
        let database = catalog()?;
        let first_archive = b"first";
        let second_archive = b"second";
        let first_checksum = ArchiveChecksum::from_bytes(Sha256::digest(first_archive).into());
        let second_checksum = ArchiveChecksum::from_bytes(Sha256::digest(second_archive).into());
        let transport = ScriptTransport::new(vec![
            Ok((200, Some("first".to_owned()), listing(first_checksum))),
            Ok((200, None, first_archive.to_vec())),
            Ok((200, Some("second".to_owned()), listing(second_checksum))),
            Ok((200, None, second_archive.to_vec())),
        ]);
        let counter = transport.counter();
        let adapter = CratesIoAdapter::with_bases(
            transport,
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        assert!(matches!(
            ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await?,
            IngestOutcome::Applied(_)
        ));
        assert!(matches!(
            ingestor
                .poll_and_apply(&mut durable, &AtomicBool::new(false))
                .await?,
            IngestOutcome::Applied(_)
        ));
        assert_eq!(counter.load(Ordering::Acquire), 4);
        let observed = durable.feed_observations(&feed).await?;
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].checksum,
            FeedContentChecksum::from_bytes(second_checksum.as_bytes())
        );
        Ok(())
    })
}

#[test]
fn yanked_version_remains_until_the_final_snapshot_chunk() -> Result<(), Box<dyn Error>> {
    block_on(async {
        let database = catalog()?;
        let archive = b"crate";
        let checksum = ArchiveChecksum::from_bytes(Sha256::digest(archive).into());
        let yanked = format!(
            "{}{}",
            sparse_row("1.2.3", checksum, true),
            String::from_utf8(yanked_listing_many(64, checksum))?
        )
        .into_bytes();
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(vec![
                Ok((200, Some("active".to_owned()), listing(checksum))),
                Ok((200, None, archive.to_vec())),
                Ok((200, Some("yanked".to_owned()), yanked.clone())),
                Ok((200, Some("yanked".to_owned()), yanked)),
            ]),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let lineage = PackageLineage::new("cargo", "fixture").map_err(|_| "fixture lineage")?;
        let version = PackageVersion::new("1.2.3").map_err(|_| "fixture version")?;
        assert!(durable.resolve(lineage, Some(version)).await.is_ok());
        ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        assert!(durable.resolve(lineage, Some(version)).await.is_ok());
        ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        assert!(matches!(
            durable.resolve(lineage, Some(version)).await,
            Err(CatalogError::NotFound)
        ));
        Ok(())
    })
}

#[test]
fn changed_snapshot_restarts_at_zero_and_cancellation_leaves_checkpoint_unchanged()
-> Result<(), Box<dyn Error>> {
    block_on(async {
        let checksum = ArchiveChecksum::from_bytes(Sha256::digest(b"crate").into());
        let changed = ArchiveChecksum::from_bytes(Sha256::digest(b"changed").into());
        let mut adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(vec![
                Ok((200, Some("first".to_owned()), listing_many(65, checksum))),
                Ok((200, Some("changed".to_owned()), listing_many(65, changed))),
            ]),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let initial = FeedCheckpoint::initial(adapter.feed().clone());
        let PollResult::Page(first) = adapter.poll(&initial, &AtomicBool::new(false))? else {
            return Err("initial snapshot was unexpectedly a no-op".into());
        };
        let interrupted = FeedCheckpoint {
            cycle: 1,
            snapshot: Some(first.snapshot),
            offset: first.next_offset,
            ..initial.clone()
        };
        let PollResult::Page(restarted) = adapter.poll(&interrupted, &AtomicBool::new(false))?
        else {
            return Err("changed snapshot was unexpectedly a no-op".into());
        };
        assert_eq!(
            restarted
                .entries
                .first()
                .map(|entry| entry.version.as_str()),
            Some("1.0.0")
        );
        assert_eq!(restarted.next_offset, u64::try_from(SPARSE_CHUNK_ROWS)?);

        let database = catalog()?;
        let cancelled = AtomicBool::new(true);
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(vec![Ok((200, None, listing(checksum)))]),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        assert!(matches!(
            ingestor.poll_and_apply(&mut durable, &cancelled).await,
            Err(IngestError::Registry(RegistryError::Transport(
                TransportFault::Cancelled
            )))
        ));
        assert_eq!(
            durable.feed_checkpoint(feed).await?.checkpoint,
            Checkpoint {
                sequence: 0,
                page: 0,
            }
        );
        Ok(())
    })
}

#[test]
fn changed_body_mid_cycle_restarts_durably_without_premature_removal() -> Result<(), Box<dyn Error>>
{
    block_on(async {
        let database = catalog()?;
        let archive = b"crate";
        let checksum = ArchiveChecksum::from_bytes(Sha256::digest(archive).into());
        let yanked = format!(
            "{}{}",
            sparse_row("1.2.3", checksum, true),
            String::from_utf8(yanked_listing_many(64, checksum))?
        )
        .into_bytes();
        let mut changed = yanked.clone();
        changed.push(b'\n');
        let adapter = CratesIoAdapter::with_bases(
            ScriptTransport::new(vec![
                Ok((200, Some("active".to_owned()), listing(checksum))),
                Ok((200, None, archive.to_vec())),
                Ok((200, Some("cycle-two".to_owned()), yanked)),
                Ok((200, Some("cycle-three".to_owned()), changed)),
            ]),
            "https://fixture.invalid",
            "https://fixture.invalid",
            "fixture",
        )?;
        let feed = adapter.feed().clone();
        let mut ingestor = RegistryIngestor::new(adapter, TestMaterializer { fail: false });
        let mut durable = TursoCatalog::open(&database.path).await?;
        ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let first_partial = ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let IngestOutcome::Applied(first_partial) = first_partial else {
            return Err("first cycle chunk was unexpectedly a no-op".into());
        };
        assert_eq!(first_partial.cycle, 2);
        assert_eq!(first_partial.offset, u64::try_from(SPARSE_CHUNK_ROWS)?);
        assert!(first_partial.snapshot.is_some());
        let restarted = ingestor
            .poll_and_apply(&mut durable, &AtomicBool::new(false))
            .await?;
        let IngestOutcome::Applied(restarted) = restarted else {
            return Err("changed body was unexpectedly a no-op".into());
        };
        assert_eq!(restarted.cycle, 3);
        assert_eq!(restarted.offset, u64::try_from(SPARSE_CHUNK_ROWS)?);
        assert!(restarted.snapshot.is_some());
        assert_eq!(durable.feed_checkpoint(feed).await?, restarted);
        let lineage = PackageLineage::new("cargo", "fixture").map_err(|_| "fixture lineage")?;
        let version = PackageVersion::new("1.2.3").map_err(|_| "fixture version")?;
        assert!(durable.resolve(lineage, Some(version)).await.is_ok());
        Ok(())
    })
}
