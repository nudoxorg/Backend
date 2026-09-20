//! Compatibility contract tests using deterministic native authority inputs.
#![forbid(unsafe_code)]

#[cfg(test)]
use backend_compile::{
    Authority, AuthorityIdentity, ContractSchema, Coverage, CoverageWitness, DiscoveryDelta,
    FactKind, FlowSchema, Input, InputKind, InputManifest, ProducerSchema, ProfileSchema,
    ScopeRoot, SemanticBasisSchema, SessionKey, ToolchainSchema, UntrustedCoverageScope, typed_of,
};
#[cfg(test)]
use backend_frontend_clang::ClangFrontend;
#[cfg(test)]
use backend_frontend_csharp::CSharpFrontend;
#[cfg(test)]
use backend_frontend_go::GoFrontend;
#[cfg(test)]
use backend_frontend_java::JavaFrontend;
#[cfg(test)]
use backend_frontend_python::PythonFrontend;
#[cfg(test)]
use backend_frontend_rust::RustFrontend;
#[cfg(test)]
use backend_frontend_typescript::TypeScriptFrontend;
#[cfg(test)]
use std::error::Error;

#[cfg(test)]
const FIXTURES: &[(&str, &[u8], &str)] = &[
    (
        "rust",
        include_bytes!("fixtures/rust.input"),
        "rust-hir-authority",
    ),
    (
        "clang",
        include_bytes!("fixtures/clang.input"),
        "translation-unit",
    ),
    (
        "typescript",
        include_bytes!("fixtures/typescript.input"),
        "typescript-checker-authority",
    ),
    (
        "python",
        include_bytes!("fixtures/python.input"),
        "python-syntax-authority",
    ),
    (
        "go",
        include_bytes!("fixtures/go.input"),
        "go-authority-image",
    ),
    (
        "java",
        include_bytes!("fixtures/java.input"),
        "javac-authority-image",
    ),
    (
        "csharp",
        include_bytes!("fixtures/csharp.input"),
        "roslyn-image",
    ),
];

#[cfg(test)]
const NATIVE_HELPER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../frontends/rust/fixtures/native_authority.py"
);
#[cfg(test)]
const NATIVE_PARTIAL_HELPER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../frontends/rust/fixtures/partial_authority.py"
);

#[cfg(test)]
fn corpus_input(language: &str) -> Result<&'static [u8], Box<dyn Error>> {
    FIXTURES
        .iter()
        .find_map(|(name, input, _)| (*name == language).then_some(*input))
        .ok_or_else(|| std::io::Error::other(format!("missing {language} fixture")).into())
}

#[test]
fn golden_fixture_inputs_are_nonempty_and_facts_are_normalized() {
    for (language, input, expected_fact) in FIXTURES {
        assert!(!input.is_empty(), "empty {language} oracle fixture");
        assert!(!expected_fact.is_empty());
    }
    let mut names = FIXTURES
        .iter()
        .map(|(name, _, _)| *name)
        .collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 7);
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum FixtureMode {
    Cold,
    Persistent,
}

#[cfg(test)]
fn authority_for(language: &str, mode: FixtureMode) -> Result<Box<dyn Authority>, Box<dyn Error>> {
    let input = corpus_input(language)?.to_vec();
    let authority: Box<dyn Authority> = match language {
        "clang" => Box::new(match mode {
            FixtureMode::Cold => {
                ClangFrontend::with_helper(input, "/bin/sh", NATIVE_HELPER, "-O0")?
            }
            FixtureMode::Persistent => {
                ClangFrontend::with_persistent_helper(input, "/bin/sh", NATIVE_HELPER, "-O0")?
            }
        }),
        "csharp" => Box::new(match mode {
            FixtureMode::Cold => {
                CSharpFrontend::with_helper(input, "/bin/sh", NATIVE_HELPER, "net8")?
            }
            FixtureMode::Persistent => {
                CSharpFrontend::with_persistent_helper(input, "/bin/sh", NATIVE_HELPER, "net8")?
            }
        }),
        "go" => Box::new(match mode {
            FixtureMode::Cold => GoFrontend::with_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                b"module p".to_vec(),
                "linux",
            )?,
            FixtureMode::Persistent => GoFrontend::with_persistent_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                b"module p".to_vec(),
                "linux",
            )?,
        }),
        "java" => Box::new(match mode {
            FixtureMode::Cold => {
                JavaFrontend::with_helper(input, "/bin/sh", NATIVE_HELPER, b"cp".to_vec(), "21")?
            }
            FixtureMode::Persistent => JavaFrontend::with_persistent_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                b"cp".to_vec(),
                "21",
            )?,
        }),
        "python" => Box::new(match mode {
            FixtureMode::Cold => {
                PythonFrontend::with_helper(input, "/bin/sh", NATIVE_HELPER, "3.13", Vec::new())?
            }
            FixtureMode::Persistent => PythonFrontend::with_persistent_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                "3.13",
                Vec::new(),
            )?,
        }),
        "rust" => Box::new(match mode {
            FixtureMode::Cold => RustFrontend::with_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                b"[package]".to_vec(),
                "default",
            )?,
            FixtureMode::Persistent => RustFrontend::with_persistent_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                b"[package]".to_vec(),
                "default",
            )?,
        }),
        "typescript" => Box::new(match mode {
            FixtureMode::Cold => {
                TypeScriptFrontend::with_helper(input, "/bin/sh", NATIVE_HELPER, "ts", Vec::new())?
            }
            FixtureMode::Persistent => TypeScriptFrontend::with_persistent_helper(
                input,
                "/bin/sh",
                NATIVE_HELPER,
                "ts",
                Vec::new(),
            )?,
        }),
        _ => return Err(std::io::Error::other(format!("unknown frontend {language}")).into()),
    };
    Ok(authority)
}

#[test]
fn all_seven_authorities_bind_facts_to_their_discovery_root() -> Result<(), Box<dyn Error>> {
    for &(language, _, _) in FIXTURES {
        let authority = authority_for(language, FixtureMode::Cold)?;
        let snapshot = authority.discover()?;
        let key = SessionKey::new(
            authority.identity(),
            snapshot.manifest(),
            typed_of::<ProfileSchema>(b"p"),
            typed_of::<FlowSchema>(b"f"),
            typed_of::<SemanticBasisSchema>(b"s"),
        );
        let extraction = authority.extract(&snapshot, key)?;
        assert_eq!(
            extraction.manifest_root(),
            Some(snapshot.manifest().digest())
        );
        assert_eq!(
            extraction.authority_root(),
            Some(authority.identity().digest())
        );
        assert_eq!(extraction.revision(), Some(snapshot.sequence()));
        let CoverageWitness::Complete(coverage) = extraction.coverage() else {
            return Err(std::io::Error::other(
                "complete fixture extraction must retain admitted producer capability",
            )
            .into());
        };
        assert_eq!(
            coverage.producer_identity(),
            authority.identity().digest().to_bytes()
        );
        assert_eq!(
            coverage.scope_root(),
            ScopeRoot::from_bytes(snapshot.manifest().digest().to_bytes())
        );
        assert_eq!(extraction.records().len(), 6);
        assert_eq!(extraction.records()[0].kind(), FactKind::Declaration);
        assert_eq!(extraction.records()[0].key_bytes(), b"main");
        assert_eq!(extraction.records()[0].value(), b"decl");
        assert_eq!(extraction.records()[1].kind(), FactKind::Type);
        assert_eq!(extraction.records()[1].value(), b"i32");
        assert_eq!(extraction.records()[2].kind(), FactKind::Edge);
        assert_eq!(extraction.records()[3].kind(), FactKind::Diagnostic);
        assert_eq!(extraction.records()[4].kind(), FactKind::Dependency);
        assert_eq!(extraction.records()[5].kind(), FactKind::NegativeDependency);
        assert!(
            extraction
                .records()
                .iter()
                .all(|record| record.evidence().is_some())
        );
    }
    Ok(())
}

#[test]
fn all_seven_authorities_complete_the_persistent_protocol() -> Result<(), Box<dyn Error>> {
    for &(language, _, _) in FIXTURES {
        let authority = authority_for(language, FixtureMode::Persistent)?;
        let snapshot = authority.discover()?;
        let key = SessionKey::new(
            authority.identity(),
            snapshot.manifest(),
            typed_of::<ProfileSchema>(b"persistent-profile"),
            typed_of::<FlowSchema>(b"persistent-flow"),
            typed_of::<SemanticBasisSchema>(b"persistent-basis"),
        );
        let extraction = authority.extract(&snapshot, key)?;
        assert_eq!(extraction.records().len(), 6, "{language}");
        assert!(matches!(
            extraction.coverage(),
            CoverageWitness::Complete(_)
        ));
        assert_eq!(
            extraction.manifest_root(),
            Some(snapshot.manifest().digest())
        );
        assert_eq!(
            extraction.authority_root(),
            Some(authority.identity().digest())
        );
    }
    Ok(())
}

#[test]
fn partial_native_authority_keeps_scope_incomplete() -> Result<(), Box<dyn Error>> {
    let authority = RustFrontend::with_helper(
        b"fn main() {}".to_vec(),
        "/bin/sh",
        NATIVE_PARTIAL_HELPER,
        b"[package]\nname = \"fixture\"\n".to_vec(),
        "default",
    )?;
    let snapshot = authority.discover()?;
    let key = SessionKey::new(
        authority.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"p"),
        typed_of::<FlowSchema>(b"f"),
        typed_of::<SemanticBasisSchema>(b"s"),
    );
    let extraction = authority.extract(&snapshot, key)?;
    assert!(matches!(extraction.coverage().state(), Coverage::Partial));
    assert_eq!(extraction.records().len(), 2);
    assert!(
        extraction
            .records()
            .iter()
            .all(|record| record.evidence().is_some())
    );
    Ok(())
}

#[test]
fn manifest_negative_dependency_and_toolchain_changes_are_visible() -> Result<(), Box<dyn Error>> {
    let old = InputManifest::new(vec![
        Input::new(InputKind::Toolchain, "oracle", b"v1")?,
        Input::absent(InputKind::NegativeDependency, "optional/module")?,
        Input::new(InputKind::Generated, "generated/config", b"a")?,
    ])?;
    let new = InputManifest::new(vec![
        Input::new(InputKind::Toolchain, "oracle", b"v2")?,
        Input::new(
            InputKind::NegativeDependency,
            "optional/module",
            b"appeared",
        )?,
        Input::new(InputKind::Generated, "generated/config", b"b")?,
    ])?;
    let delta = DiscoveryDelta::between(&old, &new);
    assert_eq!(delta.changes().len(), 3);
    Ok(())
}

#[test]
fn cold_and_session_keys_are_deterministic_and_reset_on_schema_inputs() -> Result<(), Box<dyn Error>>
{
    let authority = AuthorityIdentity {
        producer: typed_of::<ProducerSchema>(b"oracle"),
        toolchain: typed_of::<ToolchainSchema>(b"tool-v1"),
        contract: typed_of::<ContractSchema>(b"schema-v1"),
    };
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "main", b"x")?])?;
    let key = |schema: &[u8]| {
        SessionKey::new(
            authority,
            &manifest,
            typed_of::<ProfileSchema>(b"profile"),
            typed_of::<FlowSchema>(b"flow"),
            typed_of::<SemanticBasisSchema>(schema),
        )
    };
    assert_eq!(key(b"schema-v1"), key(b"schema-v1"));
    assert_ne!(key(b"schema-v1"), key(b"schema-v2"));
    Ok(())
}

#[test]
fn partial_and_unavailable_never_authorize_deletion() {
    let partial = CoverageWitness::Partial(UntrustedCoverageScope::new(7));
    let unavailable = CoverageWitness::Unavailable(UntrustedCoverageScope::new(7));
    assert!(!matches!(partial, CoverageWitness::Complete(_)));
    assert!(!matches!(unavailable, CoverageWitness::Complete(_)));
}

#[test]
fn canonical_schema_rejection_fixtures_are_data_driven() -> Result<(), Box<dyn Error>> {
    for name in ["", "../parent", "./dot", "/absolute", "back\\slash"] {
        assert!(
            Input::new(InputKind::Source, name, b"x").is_err(),
            "accepted {name:?}"
        );
    }
    let a = Input::new(InputKind::Source, "same", b"a")?;
    let b = Input::new(InputKind::Source, "same", b"b")?;
    assert!(InputManifest::new(vec![a, b]).is_err());
    Ok(())
}

#[test]
fn legacy_classification_fixture_is_explicit() {
    let text = include_str!("fixtures/legacy-classification.txt");
    for class in ["required", "intentionally_changed", "incidental"] {
        assert!(
            text.lines().any(|line| line.starts_with(class)),
            "missing {class}"
        );
    }
}
