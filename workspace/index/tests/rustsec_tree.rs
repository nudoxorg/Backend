//! A RustSec advisory directory commits through the same driver as OSV, and a
//! second poll of the same bytes is a no-change. Editing one file emits only
//! that advisory.

mod common;

use std::{fs, sync::Mutex};

use common::migrated_writer;
use index::{
    ingest::{
        advisory::parse_rustsec,
        driver::{DriveOutcome, FollowerDriver},
        rustsec::RustsecFollower,
        watermark::MemoryWatermarkStore,
    },
    protocol::CatalogOp,
};

const KEPT: &str = r#"
[advisory]
id = "RUSTSEC-2020-0001"
package = "smallvec"
date = "2020-01-15"
url = "https://rustsec.org/advisories/RUSTSEC-2020-0001"
title = "overflow"

[versions]
patched = [">= 1.6.1"]
"#;

const OTHER: &str = r#"
[advisory]
id = "RUSTSEC-2021-0001"
package = "regex"
date = "2021-02-01"
title = "other"
"#;

#[test]
fn a_changed_advisory_is_the_only_op_and_a_replay_is_unchanged() {
    let root = std::env::temp_dir().join(format!("rustsec-tree-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("crates/smallvec")).expect("dir");
    fs::create_dir_all(root.join("crates/regex")).expect("regex dir");
    fs::write(root.join("crates/smallvec/RUSTSEC-2020-0001.toml"), KEPT).expect("kept");
    fs::write(root.join("crates/regex/nope.toml"), "not an advisory").expect("poison");

    let writer = migrated_writer();
    let watermarks = MemoryWatermarkStore::new();
    let facts = Mutex::new(index::engine::turso_vc::VersionedCatalog::open().expect("ledger"));
    let driver = FollowerDriver::new(&writer, &watermarks).with_facts(&facts);
    let follower = RustsecFollower::new(&root);

    let expected = parse_rustsec(KEPT, 1_000)
        .expect("parse")
        .catalog_ops()
        .len();
    match driver.drive_once(&follower, 1_000).expect("first") {
        DriveOutcome::Committed { applied, .. } => assert_eq!(applied, expected),
        DriveOutcome::NoChange => panic!("first poll emitted nothing"),
    }
    assert!(matches!(
        driver.drive_once(&follower, 2_000).expect("replay"),
        DriveOutcome::NoChange
    ));

    fs::write(root.join("crates/regex/RUSTSEC-2021-0001.toml"), OTHER).expect("other");
    let other = parse_rustsec(OTHER, 3_000).expect("other parse");
    match driver.drive_once(&follower, 3_000).expect("delta") {
        DriveOutcome::Committed { applied, .. } => {
            assert_eq!(applied, other.catalog_ops().len());
            assert!(other.catalog_ops().iter().any(|op| matches!(
                op,
                CatalogOp::UpsertAdvisory { advisory } if advisory.upstream_id == "RUSTSEC-2021-0001"
            )));
        }
        DriveOutcome::NoChange => panic!("edited file emitted nothing"),
    }
    assert!(matches!(
        driver.drive_once(&follower, 4_000).expect("settled"),
        DriveOutcome::NoChange
    ));
    let _ = fs::remove_dir_all(&root);
}
