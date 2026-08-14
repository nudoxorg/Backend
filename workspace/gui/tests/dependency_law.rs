//! docs/AGENTS-DOCTRINE.md §1 (the dependency law), enforced instead of described.
//!
//! # Why this file exists
//!
//! §1 used to be prose that only a reviewer could apply, and it was phrased in
//! a way that could not survive contact with the dependency graph: *"`lindsey`
//! may name `nudox-engine` and nothing else"*. `lindsey` has always had a
//! transitive edge to `nudox-ir` — it sits at depth 2 under `nudox-engine`,
//! unavoidably — so read literally the rule was violated on the day it was
//! written, and read charitably it needed a distinction the text did not make.
//!
//! The distinction is **depend** vs **import**. A transitive edge is harmless: a
//! view cannot couple to a type it has no path to name. A *direct* dependency is
//! what makes `use nudox_ir::…` compile, and coupling the view to the shape of
//! the IR instead of to the protocol is the actual harm §1 names. So the rule
//! this file enforces is:
//!
//! * `lindsey` may declare a dependency on `nudox-engine` and `heart`, and on no
//!   other backend crate — in any dependency section, including
//!   `[dev-dependencies]`, because a dev-dependency is enough to make an import
//!   compile in a test and tests are where shortcuts get taken;
//! * no file under `workspace/gui/src/` may import `nudox_ir`, or reach into
//!   `nudox_engine::store::` / `nudox_engine::graph::` — the two merged planes
//!   that hold the IR's shape. The engine's own public surface (`wire`,
//!   `EngineHandle`, `Purl`, …) and the two capability-port seams
//!   (`nudox_engine::mcp::`, `nudox_engine::embed::`) remain reachable, exactly
//!   as `nudox-mcp` and `nudox-embed` were when they were sibling crates.
//!
//! The second check is not redundant with the first. The manifest check is the
//! one that can actually be violated by a one-line edit, and it fails at the
//! point of the mistake; the source check is what would survive someone deciding
//! the manifest allow-list "needs" widening, and it names the file and line.
//!
//! # Why not `cargo metadata` / a TOML parser
//!
//! `workspace/gui` has no `toml` dependency and this test must not add one — a
//! test that enforces the dependency law by *adding a dependency* is its own
//! counterexample. Shelling out to `cargo metadata` would make an architectural
//! invariant depend on a subprocess and a network-capable cargo. Both checks are
//! therefore plain text scans over files this repository owns.

use std::path::{Path, PathBuf};

/// Backend crates `lindsey` is permitted to declare a direct dependency on.
///
/// `nudox-engine` is the protocol seam, and now *also* carries the store, graph,
/// MCP and embed planes as modules. `heart` sits beside the engine on a third
/// seam: it is the transport-free wire vocabulary shared by `index::server` and
/// `heart::client::http::NudoxClient` (behind `heart`'s own off-by-default
/// `client` feature). It powers the omni-search remote-results section
/// (`src/views/omni_search.rs`), additive to the local-first engine results.
///
/// NOTE: `heart` does not start with `nudox-`, so the manifest scan below
/// (which only flags `nudox-*` crate names) does not actually gate it today.
/// It is listed anyway so this allow-list stays the single source of truth
/// for "which backend crates may `lindsey` depend on", matching §1's prose.
///
/// Adding to this list is a doctrine change, not a build fix. Update
/// `docs/AGENTS-DOCTRINE.md` §1 in the same commit or the two disagree again.
const ALLOWED_BACKEND_DEPENDENCIES: &[&str] = &["nudox-engine", "heart"];

/// Paths whose types are the *shape of the IR* — or the merged planes that hold
/// it. A view that can name these can couple to them, which is the harm §1
/// exists to prevent. `nudox-ir` remains a crate; `store` and `graph` are now
/// modules of `nudox-engine`, so they are named as `nudox_engine::store` /
/// `nudox_engine::graph` rather than as crates.
const FORBIDDEN_CRATES: &[(&str, &str)] = &[
    ("nudox-ir", "nudox_ir"),
    ("nudox-engine::store", "nudox_engine::store"),
    ("nudox-engine::graph", "nudox_engine::graph"),
];

fn gui_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Strip `//` line comments and `/* … */` block comments.
///
/// The GUI legitimately *discusses* these crates in prose — `src/theme/kind.rs`
/// mirrors `nudox_ir::kind::KindDiscriminant`'s 13 frozen wire discriminants and
/// says so in its module doc, and `src/stores/symbol.rs` and
/// `src/ui/signature_line.rs` both reference `nudox_ir` paths in comments
/// explaining what they deliberately do *not* depend on. A naive `grep` reports
/// all three and this test would be deleted within a day for crying wolf.
///
/// String literals are not tracked. A `"…nudox_ir…"` string would be reported as
/// an import, which is a false *positive* — a loud, immediately-obvious failure
/// naming the exact line — rather than a false negative. That is the correct
/// direction for a check whose whole job is to not be silently satisfiable.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let mut block_depth = 0usize;

    while i < bytes.len() {
        if block_depth > 0 {
            if bytes[i..].starts_with(b"/*") {
                block_depth += 1;
                i += 2;
            } else if bytes[i..].starts_with(b"*/") {
                block_depth -= 1;
                i += 2;
            } else {
                // Keep newlines so reported line numbers stay accurate.
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            block_depth = 1;
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", current.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The dependency key a manifest line declares, if it declares one.
///
/// Handles both spellings cargo accepts: an inline `name = { … }` / `name = "…"`
/// entry, and a `[dependencies.name]` / `[target.'…'.dependencies.name]` table
/// header. Missing the table form would leave a way to add a forbidden crate
/// that this test cannot see, which is the one failure mode it must not have.
fn declared_dependency(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    if let Some(rest) = line.strip_prefix('[') {
        let header = rest.strip_suffix(']')?;
        let (section, name) = header.rsplit_once('.')?;
        return section.ends_with("dependencies").then_some(name);
    }
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty() && !key.contains(' ')).then_some(key)
}

/// `lindsey`'s manifest declares no backend dependency outside the allow-list.
#[test]
fn lindsey_declares_no_backend_dependency_beyond_engine_and_heart() {
    let manifest_path = gui_root().join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));

    let mut violations = Vec::new();
    for (number, line) in manifest.lines().enumerate() {
        let Some(name) = declared_dependency(line) else {
            continue;
        };
        // Only backend crates are governed; `gpui`, `serde`, … are not §1's
        // concern. `nudox-test-support` is deliberately NOT allow-listed: it is
        // a test helper, but it is also a backend crate, and §1 draws its line
        // at the crate graph rather than at intent.
        if name.starts_with("nudox-") && !ALLOWED_BACKEND_DEPENDENCIES.contains(&name) {
            violations.push(format!("  Cargo.toml:{}: {name}", number + 1));
        }
    }

    assert!(
        violations.is_empty(),
        "docs/AGENTS-DOCTRINE.md §1: `lindsey` may declare a direct dependency on {:?} \
         and no other backend crate, because a direct dependency is exactly what \
         makes `use <crate>::…` compile and lets a view couple to the shape of the \
         IR instead of to the protocol. Found:\n{}\n\nIf this is intentional, it is \
         a doctrine change: amend §1 and `ALLOWED_BACKEND_DEPENDENCIES` in the same \
         commit, and say which type crosses the seam and why it is not an IR type.",
        ALLOWED_BACKEND_DEPENDENCIES,
        violations.join("\n")
    );
}

/// No GUI source file imports an IR-shaped crate or reaches into an IR-holding plane.
#[test]
fn lindsey_source_imports_no_ir_shaped_crate() {
    let src = gui_root().join("src");
    let sources = rust_sources(&src);

    // A directory rename that silently scanned nothing must not read as a pass —
    // the same "green because it never ran" failure this suite exists to catch.
    assert!(
        sources.len() > 10,
        "expected the GUI to have many source files under {}, found {} — this \
         test scanned nothing and would pass vacuously",
        src.display(),
        sources.len()
    );

    let mut violations = Vec::new();
    for path in &sources {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let code = strip_comments(&source);

        for (number, line) in code.lines().enumerate() {
            for (crate_name, module) in FORBIDDEN_CRATES {
                let imports = line.contains(&format!("use {module}"))
                    || line.contains(&format!("{module}::"));
                if imports {
                    let display = path.strip_prefix(gui_root()).unwrap_or(path);
                    violations.push(format!(
                        "  {}:{}: names `{crate_name}` — {}",
                        display.display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "docs/AGENTS-DOCTRINE.md §1: no file under `workspace/gui/src` may import \
         `nudox-ir` or reach into `nudox_engine::store`/`nudox_engine::graph` — the \
         planes that hold the IR's shape. A view must depend on the protocol \
         (`nudox-engine`, `heart`), never on the shape of the IR. Found:\n{}\n\nThe \
         fix is to widen the engine's protocol so the GUI can ask for what it needs \
         in protocol terms — not to add the dependency. (Comments are stripped before \
         this scan, so these are real imports, not prose: `src/theme/kind.rs` discusses \
         `nudox_ir::kind` on purpose and is correctly not reported.)",
        violations.join("\n")
    );
}
