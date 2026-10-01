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

/// Blanks Rust comments and string/byte/raw-string/char literals while
/// preserving byte length, so a later scan matches code tokens only. A
/// coordinate such as `"cargo:tree-sitter-cpp@0.23.4"` is data, not a second
/// structural plane, and must not trip the boundary law; a real
/// `use tree_sitter::...` must.
fn code_only(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                out[index] = b' ';
                index += 1;
            }
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let mut depth = 1_u32;
            out[index] = b' ';
            out[index + 1] = b' ';
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth += 1;
                    out[index] = b' ';
                    out[index + 1] = b' ';
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    out[index] = b' ';
                    out[index + 1] = b' ';
                    index += 2;
                } else {
                    out[index] = b' ';
                    index += 1;
                }
            }
            continue;
        }
        if let Some((hashes, quote)) = raw_string_quote(bytes, index) {
            let mut cursor = quote + 1;
            loop {
                if cursor >= bytes.len() {
                    break;
                }
                if bytes[cursor] == b'"'
                    && cursor + 1 + hashes <= bytes.len()
                    && bytes[cursor + 1..cursor + 1 + hashes]
                        .iter()
                        .all(|byte| *byte == b'#')
                {
                    cursor += 1 + hashes;
                    break;
                }
                cursor += 1;
            }
            for byte in &mut out[index..cursor] {
                *byte = b' ';
            }
            index = cursor;
            continue;
        }
        if bytes[index] == b'"' || (bytes[index] == b'b' && bytes.get(index + 1) == Some(&b'"')) {
            if bytes[index] == b'b' {
                out[index] = b' ';
            }
            let mut cursor = if bytes[index] == b'b' { index + 1 } else { index };
            out[cursor] = b' ';
            cursor += 1;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    out[cursor] = b' ';
                    cursor += 1;
                    if cursor < bytes.len() {
                        out[cursor] = b' ';
                        cursor += 1;
                    }
                    continue;
                }
                if bytes[cursor] == b'"' {
                    out[cursor] = b' ';
                    cursor += 1;
                    break;
                }
                out[cursor] = b' ';
                cursor += 1;
            }
            index = cursor;
            continue;
        }
        if bytes[index] == b'\'' {
            if bytes.get(index + 1) == Some(&b'\\') {
                let mut cursor = index + 2;
                while cursor < bytes.len() && bytes[cursor] != b'\'' {
                    cursor += 1;
                }
                if cursor < bytes.len() {
                    for byte in &mut out[index..=cursor] {
                        *byte = b' ';
                    }
                    index = cursor + 1;
                    continue;
                }
            } else if bytes.get(index + 2) == Some(&b'\'') {
                out[index] = b' ';
                out[index + 1] = b' ';
                out[index + 2] = b' ';
                index += 3;
                continue;
            }
        }
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Returns `(hash_count, quote_index)` when a raw string (`r"..."`,
/// `r#"..."#`, or the `br` byte form) begins at `index`.
fn raw_string_quote(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'"')).then_some((cursor - hashes_start, cursor))
}

/// True when `text` is a Cargo manifest declaring a `tree-sitter` dependency
/// (`tree-sitter = ...`, `tree-sitter.workspace = true`, or the
/// `[dependencies.tree-sitter]` table form).
fn declares_tree_sitter_dependency(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("tree-sitter") {
            let rest = rest.trim_start();
            return rest.starts_with('=') || rest.starts_with('.');
        }
        line.starts_with('[')
            && line
                .split(['.', ']'])
                .any(|segment| segment == "tree-sitter")
    })
}

/// True when shipping code references the `tree_sitter` crate or one of its
/// grammar crates (`tree_sitter_cpp`, ...) as an identifier/path. String data
/// such as a committed package coordinate is not a structural plane.
fn code_uses_tree_sitter(text: &str) -> bool {
    let code = code_only(text);
    code.as_bytes()
        .windows("tree_sitter".len())
        .any(|window| window == b"tree_sitter")
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Offsets of early unit-success returns (`return Ok(())`, including the
/// turbofish form) in `text`. Comments and string data are ignored, so the
/// check keys on the control-flow construct rather than on skip prose: a
/// reworded message that dodges a marker search still has to return the unit
/// success value to make an absent-toolchain test pass. Offsets index the
/// whitespace-stripped code and are reported only for diagnostics.
fn silent_success_returns(text: &str) -> Vec<usize> {
    let code: String = code_only(text)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut from = 0_usize;
    while let Some(relative) = code[from..].find("returnOk") {
        let at = from + relative;
        let left_ok = at == 0 || !is_identifier_byte(bytes[at - 1]);
        let mut cursor = at + "returnOk".len();
        if left_ok && bytes.get(cursor) == Some(&b':') && bytes.get(cursor + 1) == Some(&b':') {
            cursor += 2;
            if bytes.get(cursor) == Some(&b'<') {
                let mut depth = 1_u32;
                cursor += 1;
                while cursor < bytes.len() && depth > 0 {
                    match bytes[cursor] {
                        b'<' => depth += 1,
                        b'>' => depth -= 1,
                        _ => {}
                    }
                    cursor += 1;
                }
            }
        }
        if left_ok && bytes[cursor..].starts_with(b"(())") {
            found.push(at);
        }
        from = at + "returnOk".len();
    }
    found
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
                // Generated build trees (`.local/target`, stray `target/`)
                // hold copied manifests such as trybuild's, which are not
                // shipping source.
                let generated = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == ".local" || name == "target");
                if generated {
                    continue;
                }
                for entry in fs::read_dir(path).expect("read source directory") {
                    pending.push(entry.expect("read source entry").path());
                }
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .expect("source path is within workspace")
                .to_string_lossy()
                .into_owned();
            if relative.starts_with("crates/compile/") || relative.starts_with("frontends/") {
                continue;
            }
            let is_rust = path.extension().and_then(|extension| extension.to_str()) == Some("rs");
            let is_manifest =
                path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml");
            if !is_rust && !is_manifest {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read shipping source");
            if is_manifest {
                // A dependency declaration is the structural claim that this
                // crate participates in the Tree-sitter plane, whether or not
                // a `use` exists yet.
                assert!(
                    !declares_tree_sitter_dependency(&text),
                    "{relative} declares a Tree-sitter dependency outside the structural fallback boundary"
                );
            } else {
                // Only real code identifiers/paths count; a committed package
                // coordinate in test data is not a second structural plane.
                assert!(
                    !code_uses_tree_sitter(&text),
                    "{relative} imports Tree-sitter outside the structural fallback boundary"
                );
            }
            checked += 1;
        }
    }
    assert!(
        checked > 100,
        "boundary scan covered too few shipping files"
    );
}

/// Proves the boundary scanner is not vacuous: it ignores Tree-sitter text
/// inside string/comment data but catches an actual code reference or a
/// manifest dependency.
#[test]
fn tree_sitter_boundary_scanner_ignores_data_and_catches_code() {
    let coordinate = r#"PackageCoordinate::Purl("cargo:tree-sitter-cpp@0.23.4")"#;
    assert!(!code_uses_tree_sitter(coordinate));
    let string_data = r#"let label = "tree_sitter";"#;
    assert!(!code_uses_tree_sitter(string_data));
    let raw_string = "let query = r#\"tree_sitter::Parser\"#;";
    assert!(!code_uses_tree_sitter(raw_string));
    let comment = "// tree_sitter must stay in frontends/\n";
    assert!(!code_uses_tree_sitter(comment));

    assert!(code_uses_tree_sitter("use tree_sitter::{Parser, Query};"));
    assert!(code_uses_tree_sitter("let parser = tree_sitter::Parser::new();"));
    assert!(code_uses_tree_sitter("fn f() { tree_sitter_cpp::LANGUAGE; }"));

    assert!(declares_tree_sitter_dependency(
        "tree-sitter = \"=0.27.0\"\n"
    ));
    assert!(declares_tree_sitter_dependency(
        "tree-sitter.workspace = true\n"
    ));
    assert!(declares_tree_sitter_dependency("[dependencies.tree-sitter]\n"));
    assert!(!declares_tree_sitter_dependency(
        "[dependencies]\nserde = \"1\"\n"
    ));
}

/// The single-direction semantic authority contract.
///
/// Lane consolidation made each frontend's `src/legacy` module the one
/// production native-authority lane: the engine driver imports its symbols
/// directly from that module, with no intervening adapter. The crate-level
/// `syntax_frontend()` constructor is the separate, documented structural
/// baseline and never substitutes for that authority. The consolidation
/// deleted the top-level `Authority` adapters of the consolidated frontends,
/// so the semantic plane flows one way only — engine to `src/legacy` — and
/// no frontend can present a second semantic plane. The canonical-path
/// marker phrase stays mandatory: it is the documented statement of this
/// contract inside each lane.
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
        let legacy = source(&format!("frontends/{frontend}/src/legacy/mod.rs"));
        if !legacy.contains("CANONICAL AUTHORITY PATH") {
            failures.push(format!(
                "frontends/{frontend}/src/legacy/mod.rs: missing canonical authority path marker"
            ));
        }
        let lib = source(&format!("frontends/{frontend}/lib.rs"));
        if !lib.contains("pub fn syntax_frontend()") {
            failures.push(format!(
                "frontends/{frontend}/lib.rs: missing documented `syntax_frontend` structural baseline"
            ));
        }
        let lower = source(&format!("crates/engine/src/driver/lower/{frontend}.rs"));
        if !lower.contains(&format!("backend_frontend_{frontend}::legacy")) {
            failures.push(format!(
                "crates/engine/src/driver/lower/{frontend}.rs: engine must import the frontends/{frontend} production legacy lane directly"
            ));
        }
    }
    for frontend in ["csharp", "go", "java", "python", "rust"] {
        let lib = source(&format!("frontends/{frontend}/lib.rs"));
        if lib.contains("impl Authority for") {
            failures.push(format!(
                "frontends/{frontend}/lib.rs: top-level Authority adapter re-added; the production lane is src/legacy alone"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Real-authority frontend tests may not silently pass when their toolchain is
/// absent. Each formerly self-skipping test must resolve its toolchain through
/// the typed probe/terminal path, and the source may not contain an early
/// unit-success return. The check keys on the `return Ok(())` control-flow
/// construct rather than on skip prose, so rewording a message cannot evade
/// it.
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
            "frontends/typescript/tests/checker_protocol.rs",
            "checker.run(TypeScriptSource",
            &["skipping TypeScript checker e2e"][..],
        ),
        (
            "frontends/typescript/tests/legacy_checker_protocol.rs",
            "checker.run(",
            &["skipping TypeScript checker e2e"][..],
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
        for offset in silent_success_returns(&text) {
            failures.push(format!(
                "{relative}: early `return Ok(())` self-skip at byte {offset}; the absent-toolchain path must resolve a typed terminal instead"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Proves the self-skip guard is structural: a reworded absence skip is caught
/// by its `return Ok(())`, while the typed-terminal shape and skip prose in
/// comments are not.
#[test]
fn silent_skip_scanner_catches_reworded_absence_returns() {
    let reworded = r#"
        #[test]
        fn end_to_end() -> Result<(), AuthorityError> {
            let Ok(checker) = probe() else {
                eprintln!("checker is not provisioned on this host");
                return Ok(());
            };
            checker.run(source)?;
            Ok(())
        }
    "#;
    assert_eq!(
        silent_success_returns(reworded).len(),
        1,
        "a reworded absence skip must still be caught by its early success return"
    );

    let turbofish = r#"
        fn end_to_end() -> Result<(), AuthorityError> {
            if probe().is_err() {
                return Ok::<(), AuthorityError>(());
            }
            Ok(())
        }
    "#;
    assert_eq!(silent_success_returns(turbofish).len(), 1);

    let typed = r#"
        #[test]
        fn end_to_end() -> Result<(), AuthorityError> {
            let checker = probe()?;
            checker.run(source)?;
            Ok(())
        }
    "#;
    assert!(
        silent_success_returns(typed).is_empty(),
        "a typed-terminal test must not be flagged"
    );

    let prose_only = "// the checker is not provisioned here\nlet checker = probe()?;\nOk(())";
    assert!(
        silent_success_returns(prose_only).is_empty(),
        "skip prose without an early success return is not the guarded construct"
    );
}
