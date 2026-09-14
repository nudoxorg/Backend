//! Proves typed package URL admission and profile binding.

use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion, RustEdition,
    Stage, TypeScriptSource,
};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageEcosystem, PackageProfileMismatch,
    PackageUrl, PackageUrlError,
};

#[test]
fn all_language_ecosystems_retain_exact_components_and_bind_only_their_profile() {
    let rows = [
        (
            "pkg:cargo/serde@1.0.229",
            LanguageProfile::Rust(RustEdition::Rust2024),
            PackageEcosystem::Cargo,
            "serde",
            "1.0.229",
        ),
        (
            "pkg:npm/%40types/node@24.3.0",
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            PackageEcosystem::Npm,
            "node",
            "24.3.0",
        ),
        (
            "pkg:pypi/requests@2.32.5",
            LanguageProfile::Python(PythonVersion::Python314),
            PackageEcosystem::Pypi,
            "requests",
            "2.32.5",
        ),
        (
            "pkg:golang/golang.org/x/text@0.29.0",
            LanguageProfile::Go(GoVersion::Go125),
            PackageEcosystem::Golang,
            "text",
            "0.29.0",
        ),
        (
            "pkg:maven/org.slf4j/slf4j-api@2.0.17",
            LanguageProfile::Java(JavaRelease::Java25),
            PackageEcosystem::Maven,
            "slf4j-api",
            "2.0.17",
        ),
        (
            "pkg:nuget/System.Text.Json@9.0.9",
            LanguageProfile::CSharp(CSharpVersion::CSharp14),
            PackageEcosystem::Nuget,
            "System.Text.Json",
            "9.0.9",
        ),
        (
            "pkg:generic/sqlite@3.50.4?download_url=https%3A%2F%2Fsqlite.org#src",
            LanguageProfile::C(CStandard::C23),
            PackageEcosystem::Generic,
            "sqlite",
            "3.50.4",
        ),
    ];

    for (spelling, profile, ecosystem, name, version) in rows {
        let package = PackageUrl::try_from(spelling.to_owned()).expect("valid pinned package URL");
        assert_eq!(package.ecosystem, ecosystem);
        assert_eq!(&package[package.name], name);
        assert_eq!(&package[package.version], version);
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(41),
                profile,
                stage: Stage::LowerIr,
            },
            package,
        )
        .expect("ecosystem belongs to selected profile");
        assert_eq!(request.ecosystem, ecosystem);
        assert_eq!(request.target.profile, profile);
    }
}

#[test]
fn package_url_rejects_unpinned_noncanonical_and_cross_language_inputs() {
    let unpinned = PackageUrl::try_from("pkg:cargo/serde".to_owned())
        .expect_err("unpinned package must not enter resolution");
    assert_eq!(unpinned.error, PackageUrlError::Version);

    let unordered = PackageUrl::try_from("pkg:cargo/serde@1?z=1&a=2".to_owned())
        .expect_err("qualifier order is canonical authority");
    assert!(matches!(
        unordered.error,
        PackageUrlError::QualifierOrder { .. }
    ));

    let package =
        PackageUrl::try_from("pkg:cargo/serde@1.0.229".to_owned()).expect("valid package URL");
    let mismatch = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(7),
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            stage: Stage::LowerIr,
        },
        package,
    )
    .expect_err("npm profile cannot admit cargo coordinate");
    assert_eq!(
        mismatch,
        PackageProfileMismatch {
            profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ecosystem: PackageEcosystem::Cargo,
        }
    );
}
