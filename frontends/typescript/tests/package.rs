#![allow(
    missing_docs,
    reason = "Integration test names are the law being exercised."
)]

use backend_frontend_typescript::{
    PackagePurl, PackagePurlError, TarballMember, TarballMemberKind, decode_packument,
    locate_package,
};
use std::str::FromStr;

#[test]
fn purl_valid_forms_round_trip() {
    for text in [
        "npm:lodash@4.18.1",
        "npm:@types/react@19.2.18",
        "npm:@scope/pkg@1.2.3/types/index.d.ts",
    ] {
        let purl = PackagePurl::from_str(text).expect("valid PURL");
        assert_eq!(purl.to_string(), text);
    }
}

#[test]
fn purl_rejects_each_malformed_class() {
    assert!(matches!(
        PackagePurl::from_str("pkg@1"),
        Err(PackagePurlError::Scheme { .. })
    ));
    assert!(matches!(
        PackagePurl::from_str("npm:@scope/pkg"),
        Err(PackagePurlError::MissingVersion { .. })
    ));
    assert!(matches!(
        PackagePurl::from_str("npm:@/pkg@1"),
        Err(PackagePurlError::EmptyName { .. })
    ));
    assert!(matches!(
        PackagePurl::from_str("npm:pkg@"),
        Err(PackagePurlError::MissingVersion { .. })
    ));
    assert!(matches!(
        PackagePurl::from_str("npm:pkg @1"),
        Err(PackagePurlError::Whitespace { .. })
    ));
}

#[test]
fn packument_selects_version_and_tolerates_external_fields() {
    let bytes = br#"{"dist-tags":{"latest":"2"},"versions":{"1":{"dist":{"version":"1","tarball":"one","integrity":"sha512-one"}},"2":{"dist":{"version":"2","tarball":"two","integrity":"sha512-two"},"extra":true}}}"#;
    let metadata = decode_packument(bytes, "2").expect("selected version");
    assert_eq!(metadata.tarball, "two");
    assert!(decode_packument(bytes, "3").is_err());
}

#[test]
fn locate_matches_monorepo_name_and_version() {
    let members = vec![
        member("package/package.json", br#"{"name":"other","version":"1"}"#),
        member(
            "packages/target/package.json",
            br#"{"name":"target","version":"1"}"#,
        ),
        member("packages/target/index.js", b"ok"),
    ];
    let purl = PackagePurl::from_str("npm:target@1/index.js").expect("valid PURL");
    let located = locate_package(&members, &purl).expect("matching root");
    assert_eq!(located.root, "packages/target");
    assert_eq!(
        located.subpath.map(|member| member.name.as_str()),
        Some("packages/target/index.js")
    );
}

fn member(name: &str, bytes: &[u8]) -> TarballMember {
    TarballMember {
        name: name.to_owned(),
        kind: TarballMemberKind::Regular,
        bytes: bytes.to_vec(),
    }
}
