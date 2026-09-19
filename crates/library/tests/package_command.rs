//! Proves the CLI package command enters one typed package admission path.

use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use backend_library::interface::{ApplicationInput, CorrelationId, PackageEcosystem};
use backend_library::protocol::{AdapterErrorCause, AdapterErrorCode, decode_cli};

#[test]
fn the_cli_admits_pinned_package_facts() {
    let cli = decode_cli(&[
        "compile-package".to_owned(),
        "17".to_owned(),
        "rust-2024".to_owned(),
        "lower-ir".to_owned(),
        "pkg:cargo/serde@1.0.229".to_owned(),
    ])
    .expect("typed CLI package command");
    assert_package(cli);
}

#[test]
fn transport_retains_package_parse_and_profile_mismatch_causes() {
    let unpinned = decode_cli(&[
        "compile-package".to_owned(),
        "1".to_owned(),
        "rust-2024".to_owned(),
        "lower-ir".to_owned(),
        "pkg:cargo/serde".to_owned(),
    ])
    .expect_err("unpinned package URL");
    assert_eq!(unpinned.code, AdapterErrorCode::InvalidPackageUrl);
    assert!(matches!(
        unpinned.cause,
        Some(AdapterErrorCause::PackageUrl(rejected))
            if rejected.text == "pkg:cargo/serde"
    ));

    let mismatch = decode_cli(&[
        "compile-package".to_owned(),
        "2".to_owned(),
        "typescript".to_owned(),
        "lower-ir".to_owned(),
        "pkg:cargo/serde@1.0.229".to_owned(),
    ])
    .expect_err("cross-language package URL");
    assert_eq!(mismatch.code, AdapterErrorCode::PackageProfileMismatch);
    assert!(matches!(
        mismatch.cause,
        Some(AdapterErrorCause::PackageProfile(cause))
            if cause.profile == LanguageProfile::TypeScript(
                backend_semantic::vocabulary::TypeScriptSource::TypeScript
            ) && cause.ecosystem == PackageEcosystem::Cargo
    ));
}

fn assert_package(input: ApplicationInput) {
    let ApplicationInput::CompilePackage(request) = input else {
        panic!("package command changed application variant");
    };
    assert_eq!(request.target.correlation, CorrelationId(17));
    assert_eq!(
        request.target.profile,
        LanguageProfile::Rust(RustEdition::Rust2024)
    );
    assert_eq!(request.target.stage, Stage::LowerIr);
    assert_eq!(request.ecosystem, PackageEcosystem::Cargo);
    let package = request.as_ref();
    assert_eq!(&package[package.name], "serde");
    assert_eq!(&package[package.version], "1.0.229");
}
