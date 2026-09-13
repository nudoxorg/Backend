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
    let text = source("crates/compiler-language-go/tests/protocol.rs");
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
