//! npm integrity and a NuGet package hash land on the same versioned row
//! as a crates checksum. A later git revision does not replace either digest,
//! and the dependency edges written with the publish stay on the tip.

use heart::Language;
use index::{
    engine::turso_vc::VersionedCatalog,
    pid::{ContentDigest, VersionPid, artifact_digest},
    record::PackageRecord,
    upstream::{catalog::CatalogEvent, npm_changes::parse_changes, nuget_catalog::parse_leaf},
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
    PackageRecord::published(ecosystem, name, version, dependencies).with_content(digest)
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
