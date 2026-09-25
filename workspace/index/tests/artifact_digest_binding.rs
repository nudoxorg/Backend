//! npm integrity and a NuGet package hash land on the same versioned row
//! as a crates checksum. A later git revision does not replace either digest,
//! and the dependency edges written with the publish stay on the tip.

use heart::Language;
use index::{
    engine::turso_vc::VersionedCatalog,
    pid::{ContentDigest, VersionPid, artifact_digest},
    record::PackageRecord,
    upstream::{
        catalog::CatalogEvent, merge_document_facts, npm_changes::parse_changes,
        nuget_catalog::parse_leaf, pypi_document_facts, pypi_updates::parse_updates,
    },
};

fn published_from(event: &CatalogEvent, ecosystem: Language) -> PackageRecord {
    let CatalogEvent::Published {
        name,
        version,
        dependencies,
        checksum,
    } = event
    else {
        panic!("publish");
    };
    let digest = artifact_digest(checksum.as_deref().expect("checksum")).expect("artifact");
    PackageRecord::from_parts(
        ecosystem,
        name,
        version,
        None,
        None,
        Vec::new(),
        None,
        None,
        false,
        dependencies.clone(),
    )
    .with_content(digest)
}

#[test]
fn npm_and_nuget_digests_survive_a_later_git_revision() {
    let npm = parse_changes(
        br#"{"results":[{"seq":1,"id":"lodash","doc":{
            "dist-tags":{"latest":"4.17.21"},
            "versions":{"4.17.21":{
                "dependencies":{"ms":"^2"},
                "dist":{"integrity":"sha512-MH93Wm7R3U7jSJ/8hQmRR4eAW6CZ1vyi3tn3nhjDdQrx8G5dXLmpUte6ITcewxiwATgcMfBiVj+I5D6zhdC0dA== sha1-QsfKwaItloXI7UjFbiE7DiF26VI=","shasum":"0123456789abcdef0123456789abcdef01234567"}
            }}
        }}],"last_seq":1}"#,
        0,
    )
    .expect("npm")
    .events
    .into_iter()
    .next()
    .expect("event");
    let nuget = parse_leaf(
        br#"{
            "@type": "PackageDetails",
            "id": "Newtonsoft.Json",
            "version": "13.0.3",
            "packageHash": "gNTwQWwUA3adfelbX8plB64gYNCnXT1uKLszJM5i0fFEMxJrITma0J6ON/bDJui4te8/UIFNgWNg5T1Nk4JyQQ==",
            "packageHashAlgorithm": "SHA512",
            "dependencyGroups": [{"dependencies": [{"id": "System.Runtime"}]}]
        }"#,
    )
    .expect("nuget")
    .expect("event");

    let mut catalog = VersionedCatalog::open().expect("catalog");
    let lodash = published_from(&npm, Language::Typescript);
    let json = published_from(&nuget, Language::CSharp);
    catalog.put_record(&lodash).expect("lodash");
    catalog.put_record(&json).expect("json");

    let git = ContentDigest::GitSha1([0xab; 20]);
    for (ecosystem, name, version) in [
        ("typescript", "lodash", "4.17.21"),
        ("csharp", "Newtonsoft.Json", "13.0.3"),
    ] {
        let mut again = catalog
            .materialize(ecosystem, name, version)
            .expect("read")
            .expect("row");
        let artifact = again.content.expect("artifact");
        assert!(artifact.is_artifact());
        assert!(matches!(artifact, ContentDigest::Sha512(_)));
        again.content = Some(git);
        catalog.put_record(&again).expect("git observation");
        let tip = catalog
            .materialize(ecosystem, name, version)
            .expect("tip")
            .expect("row");
        assert_eq!(tip.content, Some(artifact));
        assert_eq!(tip.edges.len(), 1);
        let fact = catalog.get(ecosystem, name, version).expect("fact");
        assert_eq!(
            fact.version_pid,
            VersionPid::mint(ecosystem, name, version).local_name()
        );
    }
}

#[test]
fn a_pypi_sdist_sha256_binds_through_the_follower_merge() {
    let rss = br#"<rss><channel>
        <item><title>requests 2.31.0</title><pubDate>Wed, 01 Jan 2020 00:00:00 GMT</pubDate></item>
    </channel></rss>"#;
    let mut event = parse_updates(rss, "")
        .expect("rss")
        .events
        .into_iter()
        .next()
        .expect("publish");
    assert!(event.checksum().is_none());

    let hex = "cd".repeat(32);
    let body = format!(
        r#"{{"info":{{"requires_dist":["urllib3>=1.21.1","charset-normalizer"]}},"urls":[
            {{"packagetype":"bdist_wheel","digests":{{"sha256":"{hex}"}}}},
            {{"packagetype":"sdist","digests":{{"sha256":"abcd"}}}},
            {{"packagetype":"sdist","digests":{{"sha256":"{hex}"}}}}
        ]}}"#
    );
    let facts = pypi_document_facts(body.as_bytes());
    let from_json = artifact_digest(facts.checksum.as_deref().expect("sdist")).expect("sha256");
    merge_document_facts(&mut event, facts);
    let from_event = artifact_digest(event.checksum().expect("event")).expect("sha256");
    assert_eq!(from_json, from_event);
    assert!(matches!(from_event, ContentDigest::Sha256(_)));

    let mut held = CatalogEvent::Published {
        name: "requests".into(),
        version: "2.31.0".into(),
        dependencies: vec![index::record::DepEdge::runtime("certifi")],
        checksum: Some("ab".repeat(32)),
    };
    merge_document_facts(&mut held, pypi_document_facts(body.as_bytes()));
    assert_eq!(held.checksum(), Some("ab".repeat(32).as_str()));
    assert_eq!(
        held.dependencies(),
        &index::record::runtime_edges_from_names(&["charset-normalizer", "urllib3"])[..]
    );

    let record = published_from(&event, heart::Language::Python);
    let mut catalog = VersionedCatalog::open().expect("catalog");
    catalog.put_record(&record).expect("put");
    let mut again = catalog
        .materialize("python", "requests", "2.31.0")
        .expect("read")
        .expect("row");
    assert_eq!(again.content, Some(from_event));
    assert_eq!(again.edges.len(), 2);
    again.content = Some(ContentDigest::GitSha1([0x11; 20]));
    catalog.put_record(&again).expect("git");
    let tip = catalog
        .materialize("python", "requests", "2.31.0")
        .expect("tip")
        .expect("row");
    assert_eq!(tip.content, Some(from_event));
    assert_eq!(tip.edges.len(), 2);
    assert_eq!(
        catalog
            .get("python", "requests", "2.31.0")
            .expect("fact")
            .version_pid,
        VersionPid::mint("python", "requests", "2.31.0").local_name()
    );
}
