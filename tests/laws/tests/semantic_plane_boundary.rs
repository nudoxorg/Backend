//! Static boundary guards for the semantic-plane cutover.
//!
//! These checks deliberately inspect only shipping adapter source.  The
//! structural frontends and `compiler-compile` are allowed to use
//! Tree-sitter for fallback discovery; product publication, worker transport,
//! and query adapters must consume the canonical semantic image instead of
//! reconstructing a generic source-row projection.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/laws has a workspace root")
        .to_owned()
}

fn source(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read shipping adapter {relative}: {error}"))
}

fn forbidden_hits(relative: &str, forbidden: &[&str]) -> Option<String> {
    let text = source(relative);
    let hits = forbidden
        .iter()
        .filter(|needle| text.contains(**needle))
        .copied()
        .collect::<Vec<_>>();
    (!hits.is_empty()).then(|| format!("{relative}: {hits:?}"))
}

#[test]
fn semantic_adapters_do_not_rebuild_generic_product_rows() {
    let failures = [
        forbidden_hits(
            "crates/local-service/src/builtin/replication.rs",
            &[
                "ProductProjection",
                "ProductProjectionBuilder",
                "product_output_bytes",
            ],
        ),
        forbidden_hits(
            "apps/worker/src/builtin/profile.rs",
            &[
                "ProductProjection",
                "ProductProjectionBuilder",
                "product_output_bytes",
            ],
        ),
        forbidden_hits(
            "extensions/trustfall/src/query.rs",
            &["ViewRoot", "ViewRootDescriptor", "view.rows()"],
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "shipping adapters bypass the canonical semantic plane:\n{}",
        failures.join("\n")
    );
}

#[test]
fn go_authority_tests_do_not_probe_an_ambient_compiler_unbounded() {
    let text = source("frontends/go/tests/protocol.rs");
    let compact = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(
        !compact.contains("Command::new(&compiler).arg(\"version\").output()"),
        "Go authority tests must use a bounded probe or an explicit fixture"
    );
}

#[test]
fn tree_sitter_is_confined_to_structural_frontends_and_syntax_compile() {
    let root = workspace_root();
    let mut checked = 0_usize;
    for directory in ["crates", "apps", "extensions", "frontends"] {
        let base = root.join(directory);
        let mut pending = vec![base];
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).expect("inspect source tree");
            if metadata.is_dir() {
                for entry in fs::read_dir(path).expect("read source directory") {
                    pending.push(entry.expect("read source entry").path());
                }
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("source path is within workspace")
                .to_string_lossy();
            if relative.starts_with("crates/compile/") || relative.starts_with("frontends/") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read Rust source");
            assert!(
                !text.contains("tree_sitter") && !text.contains("tree-sitter"),
                "{relative} imports Tree-sitter outside the structural fallback boundary"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 100,
        "boundary scan covered too few shipping files"
    );
}

/// Every frontend exposes exactly one canonical `Authority` adapter and
/// documents its retained `legacy` module as subordinate, so no frontend can
/// present a second semantic plane.
#[test]
fn frontends_declare_one_canonical_authority_path() {
    let mut failures = Vec::new();
    for frontend in [
        "clang",
        "csharp",
        "go",
        "java",
        "python",
        "rust",
        "typescript",
    ] {
        let lib = source(&format!("frontends/{frontend}/lib.rs"));
        if lib.matches("impl Authority for").count() != 1 {
            failures.push(format!(
                "frontends/{frontend}/lib.rs: expected exactly one canonical Authority impl"
            ));
        }
        let legacy = source(&format!("frontends/{frontend}/src/legacy/mod.rs"));
        if !legacy.contains("CANONICAL AUTHORITY PATH") {
            failures.push(format!(
                "frontends/{frontend}/src/legacy/mod.rs: missing canonical authority path marker"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Real-authority frontend tests may not silently pass when their toolchain is
/// absent. Each formerly self-skipping test must resolve its toolchain through
/// the typed probe/terminal path, and no silent-skip marker may remain.
#[test]
fn real_authority_tests_do_not_self_skip() {
    let mut failures = Vec::new();
    let offenders = [
        (
            "frontends/csharp/tests/producer_parity.rs",
            "probe_dotnet()",
            &["replaying committed fixtures only"][..],
        ),
        (
            "frontends/go/tests/protocol.rs",
            "explicit_go_toolchain()",
            &["Go authority test skipped"][..],
        ),
        (
            "frontends/go/tests/native_helper.rs",
            "COMPILER_GO_COMPILER",
            &["find_executable"][..],
        ),
        (
            "frontends/python/tests/native_helper.rs",
            "COMPILER_PYTHON_COMPILER",
            &["find_executable"][..],
        ),
        (
            "frontends/python/tests/pyrefly_package.rs",
            "PyreflyPackageError::Unavailable",
            &[][..],
        ),
        (
            "frontends/typescript/tests/checker_protocol.rs",
            "checker.run(TypeScriptSource",
            &["skipping TypeScript checker e2e"][..],
        ),
        (
            "frontends/typescript/tests/legacy_checker_protocol.rs",
            "checker.run(",
            &[
                "skipping TypeScript checker e2e",
                "Err(CheckerError::ModuleUnavailable { .. }) => return Ok(())",
            ][..],
        ),
    ];
    for (relative, required, forbidden) in offenders {
        let text = source(relative);
        if !text.contains(required) {
            failures.push(format!(
                "{relative}: missing typed authority terminal `{required}`"
            ));
        }
        for marker in forbidden {
            if text.contains(marker) {
                failures.push(format!(
                    "{relative}: retained silent-skip marker `{marker}`"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
