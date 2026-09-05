//! Proves CLI and MCP package commands share one typed package admission path.

use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
use interface_core::{ApplicationInput, CorrelationId, PackageEcosystem};
use interface_protocol::{
    AdapterErrorCause, AdapterErrorCode, McpDecode, McpRequest, decode_cli, decode_mcp,
};

#[test]
fn cli_and_mcp_admit_the_same_pinned_package_facts() {
    let cli = decode_cli(&[
        "compile-package".to_owned(),
        "17".to_owned(),
        "rust-2024".to_owned(),
        "lower-ir".to_owned(),
        "pkg:cargo/serde@1.0.229".to_owned(),
    ])
    .expect("typed CLI package command");
    let mcp = decode_mcp(
        br#"{"jsonrpc":"2.0","id":"package-17","method":"tools/call","params":{"name":"compile-package","arguments":{"correlation":17,"profile":"rust-2024","stage":"lower-ir","purl":"pkg:cargo/serde@1.0.229"}}}"#,
    );
    let McpDecode::Accepted(envelope) = mcp else {
        panic!("typed MCP package command was rejected");
    };
    let McpRequest::Application(mcp) = envelope.request else {
        panic!("MCP package command did not enter application dispatch");
    };
    assert_package(cli);
    assert_package(mcp);
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
                compiler_vocabulary::TypeScriptSource::TypeScript
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
