//! C++ packages with a repository URL are the git poll set. A Rust package is
//! not. The first tick of a scripted remote commits the tag, and the second
//! tick of the same bytes is a no-change.

mod common;

use std::sync::Mutex;

use common::{FakeGitRepository, migrated_writer, stem_id};
use index::{
    ingest::{
        driver::{FollowerDriver, GitDriveOutcome},
        enumerate::upsert_cpp_package,
        monitor::GitMonitor,
        watermark::MemoryWatermarkStore,
    },
    protocol::{CatalogOp, PackageStemWire},
    store::{MetaStore, read::git_poll_targets},
};

const URL: &str = "https://example.test/real-pipeline.git";
const SLUG: &str = "example.test/real-pipeline";
const OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn listed_cpp_repos_commit_once_and_a_replay_is_unchanged() {
    let writer = migrated_writer();
    writer
        .apply_ops(&[
            upsert_cpp_package(SLUG, URL),
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem_id(9),
                    ecosystem: heart::Language::Rust,
                    name_struct: "pkg:cargo/memchr".into(),
                    name_canonical: "memchr".into(),
                    name_original: "memchr".into(),
                },
                repo_url: Some("https://example.test/memchr.git".into()),
            },
            CatalogOp::UpsertPackage {
                stem: PackageStemWire {
                    stem_id: stem_id(8),
                    ecosystem: heart::Language::Cpp,
                    name_struct: "pkg:generic/no-url".into(),
                    name_canonical: "no-url".into(),
                    name_original: "no-url".into(),
                },
                repo_url: None,
            },
        ])
        .expect("register");

    let targets = git_poll_targets(writer.engine()).expect("list");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name, SLUG);
    assert_eq!(targets[0].repo_url, URL);

    let bytes = format!("{OID}\trefs/tags/v1.0.0\n");
    let git = FakeGitRepository::new().with_ls_remote(URL, bytes.as_bytes());
    let watermarks = MemoryWatermarkStore::new();
    let facts = Mutex::new(index::engine::turso_vc::VersionedCatalog::open().expect("ledger"));
    let driver = FollowerDriver::new(&writer, &watermarks).with_facts(&facts);
    let monitor = GitMonitor::new(git);
    let target = &targets[0];
    let first = driver
        .drive_git_once(
            &monitor,
            target.stem_id,
            &target.name,
            &target.repo_url,
            1_000,
            1,
        )
        .expect("first");
    assert!(matches!(first, GitDriveOutcome::Committed { .. }));
    let tip = facts
        .lock()
        .expect("facts")
        .materialize("cpp", SLUG, "v1.0.0")
        .expect("read")
        .expect("tag");
    assert_eq!(tip.version.as_str(), "v1.0.0");

    let second = driver
        .drive_git_once(
            &monitor,
            target.stem_id,
            &target.name,
            &target.repo_url,
            2_000,
            1,
        )
        .expect("replay");
    assert!(matches!(second, GitDriveOutcome::NoChange));

    let resumed = MemoryWatermarkStore::new();
    let resumed_driver = FollowerDriver::new(&writer, &resumed).with_facts(&facts);
    let third = resumed_driver
        .drive_git_once(
            &monitor,
            target.stem_id,
            &target.name,
            &target.repo_url,
            3_000,
            1,
        )
        .expect("resume");
    assert!(
        matches!(third, GitDriveOutcome::NoChange),
        "a fresh driver watermark resumes from the catalog last_rev"
    );
}
