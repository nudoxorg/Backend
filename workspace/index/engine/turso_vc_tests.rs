use super::*;

fn fact(hash: &str) -> PackageFact {
    PackageFact {
        ecosystem: "rust".into(),
        name: "memchr".into(),
        version: "2.8.3".into(),
        payload_hash: hash.into(),
        body: hash.into(),
    }
}

#[test]
fn upsert_is_a_new_revision_and_history_reads_back() {
    let mut catalog = VersionedCatalog::open().expect("open");
    let first = catalog.upsert(&fact("aaa")).expect("first");
    let second = catalog.upsert(&fact("bbb")).expect("second");
    assert_ne!(first, second);
    let tip = catalog.get("rust", "memchr", "2.8.3").expect("tip");
    assert_eq!(tip.payload_hash, "bbb");
    let prior = catalog
        .get_at("rust", "memchr", "2.8.3", first)
        .expect("at")
        .expect("row existed");
    assert_eq!(prior.payload_hash, "aaa");
}

#[test]
fn an_unchanged_record_does_not_grow_history() {
    use crate::record::{DepEdge, PackageRecord};
    use heart::Language;

    let record = PackageRecord::from_parts(
        Language::Rust,
        "memchr",
        "2.8.3",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        vec![DepEdge::runtime("libc")],
    );
    let mut catalog = VersionedCatalog::open().expect("open");
    let FactWrite::Revised(first) = catalog.put_record(&record).expect("put") else {
        panic!("first write revises");
    };
    assert_eq!(
        catalog.put_record(&record).expect("replay"),
        FactWrite::Unchanged
    );
    let padded = PackageRecord::from_parts(
        Language::Rust,
        "memchr",
        "2.8.3",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        crate::record::runtime_edges_from_names(&["  libc  ", "libc", " "]),
    );
    assert_eq!(
        catalog.put_record(&padded).expect("normalized replay"),
        FactWrite::Unchanged
    );
    let identity = catalog
        .get("rust", "memchr", "2.8.3")
        .expect("identity")
        .payload_hash;
    let mut changed = record.clone();
    changed.edges = vec![DepEdge::runtime("windows-sys")];
    let FactWrite::Revised(second) = catalog.put_record(&changed).expect("change") else {
        panic!("a new edge revises");
    };
    assert_ne!(first, second);
    assert_eq!(
        catalog
            .get("rust", "memchr", "2.8.3")
            .expect("identity")
            .payload_hash,
        identity
    );
    let prior = catalog
        .materialize_at("rust", "memchr", "2.8.3", first)
        .expect("at")
        .expect("row");
    assert_eq!(prior.edges[0].name.as_str(), "libc");
    let tip = catalog
        .materialize("rust", "memchr", "2.8.3")
        .expect("tip")
        .expect("row");
    assert_eq!(tip.edges[0].name.as_str(), "windows-sys");
}

#[test]
fn one_edge_change_leaves_the_other_edge_and_the_package_fact() {
    use crate::record::{DepEdge, PackageRecord};
    use heart::Language;
    use smol_str::SmolStr;

    let mut libc = DepEdge::runtime("libc");
    libc.requirement = Some(SmolStr::new("^1"));
    let mut windows = DepEdge::runtime("windows-sys");
    windows.requirement = Some(SmolStr::new("^0.52"));
    let record = PackageRecord::from_parts(
        Language::Rust,
        "memchr",
        "2.8.3",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        vec![libc, windows],
    );
    let mut catalog = VersionedCatalog::open().expect("open");
    let FactWrite::Revised(package_commit) = catalog.put_record(&record).expect("package") else {
        panic!("package fact revises");
    };
    let first = catalog.sync_edges(&record).expect("edges");
    assert_eq!(first.revised, 0);
    assert_eq!(first.unchanged, 2);
    assert_eq!(first.removed, 0);
    let libc_before = catalog
        .get_edge("rust", "memchr", "2.8.3", "libc")
        .expect("libc");
    let identity = catalog
        .get("rust", "memchr", "2.8.3")
        .expect("package")
        .payload_hash;
    let mut changed = record.clone();
    changed.edges[1].requirement = Some(SmolStr::new("^0.59"));
    let second = catalog.sync_edges(&changed).expect("resync");
    assert_eq!(second.revised, 1);
    assert_eq!(second.unchanged, 1);
    assert_eq!(second.removed, 0);
    assert_eq!(
        catalog
            .get("rust", "memchr", "2.8.3")
            .expect("package")
            .payload_hash,
        identity
    );
    let libc_after = catalog
        .get_edge("rust", "memchr", "2.8.3", "libc")
        .expect("libc stays");
    assert_eq!(libc_before.payload_hash, libc_after.payload_hash);
    assert_eq!(libc_before.requirement, "^1");
    let windows_tip = catalog
        .get_edge("rust", "memchr", "2.8.3", "windows-sys")
        .expect("windows");
    assert_eq!(windows_tip.requirement, "^0.59");
    let prior = catalog
        .materialize_at("rust", "memchr", "2.8.3", package_commit)
        .expect("package history")
        .expect("package row");
    assert_eq!(prior.edges.len(), 2);
    changed.edges.retain(|edge| edge.name.as_str() != "libc");
    let third = catalog.sync_edges(&changed).expect("drop libc");
    assert_eq!(third.removed, 1);
    assert!(
        catalog
            .get_edge("rust", "memchr", "2.8.3", "libc")
            .is_none()
    );
}

#[test]
fn resync_reads_one_package_tip() {
    use crate::record::{DepEdge, PackageRecord};
    use heart::Language;
    use std::time::Instant;

    let mut catalog = VersionedCatalog::open().expect("open");
    let packages = 128;
    for index in 0..packages {
        let edges = (0..4)
            .map(|slot| DepEdge::runtime(format!("dep-{index}-{slot}")))
            .collect();
        let record = PackageRecord::from_parts(
            Language::Rust,
            format!("pkg-{index}"),
            "1.0.0",
            None,
            None,
            Vec::new(),
            None,
            None,
            false,
            edges,
        );
        catalog.put_record(&record).expect("seed");
    }
    let again = PackageRecord::from_parts(
        Language::Rust,
        "pkg-0",
        "1.0.0",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        (0..4)
            .map(|slot| DepEdge::runtime(format!("dep-0-{slot}")))
            .collect(),
    );
    let loops = 8;
    let started = Instant::now();
    for _ in 0..loops {
        let sync = catalog.sync_edges(&again).expect("tip");
        assert_eq!(sync.revised, 0);
        assert_eq!(sync.unchanged, 4);
    }
    let tip_ns = started.elapsed().as_nanos();
    let started = Instant::now();
    for _ in 0..loops {
        let rows = catalog.scan_edges();
        assert_eq!(rows.len(), packages * 4);
    }
    let scan_ns = started.elapsed().as_nanos();
    eprintln!(
        "cost case=frontier/edge_tip packages={packages} loops={loops} tip_ns={tip_ns} scan_ns={scan_ns}"
    );
    assert!(
        tip_ns.saturating_mul(2) < scan_ns,
        "tip {tip_ns} scan {scan_ns}"
    );
}

#[test]
fn a_repeated_name_keeps_the_first_requirement() {
    use crate::record::{DepClass, DepEdge, PackageRecord};
    use heart::Language;
    use smol_str::SmolStr;

    let mut first = DepEdge::runtime("libc");
    first.requirement = Some(SmolStr::new("^1"));
    first.optional = true;
    let second = DepEdge {
        name: SmolStr::new("libc"),
        requirement: Some(SmolStr::new("^9")),
        class: DepClass::Dev,
        optional: false,
    };
    let record = PackageRecord::from_parts(
        Language::Rust,
        "memchr",
        "2.8.3",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        vec![first.clone(), second],
    );
    assert_eq!(
        super::edge_fact::hash_edge(&first),
        EdgeFact::from_edge(&record, &first).payload_hash
    );
    let mut catalog = VersionedCatalog::open().expect("open");
    catalog.put_record(&record).expect("put");
    let tip = catalog
        .materialize("rust", "memchr", "2.8.3")
        .expect("join")
        .expect("row");
    assert_eq!(tip.edges.len(), 1);
    assert_eq!(tip.edges[0].requirement.as_deref(), Some("^1"));
    assert!(tip.edges[0].optional);
    assert_eq!(tip.edges[0].class, DepClass::Runtime);
}

#[test]
fn an_optional_flag_revises_one_edge_and_the_hash_matches_the_row() {
    use crate::record::{DepEdge, PackageRecord};
    use heart::Language;

    let mut edge = DepEdge::runtime("libc");
    edge.optional = true;
    let record = PackageRecord::from_parts(
        Language::Rust,
        "memchr",
        "2.8.3",
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        vec![edge.clone()],
    );
    let mut catalog = VersionedCatalog::open().expect("open");
    catalog.put_record(&record).expect("put");
    let stored = catalog
        .get_edge("rust", "memchr", "2.8.3", "libc")
        .expect("libc");
    assert_eq!(stored.payload_hash, super::edge_fact::hash_edge(&edge));
    assert_eq!(stored.optional, "1");
    let mut required = record.clone();
    required.edges[0].optional = false;
    let sync = catalog.sync_edges(&required).expect("flag");
    assert_eq!(sync.revised, 1);
    assert_eq!(sync.unchanged, 0);
    let replay = catalog.sync_edges(&required).expect("replay");
    assert_eq!(replay.revised, 0);
    assert_eq!(replay.unchanged, 1);
    assert_eq!(replay.commit, None);
}
