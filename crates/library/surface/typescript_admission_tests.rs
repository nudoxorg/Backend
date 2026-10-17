use super::*;
use crate::interface::{
    CompilerDiagnostic, CompilerTerminal, SourceAuthority, TypeScriptCompilerAdmissionCause as C,
    TypeScriptCompilerAdmissionDetail as D, TypeScriptCompilerApiExport as E,
    TypeScriptCompilerPackageRole as R, TypeScriptCompilerVersion as V,
};
use backend_semantic::vocabulary::{Language, Stage};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn required<T>(value: Option<T>, context: &'static str) -> TestResult<T> {
    value.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, context).into())
}

fn package_result<T>(value: Result<T, ProductAdmissionError>) -> TestResult<T> {
    value.map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()).into()
    })
}

fn failure(detail: D, path: &str) -> TestResult<PackageCompilerFailure> {
    let native = b"private /home/user/sdk authority.secret and raw compiler transcript";
    let diagnostic = required(
        CompilerDiagnostic::from_native(native, native.len(), false),
        "admit test compiler diagnostic",
    )?
    .with_typescript_admission(detail);
    let terminal = CompilerTerminal::AuthorityAdmission {
        source: SourceAuthority {
            identity: ContentId::from_canonical_bytes(b"source"),
            byte_len: 6,
        },
        language: Language::TypeScript,
        stage: Stage::LowerIr,
        phase: crate::interface::AuthorityPhase::Resolve,
        class: crate::interface::AuthorityDiagnosticClass::Authority,
        diagnostic: Some(diagnostic),
    };
    required(
        package_result(PackageCompilerFailure::from_package_terminal(
            path, &terminal,
        ))?,
        "project test terminal as package compiler failure",
    )
}

#[test]
fn typescript_admission_all_categories_keep_closed_kinds_and_roundtrip_facts() -> TestResult<()> {
    for cause in [
        C::RuntimeFiles {
            observed: 257,
            maximum: 256,
        },
        C::FilesystemEntries {
            observed: 4097,
            maximum: 4096,
        },
        C::DirectoryDepth {
            observed: 9,
            maximum: 8,
        },
        C::InputBytes {
            observed: 100,
            maximum: 99,
        },
        C::MissingApiEntry,
        C::MissingApiExport {
            export: E::GetModeForUsageLocation,
        },
    ] {
        let version = required(V::new("7.0.2"), "admit selected TypeScript version")?;
        let detail = required(
            D::new(R::SelectedCompiler, Some(version), cause),
            "admit TypeScript compiler facts",
        )?;
        let kind = detail.failure_kind();
        let failure = failure(detail.clone(), "main.ts")?;
        assert_eq!(failure.kind_tag(), kind.kind_tag());
        assert_eq!(failure.phase(), PackageCompilerFailurePhase::Setup);
        assert_eq!(failure.recipe_identity(), None);
        assert_eq!(failure.detail(), detail.detail());
        assert!(failure.detail().contains("typescript 7.0.2"));
        assert!(failure.detail().contains("before retrying"));
        assert!(failure.detail().len() <= MAX_COMPILER_FRAGMENT_DETAIL_BYTES);
        let bytes = package_result(failure.encode_bounded_json())?;
        let wire = String::from_utf8(bytes.clone())?;
        for private in ["/home/user", "authority.secret", "raw compiler transcript"] {
            assert!(!wire.contains(private));
        }
        let reopened = package_result(PackageCompilerFailure::decode_bounded_json(&bytes))?;
        assert_eq!(reopened, failure);
        assert!(reopened.retained_diagnostic_for_local_debug().is_none());
    }
    Ok(())
}

#[test]
fn typescript_admission_versions_and_api_export_names_are_bounded_closed_values() -> TestResult<()>
{
    for version in ["5.9.3", "7.0.2", "5.9.3-rc.1+build-2"] {
        assert_eq!(
            required(V::new(version), "admit valid version")?.as_str(),
            version
        );
    }
    for version in [
        "/private/sdk",
        "5.9.3\nsecret",
        "5.9.3-/private/key",
        "05.9.3",
        "5.9",
        "5.9.3+",
        "5.9.3++secret",
    ] {
        assert!(V::new(version).is_none(), "{version}");
        assert!(serde_json::from_value::<V>(serde_json::json!(version)).is_err());
    }
    assert!(V::new(&"9".repeat(8192)).is_none());
    let version = required(V::new("5.9.3"), "admit selected TypeScript version")?;
    for export in [
        E::GetModeForUsageLocation,
        E::GetEmitScriptTarget,
        E::GetEmitModuleKind,
        E::GetEmitModuleResolutionKind,
        E::GetEmitModuleDetectionKind,
        E::GetESModuleInterop,
        E::GetAllowSyntheticDefaultImports,
        E::MatchFiles,
    ] {
        assert_eq!(E::from_name(export.name()), Some(export));
        assert_eq!(
            serde_json::to_value(export)?,
            serde_json::json!(export.name())
        );
        assert!(
            D::new(
                R::SelectedCompiler,
                Some(version.clone()),
                C::MissingApiExport { export }
            )
            .is_some()
        );
    }
    assert!(E::from_name(&"/private/raw-output".repeat(8192)).is_none());
    Ok(())
}

#[test]
fn typescript_admission_worst_case_packet_and_forged_details_are_bounded() -> TestResult<()> {
    let version = format!("5.9.3-{}", "a".repeat(122));
    assert_eq!(version.len(), V::MAX_BYTES);
    let version = required(V::new(&version), "admit maximum-length version")?;
    let detail = required(
        D::new(
            R::SelectedCompiler,
            Some(version),
            C::RuntimeFiles {
                observed: u64::MAX,
                maximum: u64::MAX - 1,
            },
        ),
        "admit worst-case compiler facts",
    )?;
    let path = "\"".repeat(PackageCompilerFailure::MAX_RELATIVE_PATH_BYTES);
    let failure = failure(detail, &path)?;
    assert!(!failure.detail_truncated());
    let bytes = package_result(failure.encode_bounded_json())?;
    assert!(bytes.len() <= PackageCompilerFailure::MAX_ENCODED_BYTES);
    assert_eq!(
        package_result(PackageCompilerFailure::decode_bounded_json(&bytes))?,
        failure
    );
    let value = serde_json::to_value(&failure)?;
    for field in [
        "schema_version",
        "version",
        "raw_output",
        "maximum",
        "detail",
        "kind_tag",
    ] {
        let mut forged = value.clone();
        match field {
            "schema_version" => {
                forged["cause"]["fault"]["diagnostic"]["typescript_admission"]["schema_version"] =
                    serde_json::json!(2)
            }
            "version" => {
                forged["cause"]["fault"]["diagnostic"]["typescript_admission"]["version"] =
                    serde_json::json!("5.9.3-/private/sdk")
            }
            "raw_output" => {
                forged["cause"]["fault"]["diagnostic"]["typescript_admission"]["raw_output"] =
                    serde_json::json!("secret")
            }
            "maximum" => {
                forged["cause"]["fault"]["diagnostic"]["typescript_admission"]["cause"]["maximum"] =
                    serde_json::json!(u64::MAX)
            }
            "detail" => forged["detail"] = serde_json::json!("unrelated sanitized explanation"),
            "kind_tag" => {
                forged["kind_tag"] = serde_json::json!("typescript_host_compiler_api_bridge")
            }
            _ => unreachable!(),
        }
        assert!(
            serde_json::from_value::<PackageCompilerFailure>(forged).is_err(),
            "{field}"
        );
    }
    Ok(())
}

#[test]
fn typescript_admission_does_not_misidentify_a_discovered_package_as_selected() -> TestResult<()> {
    let version = required(V::new("5.9.3"), "admit discovered TypeScript version")?;
    let detail = required(
        D::new(
            R::DiscoveredProjectPackage,
            Some(version),
            C::FilesystemEntries {
                observed: 4097,
                maximum: 4096,
            },
        ),
        "admit discovered package facts",
    )?;
    assert!(
        detail
            .detail()
            .starts_with("Discovered TypeScript package typescript 5.9.3")
    );
    assert!(!detail.detail().contains("Selected"));
    assert!(D::new(detail.role(), detail.version().cloned(), C::MissingApiEntry).is_none());
    Ok(())
}

#[test]
fn typescript_admission_constructor_rejects_invalid_counts_and_api_pairings() {
    for cause in [
        C::RuntimeFiles {
            observed: 256,
            maximum: 256,
        },
        C::FilesystemEntries {
            observed: 0,
            maximum: 4096,
        },
        C::DirectoryDepth {
            observed: 8,
            maximum: 8,
        },
        C::InputBytes {
            observed: 99,
            maximum: 100,
        },
    ] {
        assert!(D::new(R::SelectedCompiler, V::new("5.9.3"), cause).is_none());
    }
    for cause in [
        C::MissingApiEntry,
        C::MissingApiExport {
            export: E::MatchFiles,
        },
    ] {
        assert!(D::new(R::SelectedCompiler, None, cause).is_none());
        for role in [R::DiscoveredProjectPackage, R::Unspecified] {
            assert!(D::new(role, V::new("5.9.3"), cause).is_none());
        }
    }
    assert_eq!(D::SCHEMA_VERSION, 1);
}
