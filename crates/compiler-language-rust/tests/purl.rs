//! PURL grammar laws: borrowed success and source-preserving typed rejection.

use compiler_languages_rust::{RustPackageUrl, RustPurlError};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use compiler_languages_rust::RustToolchain;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

#[test]
fn workspace_member_is_analyzed_under_its_declared_edition() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-purl-{nonce}-{sequence}"));
    assert!(fs::create_dir_all(&root).is_ok());
    assert!(fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2015\"\n\n[lib]\npath = \"crate_root.rs\"\n\n[workspace]\nmembers = [\".\"]\n",
    )
    .is_ok());
    assert!(
        fs::write(
            root.join("crate_root.rs"),
            "pub fn answer() -> u32 { 42 }\n"
        )
        .is_ok()
    );

    let Ok(toolchain) = RustToolchain::discover(PathBuf::from("rustc")) else {
        assert!(false, "discover toolchain");
        return;
    };
    let cancelled = AtomicBool::new(false);
    let Ok(parsed) = RustPackageUrl::parse("cargo:member@0.1.0") else {
        assert!(false, "fixture PURL");
        return;
    };
    let Ok(located) = parsed.locate(&root, &toolchain, None, &cancelled) else {
        assert!(false, "locate fixture member");
        return;
    };
    assert!(located.from_workspace());
    assert_eq!(
        located.project().source_path.file_name(),
        Some(std::ffi::OsStr::new("crate_root.rs"))
    );
    assert!(fs::remove_dir_all(root).is_ok());
}
