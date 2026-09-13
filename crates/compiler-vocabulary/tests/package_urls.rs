use compiler_vocabulary::{Language, PackageType, PackageUrl, PackageUrlError, RegistryEcosystem};

#[test]
fn one_owned_url_exposes_borrowed_typed_components() {
    let url = PackageUrl::parse(
        "pkg:generic/sqlite@3.50.4?download_url=https%3A%2F%2Fsqlite.org#src/core",
    )
    .expect("canonical package URL");

    assert_eq!(
        url.as_str().as_ptr(),
        (&url[url.name]).as_ptr().wrapping_sub(12)
    );
    assert_eq!(url.package_type(), PackageType::Generic);
    assert_eq!(url.package_type().language(), Language::Clang);
    assert_eq!(url.package_type().registry(), Some(RegistryEcosystem::Cpp));
    assert_eq!(url.namespace(), None);
    assert_eq!(url.name(), "sqlite");
    assert_eq!(url.lineage_name(), "sqlite");
    assert_eq!(url.version(), "3.50.4");
    assert_eq!(
        url.qualifiers(),
        Some("download_url=https%3A%2F%2Fsqlite.org")
    );
    assert_eq!(url.subpath(), Some("src/core"));
}

#[test]
fn package_identity_and_registry_protocol_are_distinct_facts() {
    let rows = [
        (PackageType::Cargo, RegistryEcosystem::Cargo, Language::Rust),
        (
            PackageType::Npm,
            RegistryEcosystem::Npm,
            Language::TypeScript,
        ),
        (PackageType::Pypi, RegistryEcosystem::Pypi, Language::Python),
        (PackageType::Golang, RegistryEcosystem::Golang, Language::Go),
        (PackageType::Maven, RegistryEcosystem::Maven, Language::Java),
        (
            PackageType::Nuget,
            RegistryEcosystem::Nuget,
            Language::CSharp,
        ),
        (
            PackageType::Generic,
            RegistryEcosystem::Cpp,
            Language::Clang,
        ),
    ];
    for (package_type, registry, language) in rows {
        assert_eq!(package_type.registry(), Some(registry));
        assert_eq!(registry.package_type(), package_type);
        assert_eq!(package_type.language(), language);
    }
}

#[test]
fn noncanonical_spellings_cannot_mint_competing_identities() {
    for text in [
        "PKG:cargo/serde@1",
        "pkg:Cargo/serde@1",
        "pkg:cargo/%73erde@1",
        "pkg:cargo/serde@1?z=1&a=2",
        "pkg:cargo/serde@1#src?source=other",
        "pkg:cargo/serde",
    ] {
        assert!(PackageUrl::parse(text).is_err(), "accepted {text}");
    }

    assert_eq!(
        PackageUrl::parse("pkg:cargo/serde@1?source=%2f")
            .expect_err("lowercase escapes are not canonical")
            .error,
        PackageUrlError::Escape { offset: 25 }
    );
}

#[test]
fn serde_round_trip_preserves_the_exact_admitted_url() {
    let url = PackageUrl::parse("pkg:npm/%40types/node@24.3.0?arch=x86_64#types")
        .expect("canonical scoped package");
    let wire = serde_json::to_string(&url).expect("serialize package URL");
    let reopened: PackageUrl = serde_json::from_str(&wire).expect("reopen package URL");
    assert_eq!(reopened, url);
    assert_eq!(reopened.identity, url.identity);
    assert_eq!(reopened.lineage_name(), "%40types/node");
}
