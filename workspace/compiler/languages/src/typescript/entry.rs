//! Entry-point and declaration-root discovery.
//!
//! Ported nearly verbatim from `workspace/compiler/compile/typescript/oxc/entry.rs`
//! with only the import paths changed. No IR API calls; this is pure file-system
//! and resolver logic.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use oxc_resolver::{ResolveError, ResolveOptions, Resolver};

use crate::typescript::extract::PackageError;

// ── Public API ────────────────────────────────────────────────────────────────

/// Build the resolver used for entry discovery and graph edges, configured for
/// TypeScript `.d.ts`-first resolution.
pub(crate) fn make_resolver() -> Resolver {
    Resolver::new(ResolveOptions {
        condition_names: vec![
            "types".to_string(),
            "import".to_string(),
            "node".to_string(),
        ],
        main_fields: vec![
            "types".to_string(),
            "typings".to_string(),
            "module".to_string(),
            "main".to_string(),
        ],
        extensions: vec![
            ".d.ts".to_string(),
            ".ts".to_string(),
            ".tsx".to_string(),
            ".js".to_string(),
            ".json".to_string(),
        ],
        extension_alias: vec![
            (
                ".js".to_string(),
                vec![".d.ts".to_string(), ".ts".to_string(), ".js".to_string()],
            ),
            (
                ".mjs".to_string(),
                vec![".d.mts".to_string(), ".mts".to_string(), ".mjs".to_string()],
            ),
            (
                ".cjs".to_string(),
                vec![".d.cts".to_string(), ".cts".to_string(), ".cjs".to_string()],
            ),
        ],
        builtin_modules: true,
        node_path: false,
        ..ResolveOptions::default()
    })
}

/// Discover declaration roots for the package at `root`.
pub(crate) fn discover_entry_points(root: &Path) -> Result<Vec<PathBuf>, PackageError> {
    let resolver = make_resolver();
    discover_entry_points_with(&resolver, root, None)
}

/// Discover entry points given a pre-built resolver and an optional repo hint.
pub(crate) fn discover_entry_points_with(
    resolver: &Resolver,
    root: &Path,
    repo_hint: Option<&str>,
) -> Result<Vec<PathBuf>, PackageError> {
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();

    if let Some(hint) = repo_hint {
        let candidate = root.join(hint);
        if candidate.is_file() && is_ts_module_path(&candidate) {
            found.insert(candidate);
        }
    }

    match resolver.resolve(root, ".") {
        Ok(resolution) => {
            let path = resolution.into_path_buf();
            if path.is_file() && is_ts_module_path(&path) {
                found.insert(path);
            }
        }
        Err(ResolveError::Builtin { .. }
            | ResolveError::NotFound(_)
            | ResolveError::PackagePathNotExported { .. }) => {}
        Err(other) => {
            tracing::debug!(
                root = %root.display(),
                error = %other,
                "resolver.resolve(root, \".\") failed; trying fallbacks"
            );
        }
    }

    let mut has_exports_field = false;
    if let Ok((had_exports, roots)) = exports_entry_points(resolver, root) {
        has_exports_field = had_exports;
        found.extend(roots);
    }

    // Node's classic (pre-`exports`) resolution rule: when `package.json` has
    // no `exports` map, every file in the package is real, importable API —
    // `require('lodash/chunk')` is not a hack, it is how `lodash` ships ~300
    // of its ~300 functions, because `main` (`lodash.js`) is a single 17k-line
    // UMD `ExpressionStatement` with no declaration this extractor can read.
    // Recovers real API from `lodash` and `debug`; must NOT fire for `ws`,
    // which declares an `exports` map (see `exports_entry_points`'s `bool`).
    if !has_exports_field {
        found.extend(deep_import_roots(root));
    }

    if found.is_empty() {
        for rel in &[
            "mod.ts",
            "index.ts",
            "src/mod.ts",
            "src/index.ts",
            "index.d.ts",
        ] {
            let candidate = root.join(rel);
            if candidate.is_file() {
                found.insert(candidate);
                break;
            }
        }
    }

    if found.is_empty() {
        return Err(PackageError::EntryPointDiscoveryFailed {
            path: root.to_path_buf(),
        });
    }

    Ok(found.into_iter().collect())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Returns `(had_exports_field, roots)`: `had_exports_field` is `true` iff
/// `package.json` declares an `exports` map at all (regardless of whether any
/// of its entries actually resolved to a file) — callers use this, not
/// `roots.is_empty()`, to decide whether Node's classic no-`exports`
/// resolution fallback applies (`deep_import_roots`), because an `exports`
/// map that failed to resolve anything is still an `exports` map: it does not
/// hand back "any file in the package", the fallback only Node's *absence* of
/// one triggers.
fn exports_entry_points(
    resolver: &Resolver,
    package_root: &Path,
) -> Result<(bool, Vec<PathBuf>), PackageError> {
    let manifest_path = package_root.join("package.json");
    if !manifest_path.is_file() {
        return Ok((false, vec![]));
    }

    let content = fs::read_to_string(&manifest_path).map_err(PackageError::Io)?;
    let manifest: serde_json::Value =
        serde_json::from_str(&content).map_err(PackageError::Serialization)?;

    let Some(exports) = manifest.get("exports") else {
        return Ok((false, vec![]));
    };

    let mut candidates: Vec<String> = Vec::new();
    collect_export_specifiers(exports, &mut candidates);

    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for spec in candidates {
        match resolver.resolve(package_root, &spec) {
            Ok(resolution) => {
                let path = resolution.into_path_buf();
                if path.is_file() && is_ts_module_path(&path) {
                    out.insert(path);
                }
            }
            Err(ResolveError::Builtin { .. }
                | ResolveError::NotFound(_)
                | ResolveError::PackagePathNotExported { .. }
                | ResolveError::Ignored(_)) => {}
            Err(err) => {
                tracing::debug!(
                    package_root = %package_root.display(),
                    specifier = %spec,
                    error = %err,
                    "exports fan-out: resolver error; skipping"
                );
            }
        }
    }

    Ok((true, out.into_iter().collect()))
}

/// Every `.js`/`.d.ts` file under `root`, for packages whose `package.json`
/// has no `exports` field — Node's classic resolution rule makes every one of
/// them real, importable API, not just the file(s) reachable from `main`
/// (`lodash`'s ~300 public functions, one per sibling file next to the UMD
/// `main` bundle, are the motivating case; see the call site).
///
/// Skips hidden directories and the conventional non-API directories a real
/// npm tarball's own `files`/`.npmignore` selection would already have
/// excluded were this crate reading that instead of walking the checkout
/// directly — a package's test suite is not its public surface. Individual
/// file parse failures are handled by the normal skip-on-panic path in
/// `graph::build_and_extract`, not here; this function only walks the
/// filesystem.
fn deep_import_roots(root: &Path) -> Vec<PathBuf> {
    const SKIP_DIRS: &[&str] = &[
        "node_modules",
        "test",
        "tests",
        "__tests__",
        "example",
        "examples",
        "coverage",
        "benchmark",
        "benchmarks",
        "docs",
        "doc",
    ];

    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && (has_extension(name, ".js") || has_extension(name, ".d.ts"))
            {
                out.push(path);
            }
        }
    }
    out
}

fn is_package_json_export(spec: &str) -> bool {
    let trimmed = spec.trim_start_matches("./").trim_start_matches('/');
    trimmed == "package.json" || trimmed.ends_with("/package.json")
}

/// True when `path` is a JS/TS module the OXC parser can handle.
pub(crate) fn is_ts_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    has_extension(name, ".d.ts")
        || has_extension(name, ".d.mts")
        || has_extension(name, ".d.cts")
        || has_extension(name, ".ts")
        || has_extension(name, ".tsx")
        || has_extension(name, ".mts")
        || has_extension(name, ".cts")
        || has_extension(name, ".js")
        || has_extension(name, ".mjs")
        || has_extension(name, ".cjs")
        || has_extension(name, ".jsx")
}

/// Case-insensitive suffix test for a file name. Kept as explicit suffix
/// strings (rather than `Path::extension`) because `.d.ts`/`.d.mts`/`.d.cts`
/// are two-part extensions that `Path::extension` would misread as `ts`.
pub(crate) fn has_extension(name: &str, ext: &str) -> bool {
    name.len() >= ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

fn collect_export_specifiers(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if !is_package_json_export(s) {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_export_specifiers(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if key.starts_with('.') && !is_package_json_export(key) {
                    out.push(key.clone());
                }
                // Recurse into every value, including a `.`-prefixed subpath
                // key's own conditional-exports object: `"." : { "import":
                // "./wrapper.mjs", "require": "./index.js" }` (real shape,
                // `ws`) must yield `"./wrapper.mjs"` as a candidate, not just
                // the literal string `"."`. Condition-name keys (`"import"`,
                // `"require"`, `"browser"`, …) never start with `.`, so they
                // fall into this same call and are walked one level further
                // until a string leaf is reached — this cannot mistake a
                // condition name for another subpath export.
                collect_export_specifiers(val, out);
            }
        }
        _ => {}
    }
}

/// Module-name assignment: convert a file path to a short dotted name.
pub(crate) fn specifier_to_module_name(specifier: &str) -> String {
    let clean = specifier
        .split('?')
        .next()
        .unwrap_or(specifier)
        .split('#')
        .next()
        .unwrap_or(specifier);
    let last = clean.split('/').next_back().unwrap_or(clean);
    let name = strip_ts_suffix(last);
    if name.is_empty() {
        clean.to_string()
    } else {
        name.to_string()
    }
}

pub(crate) fn strip_ts_suffix(value: &str) -> &str {
    value
        .trim_end_matches(".d.ts")
        .trim_end_matches(".d.tsx")
        .trim_end_matches(".d.mts")
        .trim_end_matches(".d.cts")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".mts")
        .trim_end_matches(".cts")
        .trim_end_matches(".js")
        .trim_end_matches(".mjs")
        .trim_end_matches(".cjs")
        .trim_end_matches(".jsx")
}

#[cfg(test)]
mod tests {
    use super::discover_entry_points;

    /// Build a synthetic package under a fresh temp dir: `package.json` with
    /// `main: "index.js"` and, when `exports_field` is `Some`, an `exports`
    /// key set to it; `index.js` (the only file `main`/`exports` can reach);
    /// and `sibling.js`, a second top-level file `index.js` never
    /// `require()`s or imports — the only way to it is Node's classic
    /// "no `exports` field means every file is importable" rule.
    fn write_fixture(exports_field: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let manifest = exports_field.map_or_else(
            || r#"{"name":"fixture","main":"index.js"}"#.to_string(),
            |exports| format!(r#"{{"name":"fixture","main":"index.js","exports":{exports}}}"#),
        );
        std::fs::write(root.join("package.json"), manifest).expect("write package.json");
        std::fs::write(root.join("index.js"), "module.exports = {};\n").expect("write index.js");
        std::fs::write(root.join("sibling.js"), "module.exports = {};\n")
            .expect("write sibling.js");
        dir
    }

    /// Node's classic resolution rule: no `exports` field means every file
    /// in the package is real, importable API. `deep_import_roots` must
    /// recover `sibling.js` even though nothing `main`/the resolver's
    /// default entry reaches ever references it.
    #[test]
    fn deep_import_roots_recovers_sibling_files_when_package_has_no_exports_field() {
        let dir = write_fixture(None);
        let found = discover_entry_points(dir.path()).expect("discovery should succeed");
        assert!(
            found.iter().any(|p| p.ends_with("sibling.js")),
            "expected sibling.js among {found:?} — package.json has no `exports` field, so \
             every file should be a declaration root"
        );
    }

    /// The same fallback must NOT fire once `package.json` declares an
    /// `exports` map: that map is Node's own authoritative statement of
    /// what is importable, and `sibling.js` is deliberately not in it. This
    /// is the real-package shape `ws` 8.16.0 has — verified separately
    /// against `result/ws-8.16.0/package.json` (`exports` present) —
    /// this test pins the same property on a minimal, controlled fixture so
    /// it doesn't depend on the corpus checkout being present.
    #[test]
    fn deep_import_roots_does_not_fire_when_package_has_exports_field() {
        let dir = write_fixture(Some(r#"{".":"./index.js"}"#));
        let found = discover_entry_points(dir.path()).expect("discovery should succeed");
        assert!(
            !found.iter().any(|p| p.ends_with("sibling.js")),
            "sibling.js must not be discovered — package.json declares an `exports` map, so \
             Node's classic \"every file is importable\" fallback must not apply. Found: \
             {found:?}"
        );
    }
}
