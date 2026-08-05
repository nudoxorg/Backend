//! Driver + Homebrew follower tests (adversarial): atomic batches, ETag 304
//! short-circuit, and the Homebrew fixture → golden catalog ops incl. alias
//! confidence tiers.

mod common;

use common::migrated_writer;

use index::enums::AliasConfidence;
use index::protocol::CatalogOp;
use index::store::Catalog;

use index::ingest::driver::{DriveOutcome, FollowerDriver};
use index::ingest::follower::{Follower, FollowerBatch, FollowerError, PollCadence};
use index::ingest::homebrew::{FEED_ID, HomebrewFollower, parse_formulae};
use index::ingest::transport::{FixtureTransport, TransportError};
use index::ingest::watermark::{FeedWatermark, MemoryWatermarkStore, WatermarkStore};

const FIXTURE: &[u8] = include_bytes!("fixtures/homebrew_formula.json");
const FORMULA_URL: &str = "https://formulae.brew.sh/api/formula.json";

// ─────────────────────────────────────────────────────────────────────────────
// Homebrew fixture → golden ops
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn homebrew_fixture_maps_to_expected_ops() {
    let ops = parse_formulae(FIXTURE).expect("parse fixture");

    // 3 formulae × (package + alias + version) = 9 ops (all have stable).
    let packages = ops
        .iter()
        .filter(|o| matches!(o, CatalogOp::UpsertPackage { .. }))
        .count();
    let aliases = ops
        .iter()
        .filter(|o| matches!(o, CatalogOp::UpsertAlias { .. }))
        .count();
    let versions = ops
        .iter()
        .filter(|o| matches!(o, CatalogOp::UpsertVersion { .. }))
        .count();
    assert_eq!((packages, aliases, versions), (3, 3, 3));

    // Every alias is a brew_formula alias for one of the three formulae.
    let names: Vec<String> = ops
        .iter()
        .filter_map(|o| match o {
            CatalogOp::UpsertAlias { kind, alias, .. } => {
                assert_eq!(kind.as_str(), "brew_formula");
                Some(alias.to_string())
            }
            _ => None,
        })
        .collect();
    assert!(names.contains(&"zlib".to_owned()));
    assert!(names.contains(&"openssl@3".to_owned()));
    assert!(names.contains(&"curl".to_owned()));
}

#[test]
fn homebrew_alias_confidence_tiers_are_correct() {
    let ops = parse_formulae(FIXTURE).expect("parse fixture");

    let tier = |name: &str| -> AliasConfidence {
        ops.iter()
            .find_map(|o| match o {
                CatalogOp::UpsertAlias {
                    alias, confidence, ..
                } if alias.as_str() == name => Some(*confidence),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no alias for {name}"))
    };

    // zlib's stable url is a GitHub *release* download → authoritative (§7.1.3).
    assert_eq!(tier("zlib"), AliasConfidence::Authoritative);
    // openssl@3 and curl resolve via a non-release URL/homepage sniff → heuristic.
    assert_eq!(tier("openssl@3"), AliasConfidence::Heuristic);
    assert_eq!(tier("curl"), AliasConfidence::Heuristic);
}

#[test]
fn homebrew_version_keeps_checksum_and_no_git_source() {
    let ops = parse_formulae(FIXTURE).expect("parse fixture");
    for op in &ops {
        if let CatalogOp::UpsertVersion { source, .. } = op {
            let source = source.as_ref().expect("source wire present");
            // Brew records the registry checksum for honesty…
            assert!(source.registry_checksum.is_some(), "sha256 checksum kept");
            // …but never claims a git rev (git is preferred when P6 enumerates).
            assert!(
                source.source_rev.is_none(),
                "brew is reconstruction fallback, no git rev"
            );
        }
    }
}

#[test]
fn homebrew_github_release_url_reduces_to_clean_stem() {
    // zlib's stable url is a GitHub *release* download; the forge-aware reducer
    // in `normalize_repo_url` collapses it to `github.com/madler/zlib` rather
    // than a noisy stem carrying the whole release path — so cross-eco
    // clustering keys on the repo root.
    let ops = parse_formulae(FIXTURE).expect("parse fixture");
    let zlib_package = ops.iter().find_map(|o| match o {
        CatalogOp::UpsertPackage { stem, repo_url }
            if stem.name_canonical == "github.com/madler/zlib" =>
        {
            Some(repo_url.clone())
        }
        _ => None,
    });
    let repo_url = zlib_package.expect("zlib stem canonical is the reduced repo root");
    assert_eq!(repo_url.as_deref(), Some("https://github.com/madler/zlib"));
}

#[test]
fn homebrew_recipe_edges_recorded_literally() {
    let ops = parse_formulae(FIXTURE).expect("parse fixture");
    // curl has 5 runtime + 1 build dependency = 6 recipe edges.
    let curl_edges = ops.iter().find_map(|o| match o {
        CatalogOp::UpsertVersion {
            coordinates: _,
            edges,
            ..
        } if !edges.is_empty() && edges.len() == 6 => Some(edges.clone()),
        _ => None,
    });
    let edges = curl_edges.expect("curl's 6 recipe edges");
    assert!(
        edges
            .iter()
            .all(|e| matches!(e.kind, index::enums::EdgeKind::Recipe))
    );
    assert!(edges.iter().any(|e| e.dep_name_canonical == "openssl@3"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Driver: full follower → catalog path + ETag 304
// ─────────────────────────────────────────────────────────────────────────────

fn brew_transport() -> FixtureTransport {
    FixtureTransport::new().with(FORMULA_URL, FIXTURE.to_vec(), Some("etag-v1"))
}

#[test]
fn driver_commits_homebrew_batch_and_advances_watermark() {
    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    let driver = FollowerDriver::new(&writer, &watermarks);
    let follower = HomebrewFollower::new(brew_transport());

    let outcome = driver.drive_once(&follower, 1000).expect("drive brew");
    match outcome {
        DriveOutcome::Committed { applied, .. } => assert_eq!(applied, 9),
        other => panic!("expected commit, got {other:?}"),
    }

    // The watermark now carries the server ETag.
    let watermark = watermarks
        .feed_watermark(FEED_ID)
        .expect("read")
        .expect("exists");
    assert_eq!(watermark.last_ref.as_deref(), Some("etag-v1"));
    assert!(watermark.last_error.is_none());

    // A package landed in the catalog (proves the batch committed, not just
    // staged). The GitHub release URL reduces to the repo root, so the stem
    // keys on `github.com/madler/zlib` — not the full release path.
    let zlib_stem = index::ingest::enumerate::cpp_stem_id("github.com/madler/zlib");
    let row = writer.get_package(zlib_stem).expect("read");
    assert!(row.is_some(), "zlib package committed to catalog");
}

#[test]
fn driver_short_circuits_on_etag_304() {
    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    // Pre-seed the watermark with the ETag the fixture server will return.
    watermarks
        .put_feed_watermark(&FeedWatermark {
            feed: FEED_ID.to_owned(),
            last_ref: Some("etag-v1".to_owned()),
            last_checked_at: 500,
            last_error: None,
        })
        .unwrap();

    let driver = FollowerDriver::new(&writer, &watermarks);
    let follower = HomebrewFollower::new(brew_transport());

    let outcome = driver.drive_once(&follower, 2000).expect("drive brew");
    assert!(
        matches!(outcome, DriveOutcome::NoChange),
        "304 short-circuits to NoChange"
    );

    // The crawl clock advanced but the ETag is unchanged and no rows were written.
    let watermark = watermarks
        .feed_watermark(FEED_ID)
        .expect("read")
        .expect("exists");
    assert_eq!(watermark.last_ref.as_deref(), Some("etag-v1"));
    assert_eq!(watermark.last_checked_at, 2000);
}

// ─────────────────────────────────────────────────────────────────────────────
// Atomicity: a bad op batch must not advance the watermark or write rows
// ─────────────────────────────────────────────────────────────────────────────

/// A follower whose batch contains a valid package + version followed by a
/// second version that reuses the same `(stem_id, version_canonical)` under a
/// *different* id — violating the `uq_versions_stem_ver` UNIQUE index. Because
/// `apply_ops` runs the whole batch in one transaction, the earlier valid rows
/// must roll back too: nothing lands and the watermark must not advance.
struct BadBatchFollower;

impl BadBatchFollower {
    fn stem() -> index::ids::PackageStemId {
        index::ids::PackageStemId::from_uuid(uuid::Uuid::from_u128(0xDEAD))
    }
}

impl Follower for BadBatchFollower {
    fn feed_id(&self) -> &str {
        "bad-batch"
    }
    fn cadence(&self) -> PollCadence {
        PollCadence::EverySeconds(1)
    }
    fn poll(
        &self,
        _previous: Option<&FeedWatermark>,
        now_unix_ms: i64,
    ) -> Result<FollowerBatch, FollowerError> {
        use index::ids::PackageId;
        use index::protocol::{FacetWire, PackageStemWire, VersionCoordinates};
        let stem = Self::stem();
        let package = CatalogOp::UpsertPackage {
            stem: PackageStemWire {
                stem_id: stem,
                ecosystem: heart::Language::Cpp,
                name_struct: "pkg:generic/bad".to_owned(),
                name_canonical: "bad".to_owned(),
                name_original: "bad".to_owned(),
            },
            repo_url: None,
        };
        let make_version = |id: u128| CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: PackageId::from_uuid(uuid::Uuid::from_u128(id)),
                stem_id: stem,
                // Same canonical for both ids ⇒ the UNIQUE(stem, canonical)
                // index rejects the second insert.
                version_canonical: "1.0.0".to_owned(),
                version_original: "1.0.0".to_owned(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: Vec::new(),
            facets: FacetWire::default(),
            source: None,
        };
        Ok(FollowerBatch {
            ops: vec![package, make_version(0xBEEF), make_version(0xCAFE)],
            next_watermark: FeedWatermark {
                feed: "bad-batch".to_owned(),
                last_ref: Some("should-not-persist".to_owned()),
                last_checked_at: now_unix_ms,
                last_error: None,
            },
            caught_up: true,
        })
    }
}

#[test]
fn bad_batch_does_not_advance_watermark_or_write_rows() {
    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    let driver = FollowerDriver::new(&writer, &watermarks);

    let result = driver.drive_once(&BadBatchFollower, 9000);
    assert!(
        result.is_err(),
        "a failing op batch must surface as a DriveError"
    );

    // Watermark was NOT advanced — the follower would re-deliver the batch.
    let watermark = watermarks.feed_watermark("bad-batch").expect("read");
    assert!(
        watermark.is_none(),
        "no watermark persisted after a failed batch"
    );

    // And the *valid* leading package op rolled back with the batch — proving
    // atomicity, not just that the bad op was skipped.
    let package = writer.get_package(BadBatchFollower::stem()).expect("read");
    assert!(
        package.is_none(),
        "no partial rows after a rolled-back batch"
    );
}

// A transport error must also propagate typed, not panic.
struct BrokenTransport;
impl index::ingest::transport::FeedTransport for BrokenTransport {
    fn fetch(
        &self,
        request: &index::ingest::transport::FeedRequest,
    ) -> Result<index::ingest::transport::FeedResponse, TransportError> {
        Err(TransportError::Request {
            url: request.url.clone(),
            message: "connection refused".to_owned(),
        })
    }
}

#[test]
fn transport_error_propagates_typed() {
    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    let driver = FollowerDriver::new(&writer, &watermarks);
    let follower = HomebrewFollower::new(BrokenTransport);

    let result = driver.drive_once(&follower, 1000);
    assert!(
        result.is_err(),
        "transport failure is a typed DriveError, never a panic"
    );
}
