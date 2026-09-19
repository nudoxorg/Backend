//! Compatibility contract tests using deterministic native authority inputs.
//!
//! AUTHORITY-LANE CONSOLIDATION (cutover rationale, mirroring
//! `tests/journeys/tests/cutover_e2e.rs`): the manifest-only native stub
//! lanes for C#, Go, Java, and Python (`CSharpFrontend`, `GoFrontend`,
//! `JavaFrontend`, `PythonFrontend`) were deleted together with their
//! helper-protocol transports. Their production native-authority lanes
//! survive as the real toolchain transactions the engine drives (the Roslyn
//! `CSharpAuthorityProducer`, the vendored `GoOracle` image, the javac
//! harness behind `JavaAuthorityImage`, and the `Pyrefly` checker), but those
//! require a live dotnet/go/jdk/pyrefly toolchain, so they cannot ground a
//! deterministic helper-protocol contract here. The surviving
//! deterministic authority owners — `ClangFrontend` and the
//! `TypeScriptFrontend` (whose helper protocol is the same language-agnostic
//! native contract) — carry the discovery-root-binding, persistent-protocol,
//! and partial-coverage legs below. The retired legs per test:
//!
//! - `native_authorities_bind_facts_to_their_discovery_root`: the C#, Go,
//!   Java, and Python stub legs are retired (deleted stub lanes; the same
//!   discovery-root law is exercised by the two surviving deterministic
//!   lanes and by the engine suites driving the real lanes, e.g.
//!   `go_render`, `java_corpus`, `python_render`, and the Roslyn
//!   authority tests).
//! - `native_authorities_complete_the_persistent_protocol`: same four
//!   retired stub legs, same rationale; the persistent half-duplex protocol
//!   itself is language-agnostic and stays proven by the surviving lanes.
//! - `semantic_profiles_parse_before_environment_validation`: the Java and
//!   Python string-profile legs are re-pointed at the closed vocabulary
//!   where that parsing now lives (`backend_semantic::vocabulary`), keeping
//!   the open-ended-profile rejection intent.
//! - `semantic_profile_aliases_have_one_manifest_identity`: the Java alias
//!   leg is re-pointed at `JavaRelease` (which still admits both spellings);
//!   the Python alias leg is retired because the closed vocabulary admits
//!   exactly one canonical spelling per profile, so the one-identity law
//!   holds by construction (documented below).
//!
//! Rust's stub authority was retired earlier for the same reason (the root
//! `RustFrontend` was deleted; the engine drives the legacy Cargo/rust-analyzer
//! adapter, and Rust's structural floor is proven by the `compiler_parity`
//! and `structural_containment` suites through
//! `backend_frontend_rust::syntax_frontend`).
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
use backend_frontend_typescript::TypeScriptFrontend;
#[cfg(test)]
use backend_semantic::vocabulary::{JavaRelease, LanguageProfile};
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

#[test]
fn semantic_profiles_parse_before_environment_validation() -> Result<(), &'static str> {
    // The Java and Python string-profile grammars moved into the closed
    // vocabulary the surviving lanes share (`backend_semantic::vocabulary`),
    // so the parse-before-environment law is asserted exactly where the
    // rejection now happens: an open-ended profile never parses, and
    // parsing touches no toolchain, executable, or filesystem state.
    assert!(matches!(JavaRelease::try_from("latest"), Err("latest")));
    assert!(matches!(
        LanguageProfile::try_from("python-next"),
        Err("python-next")
    ));

    let Err(typescript) = TypeScriptFrontend::new(
        Vec::new(),
        "relative-node",
        "relative-tsc",
        "javascript",
        Vec::new(),
    ) else {
        return Err("ambiguous TypeScript grammar was accepted");
    };
    assert!(
        typescript
            .to_string()
            .contains("unsupported typescript profile \"javascript\"")
    );
    Ok(())
}

#[test]
fn semantic_profile_aliases_have_one_manifest_identity() -> Result<(), Box<dyn Error>> {
    // Java still admits the canonical javac release spelling and its profile
    // alias; both must collapse onto the one closed variant.
    assert_eq!(
        JavaRelease::try_from("21"),
        JavaRelease::try_from("java-21")
    );

    // The Python alias leg is retired: the closed `LanguageProfile`
    // vocabulary admits exactly one canonical spelling per profile
    // (`python-3.13`), so the alias-collapse law the stub lane used to
    // enforce holds by construction and no second spelling remains to join
    // against. The TypeScript lane keeps a live alias surface and still
    // binds both spellings to one manifest identity.
    let canonical =
        TypeScriptFrontend::new(Vec::new(), "/bin/sh", "/bin/sh", "ts", Vec::new())?.discover()?;
    let alias =
        TypeScriptFrontend::new(Vec::new(), "/bin/sh", "/bin/sh", "typescript", Vec::new())?
            .discover()?;
    assert_eq!(canonical.manifest().digest(), alias.manifest().digest());
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum FixtureMode {
    Cold,
    Persistent,
}

#[cfg(test)]
/// The deterministic authority legs that survive the stub-lane deletion.
///
/// Deliberately two lanes: the C#, Go, Java, and Python native stub
/// authorities were deleted together with their manifest-only frontends
/// (`CSharpFrontend`, `GoFrontend`, `JavaFrontend`, `PythonFrontend`), and
/// their production lanes (Roslyn `CSharpAuthorityProducer`, vendored
/// `GoOracle`, javac harness + `JavaAuthorityImage`, `Pyrefly`) are real
/// toolchain transactions that cannot ground a deterministic helper-protocol
/// contract here. Those lanes' semantics are exercised by the engine suites
/// that drive them (`go_render`, `java_corpus`, `python_render`, and the
/// Roslyn authority tests). Clang and TypeScript keep live helper lanes with
/// the same language-agnostic native protocol and carry the contract below.
const AUTHORITY_LEGS: &[&str] = &["clang", "typescript"];

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
fn native_authorities_bind_facts_to_their_discovery_root() -> Result<(), Box<dyn Error>> {
    // Deliberately two lanes: the C#, Go, Java, Python, and Rust native stub
    // authorities were deleted with their manifest-only frontends (see the
    // module-level cutover rationale and [`AUTHORITY_LEGS`]). The surviving
    // deterministic helper lanes carry the discovery-root law; the real
    // production lanes for the retired languages prove it in the engine
    // suites that drive them.
    for &language in AUTHORITY_LEGS {
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
        assert!(!extraction.records()[0].value().is_empty());
        assert_eq!(extraction.records()[1].kind(), FactKind::Type);
        assert!(!extraction.records()[1].value().is_empty());
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
fn native_authorities_complete_the_persistent_protocol() -> Result<(), Box<dyn Error>> {
    // Deliberately two lanes: the C#, Go, Java, Python, and Rust native stub
    // authorities were deleted with their manifest-only frontends (see the
    // module-level cutover rationale and [`AUTHORITY_LEGS`]).
    for &language in AUTHORITY_LEGS {
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
    // Re-based from `RustFrontend` onto the live TypeScript native lane. The
    // `partial_authority.py` fixture speaks the language-agnostic protocol,
    // echoes the requested language, and emits the same generic record shape
    // the TypeScript adapter already admits in `native_authority.py`'s
    // fallback, so the coverage-semantics claim (a partial native authority
    // never claims complete scope and every record still carries evidence) is
    // preserved unchanged.
    let authority = TypeScriptFrontend::with_helper(
        b"export function main(): number { return 0; }\n".to_vec(),
        "/bin/sh",
        NATIVE_PARTIAL_HELPER,
        "ts",
        Vec::new(),
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
fn a_selected_but_vanished_authority_is_unavailable_not_unsupported() -> Result<(), Box<dyn Error>>
{
    // Journey-leg restoration. `tests/journeys/tests/cutover_e2e.rs` used to
    // carry the only journey-level proof that a selected-but-vanished
    // authority yields the typed `Coverage::Unavailable` witness — distinct
    // from `Coverage::Unsupported` — and that witness was retired there with
    // its manifest-only constructors (Rust, Python, Go, Java, C#). The
    // distinction now lives in the shared native adapter
    // (`extract_native_with_adapter`): no configured helper is Unsupported,
    // while a helper that was configured but whose executable has vanished is
    // Unavailable. This leg re-proves both terminals through the surviving
    // TypeScript authority shape: same source, same profile, only the
    // helper's existence differs.
    let source = b"export function main(): number { return 0; }\n".to_vec();
    let node = "/bin/sh";

    // Manifest-only: no helper was ever selected, so the lane never claims
    // native semantic authority — the typed Unsupported witness.
    let manifest_only = TypeScriptFrontend::new(source.clone(), node, node, "ts", Vec::new())?;
    let snapshot = manifest_only.discover()?;
    let key = SessionKey::new(
        manifest_only.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"vanish-profile"),
        typed_of::<FlowSchema>(b"vanish-flow"),
        typed_of::<SemanticBasisSchema>(b"vanish-basis"),
    );
    let extraction = manifest_only.extract(&snapshot, key)?;
    assert_eq!(
        extraction.coverage().state(),
        Coverage::Unsupported,
        "a manifest-only authority must report Unsupported"
    );

    // Selected-but-vanished: the helper path was configured and its identity
    // is part of the manifest, but the executable is gone. The lane reports
    // the typed Unavailable witness — a counted terminal, never a silent
    // skip and never the Unsupported that would mean "never selected".
    let vanished = TypeScriptFrontend::with_helper(
        source,
        node,
        "/nonexistent/backend-vanished-checker.js",
        "ts",
        Vec::new(),
    )?;
    let snapshot = vanished.discover()?;
    let key = SessionKey::new(
        vanished.identity(),
        snapshot.manifest(),
        typed_of::<ProfileSchema>(b"vanish-profile"),
        typed_of::<FlowSchema>(b"vanish-flow"),
        typed_of::<SemanticBasisSchema>(b"vanish-basis"),
    );
    let extraction = vanished.extract(&snapshot, key)?;
    assert_eq!(
        extraction.coverage().state(),
        Coverage::Unavailable,
        "a selected-but-vanished authority must report Unavailable"
    );
    assert!(
        extraction.records().is_empty(),
        "an unavailable authority manufactures no facts"
    );
    Ok(())
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
