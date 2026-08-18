//! Real Go test-package coverage through the production oracle.
//!
//! `packages.Load(Tests: true)` produces three closely related values for one
//! directory: the ordinary package, an augmented package that includes
//! internal `_test.go` files, and a generated `main` test binary.  It also
//! produces an external `package foo_test` as a distinct package.  This test
//! makes all four cases observable against real Go source, so a future change
//! cannot quietly revert to `Tests: false`, duplicate production declarations,
//! or index the synthetic test-main package.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    kind::KindDiscriminant,
};
use nudox_languages::go::GoProducer;
use nudox_languages::{PackageSource, produce};

const MODULE: &str = "example.test/nudox-test-packages";

const PRODUCTION_GO: &str = r#"package probe

// ProductionMarker belongs to normal package source.
type ProductionMarker struct { Value int }

func ProductionOnly() ProductionMarker { return ProductionMarker{} }
"#;

const INTERNAL_TEST_GO: &str = r#"package probe

import "testing"

// InternalTestMarker belongs to the augmented ordinary package.
type InternalTestMarker struct { Value int }

func TestInternalMarker(t *testing.T) { _ = InternalTestMarker{} }
"#;

const EXTERNAL_TEST_GO: &str = r#"package probe_test

import "testing"

// ExternalTestMarker belongs to a distinct external test package.
type ExternalTestMarker struct { Value int }

func TestExternalMarker(t *testing.T) { _ = ExternalTestMarker{} }
"#;

const WINDOWS_ONLY_GO: &str = r#"//go:build windows

package probe

// WindowsOnly is intentionally unavailable on the current platform.
type WindowsOnly struct{}
"#;

fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-test-packages");
        let status = Command::new("go")
            .current_dir(&oracle_dir)
            .args(["build", "-o"])
            .arg(&out)
            .arg("./...")
            .status()
            .expect("`go` must be available to build the committed Go oracle");
        assert!(status.success(), "go build of the committed oracle failed");
        out
    })
}

fn fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("create temporary Go module");
    std::fs::write(
        fixture.path().join("go.mod"),
        format!("module {MODULE}\n\ngo 1.22\n"),
    )
    .expect("write go.mod");
    std::fs::write(fixture.path().join("probe.go"), PRODUCTION_GO)
        .expect("write production Go source");
    std::fs::write(fixture.path().join("probe_test.go"), INTERNAL_TEST_GO)
        .expect("write internal Go test source");
    std::fs::write(fixture.path().join("external_test.go"), EXTERNAL_TEST_GO)
        .expect("write external Go test source");
    std::fs::write(fixture.path().join("windows_only.go"), WINDOWS_ONLY_GO)
        .expect("write build-tagged Go source");
    fixture
}

fn build_tag_fixture() -> tempfile::TempDir {
    let fixture = tempfile::tempdir().expect("create temporary Go module");
    std::fs::write(
        fixture.path().join("go.mod"),
        format!("module {MODULE}\n\ngo 1.22\n"),
    )
    .expect("write go.mod");
    std::fs::write(
        fixture.path().join("probe.go"),
        "package probe\n\ntype Available struct{}\n",
    )
    .expect("write active Go source");
    std::fs::write(fixture.path().join("windows_only.go"), WINDOWS_ONLY_GO)
        .expect("write build-tagged Go source");
    fixture
}

fn entries_named<'a>(
    table: &'a nudox_ir::apply::PristineIntroTable,
    name: &str,
    kind: KindDiscriminant,
) -> Vec<(nudox_ir::change::IntroId, &'a nudox_ir::entry::Entry)> {
    table
        .iter()
        .filter(|(_, entry)| entry.sym().name == name && entry.kind().discriminant() == Some(kind))
        .collect()
}

#[test]
fn go_oracle_indexes_internal_and_external_test_packages_once_without_testmain() {
    let fixture = fixture();
    // SAFETY: this integration-test process owns the oracle invocation and
    // writes one immutable binary path before the producer starts.
    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let source = PackageSource::new(fixture.path(), MODULE, "0.1.0");
    let lineage = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MODULE));
    let (produced, _cost) =
        heart::cost::measured("producer/go-test-packages", fixture.path(), || {
            produce(&GoProducer, &source, &lineage, &nudox_ir::foreign::Unlinked)
        });
    let produced = produced.expect("the self-contained Go module must lower through the oracle");
    assert!(
        produced.source_issues.is_empty(),
        "real Go and _test.go spans must all materialize exactly: {:?}",
        produced.source_issues
    );

    // The production package appears both as its ordinary and augmented
    // variants in go/packages. Keeping both would create two definitions of
    // ProductionMarker; keeping only the ordinary one would lose the internal
    // marker. The selected rich variant must yield exactly one of each.
    for name in [
        "ProductionMarker",
        "InternalTestMarker",
        "ExternalTestMarker",
    ] {
        let matches = entries_named(&produced.table, name, KindDiscriminant::Record);
        assert_eq!(
            matches.len(),
            1,
            "{name} must be represented exactly once; got {} entries at {:?}",
            matches.len(),
            matches
                .iter()
                .map(|(intro, _)| intro.to_hex())
                .collect::<Vec<_>>()
        );
    }

    // The test main has no source-level declaration from this module. Its
    // generated `main` / testDeps symbols are the signature of accidentally
    // treating `example.test/nudox-test-packages/probe.test` as user code.
    for synthetic in ["testDeps", "matchString", "tests"] {
        assert!(
            entries_named(&produced.table, synthetic, KindDiscriminant::Function).is_empty(),
            "generated test-main symbol {synthetic:?} must not be indexed"
        );
    }

    // Exact excerpts must be read from the test source, not reconstructed from
    // Go types or restricted to non-test files. This also proves the selected
    // augmented package preserves a real source position for the declaration.
    for marker in ["InternalTestMarker", "ExternalTestMarker"] {
        let (intro, _) = entries_named(&produced.table, marker, KindDiscriminant::Record)
            .into_iter()
            .next()
            .expect("checked exact-one above");
        let excerpt = produced
            .source
            .iter()
            .find_map(|(source_intro, text)| (*source_intro == intro).then_some(text.as_str()))
            .unwrap_or_else(|| {
                panic!("{marker} must carry a source excerpt from its _test.go file")
            });
        assert!(
            excerpt.contains(&format!("type {marker} struct")),
            "{marker}'s exact source excerpt came from the wrong declaration: {excerpt:?}"
        );
    }
}

#[test]
fn go_oracle_records_excluded_build_tagged_exported_declarations() {
    let fixture = build_tag_fixture();
    // SAFETY: this integration-test process owns the oracle invocation and
    // writes one immutable binary path before the producer starts.
    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let source = PackageSource::new(fixture.path(), MODULE, "0.1.0");
    let oracle_output = Command::new(oracle_bin())
        .arg(source.root())
        .output()
        .expect("the self-contained Go module must invoke the oracle");
    assert!(
        oracle_output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&oracle_output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&oracle_output.stdout).expect("oracle output must be JSON");
    let package = json["packages"]
        .as_array()
        .and_then(|packages| {
            packages
                .iter()
                .find(|p| p["importPath"] == format!("{MODULE}/probe"))
        })
        .expect("probe package must be present");

    assert!(
        !package["decls"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|decl| decl["name"] == "WindowsOnly"),
        "WindowsOnly should be excluded from the active declaration set"
    );
    let skipped = package["buildConstraints"]
        .as_array()
        .expect("excluded Go files must produce typed buildConstraints records");
    let record = skipped
        .iter()
        .find(|record| {
            record["file"]
                .as_str()
                .is_some_and(|file| file.ends_with("windows_only.go"))
        })
        .expect("windows_only.go constraint record must be present");
    assert!(
        record["constraints"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|constraint| constraint == "windows"),
        "constraint record must preserve the windows build tag: {record}"
    );
    assert!(
        record["exportedDecls"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|decl| decl["name"] == "WindowsOnly"),
        "constraint record must preserve the skipped exported declaration: {record}"
    );
}
