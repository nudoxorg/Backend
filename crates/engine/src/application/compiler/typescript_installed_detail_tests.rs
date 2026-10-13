use super::*;
use crate::application::typescript_host::TypeScriptProjectHostError as E;
use backend_library::interface::{
    TypeScriptAuthorityFailureKind as F, TypeScriptCompilerAdmissionCause as C,
    TypeScriptCompilerApiExport as A, TypeScriptCompilerPackageRole as R,
};
use backend_semantic::vocabulary::TypeScriptSource;

fn package_path() -> Box<Path> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/private/authority.secret/ts-\xff".to_vec(),
        ))
        .into_boxed_path()
    }
    #[cfg(not(unix))]
    {
        std::path::PathBuf::from("C:/private/authority.secret/typescript").into_boxed_path()
    }
}

fn selected(error: E, version: &str) -> PackageAuthorityError {
    PackageAuthorityError::TypeScriptProjectHost(E::CompilerPackageAdmission {
        package_version: version.into(),
        role: R::SelectedCompiler,
        source: Box::new(error),
    })
}

#[test]
fn installed_typescript_limits_retain_version_counts_and_exact_original_failure_kind() {
    let cases = [
        (
            E::ModuleFileLimit {
                package_root: package_path(),
                observed: 257,
                maximum: 256,
            },
            C::RuntimeFiles {
                observed: 257,
                maximum: 256,
            },
            F::HostModuleFileLimit,
        ),
        (
            E::ModuleEntryLimit {
                package_root: package_path(),
                observed: 4097,
                maximum: 4096,
            },
            C::FilesystemEntries {
                observed: 4097,
                maximum: 4096,
            },
            F::HostModuleFileLimit,
        ),
        (
            E::ModuleDirectoryDepth {
                package_root: package_path(),
                observed: 9,
                maximum: 8,
            },
            C::DirectoryDepth {
                observed: 9,
                maximum: 8,
            },
            F::HostModuleFileLimit,
        ),
        (
            E::FileTooLarge {
                path: package_path(),
                observed: 1025,
                maximum: 1024,
            },
            C::InputBytes {
                observed: 1025,
                maximum: 1024,
            },
            F::HostFileTooLarge,
        ),
    ];
    for (error, cause, kind) in cases {
        let error = selected(error, "5.9.3");
        assert_eq!(
            super::super::compiler_typescript_failure::failure_kind(&error),
            Some(kind)
        );
        let detail = super::super::compiler_typescript_failure::admission_detail(&error).unwrap();
        assert_eq!(detail.cause(), cause);
        assert_eq!(detail.version().unwrap().as_str(), "5.9.3");
        let request = ApplicationCompilerRequest {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
            source: "export const value = 1;",
        };
        let terminal = package_authority_terminal(
            ContentId::from_canonical_bytes(b"selected-install"),
            request,
            request_source(request).unwrap(),
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::TypeScriptCompiler,
            },
            error,
        );
        let failure =
            backend_library::PackageCompilerFailure::from_package_terminal("main.ts", &terminal)
                .unwrap()
                .unwrap();
        assert_eq!(failure.kind_tag(), kind.kind_tag());
        assert_eq!(failure.detail(), detail.detail());
        assert_eq!(failure.recipe_identity(), None);
        assert!(!failure.requires_tool_configuration());
        let bytes = failure.encode_bounded_json().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("/private"));
        assert!(!text.contains("authority.secret"));
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(
            backend_library::PackageCompilerFailure::decode_bounded_json(&bytes).unwrap(),
            failure
        );
    }
}

#[test]
fn installed_typescript_api_capabilities_remain_closed_without_raw_export_or_path_fallback() {
    for export in [
        None,
        Some(A::GetModeForUsageLocation),
        Some(A::GetEmitScriptTarget),
        Some(A::GetEmitModuleKind),
        Some(A::GetEmitModuleResolutionKind),
        Some(A::GetEmitModuleDetectionKind),
        Some(A::GetESModuleInterop),
        Some(A::GetAllowSyntheticDefaultImports),
        Some(A::MatchFiles),
    ] {
        let error = PackageAuthorityError::TypeScriptProjectHost(match export {
            None => E::CompilerApiEntryMissing {
                package_root: package_path(),
                package_version: "7.0.2".into(),
                entrypoint: package_path(),
            },
            Some(export) => E::CompilerApiExportMissing {
                package_root: package_path(),
                package_version: "5.9.3".into(),
                entrypoint: package_path(),
                export: export.name().into(),
            },
        });
        assert_eq!(
            super::super::compiler_typescript_failure::failure_kind(&error),
            Some(F::HostCompilerApiBridge)
        );
        let detail = super::super::compiler_typescript_failure::admission_detail(&error).unwrap();
        assert_eq!(detail.role(), R::SelectedCompiler);
        assert!(detail.detail().contains("JavaScript Compiler API"));
        assert!(!detail.detail().contains("/private"));
        match export {
            None => assert!(detail.detail().contains("7.0.2")),
            Some(export) => assert!(detail.detail().contains(export.name())),
        }
    }
    let error = PackageAuthorityError::TypeScriptProjectHost(E::CompilerApiExportMissing {
        package_root: package_path(),
        package_version: "5.9.3".into(),
        entrypoint: package_path(),
        export: "/private/raw-export-name".repeat(100_000).into(),
    });
    assert_eq!(
        super::super::compiler_typescript_failure::failure_kind(&error),
        Some(F::HostCompilerApiBridge)
    );
    assert!(super::super::compiler_typescript_failure::admission_detail(&error).is_none());
    assert!(!F::HostCompilerApiBridge.detail().contains("raw-export"));
    let error = selected(
        E::ModuleFileLimit {
            package_root: package_path(),
            observed: 257,
            maximum: 256,
        },
        &"malicious version /private/authority.secret".repeat(100_000),
    );
    let detail = super::super::compiler_typescript_failure::admission_detail(&error).unwrap();
    assert!(detail.version().is_none());
    assert!(detail.detail().contains("version unavailable"));
    assert!(detail.detail().len() < 384);
    assert!(!detail.detail().contains("authority.secret"));
}
