use super::*;

fn fact(hash: &str) -> PackageFact {
    PackageFact {
        version_pid: super::edge_fact::version_pid_of("rust", "memchr", "2.8.3"),
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
    assert_eq!(tip.version_pid, fact("bbb").version_pid);
    assert_ne!(tip.version_pid, "memchr");
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
        kind: crate::enums::EdgeKind::Build,
        optional: false,
        dep_ecosystem: None,
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
    assert_eq!(tip.edges.len(), 2);
    assert_eq!(tip.edges[0].requirement.as_deref(), Some("^1"));
    assert!(tip.edges[0].optional);
    assert_eq!(tip.edges[0].class, DepClass::Runtime);
    assert_eq!(tip.edges[1].class, DepClass::Dev);
    assert_eq!(tip.edges[1].requirement.as_deref(), Some("^9"));
    let mut duplicate = first.clone();
    duplicate.requirement = Some(SmolStr::new("^2"));
    let mut again = record.clone();
    again.edges.push(duplicate);
    let sync = catalog.sync_edges(&again).expect("same class");
    assert_eq!(sync.revised, 0);
    assert_eq!(sync.unchanged, 2);
    let kept = catalog
        .materialize("rust", "memchr", "2.8.3")
        .expect("join")
        .expect("row");
    assert_eq!(kept.edges[0].requirement.as_deref(), Some("^1"));
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

#[test]
fn dropping_a_version_tombstones_the_tip_and_keeps_the_prior_commit() {
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
        vec![DepEdge::runtime("libc"), DepEdge {
            name: "cc".into(),
            requirement: None,
            class: crate::record::DepClass::Build,
            kind: crate::enums::EdgeKind::Build,
            optional: false,
            dep_ecosystem: None,
        }],
    );
    let mut catalog = VersionedCatalog::open().expect("open");
    let FactWrite::Revised(written) = catalog.put_record(&record).expect("put") else {
        panic!("first write revises");
    };
    let FactWrite::Revised(dropped) = catalog
        .drop_version("rust", "memchr", "2.8.3")
        .expect("drop")
    else {
        panic!("a live row drops");
    };
    assert_ne!(written, dropped);
    assert!(
        catalog
            .materialize("rust", "memchr", "2.8.3")
            .expect("tip")
            .is_none()
    );
    assert!(catalog.scan_edges().is_empty());
    let prior = catalog
        .materialize_at("rust", "memchr", "2.8.3", written)
        .expect("history")
        .expect("row existed");
    assert_eq!(prior.edges.len(), 2);
    assert_eq!(
        catalog
            .drop_version("rust", "memchr", "2.8.3")
            .expect("again"),
        FactWrite::Unchanged
    );
}

#[test]
fn withdrawing_keeps_the_edges_and_a_second_withdraw_is_unchanged() {
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
    let FactWrite::Revised(written) = catalog.put_record(&record).expect("put") else {
        panic!("first write revises");
    };
    let live = catalog
        .resolve_version("rust", "memchr", "2.8.3")
        .expect("resolve")
        .expect("present");
    let crate::pid::Resolve::Live(live_kernel) = live else {
        panic!("published tip is live");
    };
    let FactWrite::Revised(_) = catalog
        .withdraw("rust", "memchr", "2.8.3")
        .expect("withdraw")
    else {
        panic!("yank revises");
    };
    let tip = catalog
        .materialize("rust", "memchr", "2.8.3")
        .expect("tip")
        .expect("row");
    assert!(tip.yanked);
    assert_eq!(tip.edges.len(), 1);
    assert_eq!(tip.edges[0].name.as_str(), "libc");
    let prior = catalog
        .materialize_at("rust", "memchr", "2.8.3", written)
        .expect("history")
        .expect("row");
    assert!(!prior.yanked);
    assert_eq!(prior.edges.len(), 1);
    assert_eq!(
        catalog.withdraw("rust", "memchr", "2.8.3").expect("again"),
        FactWrite::Unchanged
    );
    assert_eq!(
        catalog
            .withdraw("rust", "missing", "0.1.0")
            .expect("absent"),
        FactWrite::Unchanged
    );
    let yanked = catalog
        .resolve_version("rust", "memchr", "2.8.3")
        .expect("resolve")
        .expect("present");
    let crate::pid::Resolve::Tombstone(tombstone) = yanked else {
        panic!("yanked tip is a tombstone");
    };
    assert_eq!(tombstone.pid, live_kernel.pid);
    assert!(
        catalog
            .resolve_version("rust", "missing", "0.1.0")
            .expect("miss")
            .is_none()
    );
}

#[test]
fn a_full_git_sha_binds_content_and_a_short_rev_does_not() {
    use crate::{
        edge_project::records_from_ops_named,
        enums::SourceKind,
        ids::PackageStemId,
        pid::{BoundPid, ContentDigest, Resolve, VersionPid},
        protocol::{CatalogOp, FacetWire, SourceAcquisitionWire, VersionCoordinates},
    };
    use heart::{Language, PackageId};

    fn version_op(stem_id: PackageStemId, rev: &str) -> CatalogOp {
        CatalogOp::UpsertVersion {
            coordinates: VersionCoordinates {
                version_id: PackageId::from_uuid(uuid::Uuid::from_u128(12)),
                stem_id,
                version_canonical: "v1".into(),
                version_original: "v1".into(),
            },
            published_at: None,
            toolchain: None,
            license: None,
            edges: crate::protocol::EdgeSnapshot::unobserved(),
            facets: FacetWire::default(),
            source: Some(SourceAcquisitionWire {
                source_kind: SourceKind::Git,
                source_pack: None,
                source_rev: Some(rev.into()),
                registry_checksum: None,
                registry_package_uri: None,
            }),
        }
    }

    let sha = "0123456789abcdef0123456789abcdef01234567";
    let stem_id = PackageStemId::from_uuid(uuid::Uuid::from_u128(11));
    let ops = [version_op(stem_id, sha), version_op(stem_id, "abcdef")];
    let records = records_from_ops_named(&ops, Some((Language::Cpp, "example.test/repo")));
    assert_eq!(records.len(), 2);
    assert!(matches!(
        records[0].content,
        Some(ContentDigest::GitSha1(_))
    ));
    assert!(records[1].content.is_none());
    let mut catalog = VersionedCatalog::open().expect("open");
    catalog.put_record(&records[0]).expect("sha");
    catalog
        .put_record(&records[1])
        .expect("short rev keeps the sha");
    let Resolve::Live(kernel) = catalog
        .resolve_version("cpp", "example.test/repo", "v1")
        .expect("resolve")
        .expect("row")
    else {
        panic!("live");
    };
    assert_eq!(
        kernel.pid,
        BoundPid::Version(VersionPid::mint("cpp", "example.test/repo", "v1"))
    );
    assert_eq!(kernel.checksum, records[0].content);
    let other = "fedcba9876543210fedcba9876543210fedcba98";
    let mut rebuilt = records[0].clone();
    rebuilt.content = crate::pid::git_sha1(other);
    catalog.put_record(&rebuilt).expect("new bytes");
    let Resolve::Live(rebuilt_kernel) = catalog
        .resolve_version("cpp", "example.test/repo", "v1")
        .expect("resolve")
        .expect("row")
    else {
        panic!("live");
    };
    assert_eq!(rebuilt_kernel.pid, kernel.pid);
    assert_eq!(rebuilt_kernel.checksum, rebuilt.content);
    assert_ne!(rebuilt_kernel.checksum, kernel.checksum);
}

#[test]
fn a_registry_sha256_beats_a_git_sha_and_keeps_the_version_pid() {
    use crate::{
        edge_project::records_from_ops_named,
        enums::SourceKind,
        ids::PackageStemId,
        pid::{ContentDigest, Resolve},
        protocol::{CatalogOp, FacetWire, SourceAcquisitionWire, VersionCoordinates},
    };
    use heart::{Language, PackageId};

    let checksum = "ab".repeat(32);
    let stem_id = PackageStemId::from_uuid(uuid::Uuid::from_u128(21));
    let artifact = CatalogOp::UpsertVersion {
        coordinates: VersionCoordinates {
            version_id: PackageId::from_uuid(uuid::Uuid::from_u128(22)),
            stem_id,
            version_canonical: "1.0.0".into(),
            version_original: "1.0.0".into(),
        },
        published_at: None,
        toolchain: None,
        license: None,
        edges: crate::protocol::EdgeSnapshot::unobserved(),
        facets: FacetWire::default(),
        source: Some(SourceAcquisitionWire {
            source_kind: SourceKind::Git,
            source_pack: None,
            source_rev: Some("0123456789abcdef0123456789abcdef01234567".into()),
            registry_checksum: Some(checksum),
            registry_package_uri: None,
        }),
    };
    let records = records_from_ops_named(&[artifact], Some((Language::Rust, "memchr")));
    assert!(matches!(records[0].content, Some(ContentDigest::Sha256(_))));
    let mut catalog = VersionedCatalog::open().expect("open");
    catalog.put_record(&records[0]).expect("artifact");
    let mut git_record = records[0].clone();
    git_record.content = crate::pid::git_sha1("fedcba9876543210fedcba9876543210fedcba98");
    catalog
        .put_record(&git_record)
        .expect("git does not replace sha256");
    let Resolve::Live(kernel) = catalog
        .resolve_version("rust", "memchr", "1.0.0")
        .expect("resolve")
        .expect("row")
    else {
        panic!("live");
    };
    assert_eq!(kernel.checksum, records[0].content);
    assert!(crate::pid::sha256("abc").is_none());
}

#[test]
fn degree_histogram_matches_the_tip_walk_across_writes() {
    use crate::record::{DepClass, DepEdge, PackageRecord};
    use heart::Language;
    use smol_str::SmolStr;

    let classes = [
        DepClass::Runtime,
        DepClass::Dev,
        DepClass::Optional,
        DepClass::Build,
        DepClass::Peer,
    ];
    let mut catalog = VersionedCatalog::open().expect("open");
    for step in 0..48u32 {
        let package = format!("pkg-{}", step % 5);
        let version = format!("1.{}", step % 3);
        let mut edge = DepEdge::runtime(format!("dep-{}", step % 7));
        edge.class = classes[(step as usize) % classes.len()];
        if step % 11 == 0 {
            edge.dep_ecosystem = Some(Language::Python);
        }
        if step % 13 == 0 {
            edge.name = SmolStr::new(&package);
        }
        let mut record =
            PackageRecord::published(Language::Rust, &package, &version, &[] as &[&str]);
        record.edges = vec![edge];
        if step % 4 == 0 {
            record.edges.push(DepEdge::runtime("serde"));
        }
        catalog.put_record(&record).expect("put");
        if step % 9 == 0 {
            catalog
                .drop_version("rust", &package, &version)
                .expect("drop");
        }
        assert_eq!(
            catalog.dependents(),
            catalog.dependents_from_tips(),
            "step {step}"
        );
    }
}

#[test]
fn a_feed_republish_replaces_runtime_names_and_keeps_build() {
    use crate::{
        enums::{EdgeKind, EdgeSource},
        protocol::EdgeSnapshot,
        record::{DepClass, DepEdge, PackageRecord},
    };
    use heart::Language;

    let mut catalog = VersionedCatalog::open().expect("open");
    let mut record = PackageRecord::published(Language::Rust, "app", "1.0.0", &["serde"]);
    record.edges.push(DepEdge {
        name: "cc".into(),
        requirement: None,
        class: DepClass::Build,
        kind: EdgeKind::Build,
        optional: false,
        dep_ecosystem: None,
    });
    catalog.put_record(&record).expect("seed");

    let revised = PackageRecord::published(Language::Rust, "app", "1.0.0", &["tokio"]);
    let snapshot = EdgeSnapshot::feed(crate::edge_project::project_edges(
        Language::Rust,
        &revised.edges,
        EdgeSource::Feed,
    ));
    catalog.put_observed(&revised, &snapshot).expect("feed");

    let tip = catalog
        .materialize("rust", "app", "1.0.0")
        .expect("read")
        .expect("row");
    let mut pairs: Vec<_> = tip
        .edges
        .iter()
        .map(|edge| (edge.kind, edge.name.as_str()))
        .collect();
    pairs.sort_by_key(|pair| (pair.0.as_token(), pair.1));
    assert_eq!(pairs, vec![
        (EdgeKind::Build, "cc"),
        (EdgeKind::Runtime, "tokio")
    ]);
    assert_eq!(catalog.dependents(), catalog.dependents_from_tips());
}
