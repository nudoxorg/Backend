//! PURL grammar laws: borrowed success and source-preserving typed rejection.

use compiler_languages_rust::{RustPackageUrl, RustPurlError};

#[test]
fn malformed_purls_retain_the_complete_rejected_input() {
    let cases = [
        ("https:crate@1.0.0", "wrong scheme"),
        ("cargo:crate", "missing version"),
        ("cargo:@1.0.0", "empty name"),
        ("cargo:crate@not-a-version", "malformed version"),
    ];
    for (input, label) in cases {
        let error = match RustPackageUrl::parse(input) {
            Ok(_) => panic!("accepted {label}"),
            Err(error) => error,
        };
        match error {
            RustPurlError::WrongScheme { purl }
            | RustPurlError::MissingVersion { purl }
            | RustPurlError::EmptyName { purl }
            | RustPurlError::MalformedVersion { purl, .. } => assert_eq!(purl, input),
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

#[test]
fn valid_purl_borrows_exact_components() {
    let input = String::from("cargo:fixture-name@1.2.3-alpha.1");
    let parsed = match RustPackageUrl::parse(&input) {
        Ok(parsed) => parsed,
        Err(error) => panic!("valid PURL rejected: {error}"),
    };
    assert_eq!(parsed.name(), "fixture-name");
    assert_eq!(parsed.version(), "1.2.3-alpha.1");
}
