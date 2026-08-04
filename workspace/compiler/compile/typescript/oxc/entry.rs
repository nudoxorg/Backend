//! Entry-point + declaration-root discovery (OXC-PLAN §Phase 1), resolver-first.
//!
//! Replaces the hand-rolled `.d.ts` discovery: a package root resolves through
//! `oxc_resolver` (`ts.resolveModuleName` semantics), with the existing
//! conventional fallbacks (`mod.ts`, `index.ts`, ...) and the `repo:` hint kept
//! for manifest-less trees.
//!
//! # Algorithm
//!
//! 1. Build the `Resolver` once via [`make_resolver`] with `.d.ts`-first
//!    `ResolveOptions` (OXC-PLAN §Phase 1.1).
//! 2. [`discover_entry_points`] calls `resolver.resolve(root, ".")` to get the
//!    primary entry.  If that fails it tries the `repo:` hint path (from
//!    `entry_point.rs`) and then the conventional fallbacks
//!    (`mod.ts`/`index.ts`/`src/mod.ts`/`src/index.ts`/`index.d.ts`).
//! 3. After a primary entry is found, the package's `exports` map is read
//!    from `package.json` via `serde_json` and each candidate is resolved
//!    through the same resolver (the resolver exposes `types()`/`typings()` on
//!    `Resolution` but not arbitrary package.json fields such as `exports`).
//! 4. The resulting set is deduped and returned as absolute `PathBuf`s
//!    (no `file://` URLs — the `ModuleSpecifier` round-trip from the deno path
//!    is intentionally absent per OXC-PLAN §Phase 3.5).

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use oxc_resolver::{ResolveError, ResolveOptions, Resolver};

use super::error::Package;

// ─── Public entry points ─────────────────────────────────────────────────────

/// Build the resolver used for both entry discovery and graph edges, configured
/// for TypeScript `.d.ts`-first resolution (OXC-PLAN §Phase 1.1 ResolveOptions).
///
/// The returned `Resolver` is cheap to clone (it wraps an `Arc`-backed cache)
/// and is intended to be reused for the whole package build.
pub(crate) fn make_resolver() -> Resolver {
    Resolver::new(ResolveOptions {
        // Condition names: "types" first so package exports map routes to
        // declaration files before runtime JS.
        condition_names: vec![
            "types".to_string(),
            "import".to_string(),
            "node".to_string(),
        ],
        // Main fields: types/typings override module/main for declaration-first
        // resolution.
        main_fields: vec![
            "types".to_string(),
            "typings".to_string(),
            "module".to_string(),
            "main".to_string(),
        ],
        // Extensions tried in order: declaration files first, then source,
        // then JS, then JSON.
        extensions: vec![
            ".d.ts".to_string(),
            ".ts".to_string(),
            ".tsx".to_string(),
            ".js".to_string(),
            ".json".to_string(),
        ],
        // Extension aliases: when an import ends in .js/.mjs/.cjs the resolver
        // tries the corresponding TS/declaration variants first.
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
        // Recognise `node:*` builtins so they do not surface as NotFound errors
        // during graph traversal.
        builtin_modules: true,
        // Do not read NODE_PATH from the environment — the pipeline is hermetic
        // (sealed-input trees have no ambient node_modules).
        node_path: false,
        ..ResolveOptions::default()
    })
}

/// Discover the declaration roots for the package at `root`.
///
/// Uses a freshly-built resolver (via [`make_resolver`]).  Callers that already
/// hold a resolver should use [`discover_entry_points_with`] to avoid
/// allocating a second resolver.
pub(crate) fn discover_entry_points(root: &Path) -> Result<Vec<PathBuf>, Package> {
    let resolver = make_resolver();
    discover_entry_points_with(&resolver, root, None)
}

/// Discover the declaration roots for the package at `root`, given a pre-built
/// `resolver` and an optional `repo:` entry hint (relative path from root).
///
/// Resolves the package entry (`resolver.resolve(root, ".")`), expands the
/// `exports` fan-out, and falls back to conventional entry points and the
/// `repo:` hint. Returns absolute, deduped module file paths.
pub(crate) fn discover_entry_points_with(
    resolver: &Resolver,
    root: &Path,
    repo_hint: Option<&str>,
) -> Result<Vec<PathBuf>, Package> {
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();

    // ── 1. repo: hint (manifest-less trees, e.g. monorepo source checkouts) ──
    if let Some(hint) = repo_hint {
        let candidate = root.join(hint);
        if candidate.is_file() && is_ts_module_path(&candidate) {
            found.insert(candidate);
        }
    }

    // ── 2. resolver.resolve(root, ".") — package.json exports/types/main ──
    match resolver.resolve(root, ".") {
        Ok(resolution) => {
            let path = resolution.into_path_buf();
            // Resolver may land on package.json / assets when main fields
            // point at non-code; only seed TS/JS modules into the graph.
            if path.is_file() && is_ts_module_path(&path) {
                found.insert(path);
            }
        }
        Err(ResolveError::Builtin { .. }) => {
            // root is somehow a builtin — not a real package; skip.
        }
        Err(ResolveError::NotFound(_)) => {
            // No package.json or no matching entry — fall through to
            // conventional fallbacks below.
        }
        Err(ResolveError::PackagePathNotExported { .. }) => {
            // exports map exists but "." is not exported — fall through;
            // exports fan-out will still expand non-root subpaths.
        }
        Err(other) => {
            // For diagnostic purposes, surface non-trivial errors as warnings
            // rather than hard failures; fall through to fallbacks.
            tracing::debug!(
                root = %root.display(),
                error = %other,
                "resolver.resolve(root, \".\") failed; trying fallbacks"
            );
        }
    }

    // ── 3. exports fan-out: read package.json and resolve each exports entry ──
    if let Ok(roots) = exports_entry_points(resolver, root) {
        found.extend(roots);
    }

    // ── 4. Conventional fallbacks for manifest-less / bare repos ──
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
                break; // first match is sufficient as primary fallback
            }
        }
    }

    if found.is_empty() {
        return Err(Package::EntryPointDiscoveryFailed { path: root.to_path_buf() });
    }

    Ok(found.into_iter().collect())
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Read the `exports` field from `package.json` under `package_root` and
/// resolve each string-valued candidate through `resolver`.  Returns only paths
/// that exist on disk, deduped.
fn exports_entry_points(
    resolver: &Resolver,
    package_root: &Path,
) -> Result<Vec<PathBuf>, Package> {
    let manifest_path = package_root.join("package.json");
    if !manifest_path.is_file() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(&manifest_path)?;
    let manifest: serde_json::Value = serde_json::from_str(&content)?;

    let Some(exports) = manifest.get("exports") else {
        return Ok(vec![]);
    };

    let mut candidates: Vec<String> = Vec::new();
    collect_export_specifiers(exports, &mut candidates);

    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for spec in candidates {
        // Resolve each specifier from the package root.
        match resolver.resolve(package_root, &spec) {
            Ok(resolution) => {
                let path = resolution.into_path_buf();
                if path.is_file() && is_ts_module_path(&path) {
                    out.insert(path);
                }
            }
            Err(ResolveError::Builtin { .. })
            | Err(ResolveError::NotFound(_))
            | Err(ResolveError::PackagePathNotExported { .. })
            | Err(ResolveError::Ignored(_)) => {
                // Not all subpath exports are meaningful for documentation
                // (e.g. internal `#` imports, runtime-only paths).
            }
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

    Ok(out.into_iter().collect())
}

/// True for package.json self-exports (`"./package.json"`, `"package.json"`,
/// …). These are a common npm pattern so tooling can read the manifest, but
/// they are not TypeScript modules and must never seed the OXC graph.
fn is_package_json_export(spec: &str) -> bool {
    let trimmed = spec.trim_start_matches("./").trim_start_matches('/');
    trimmed == "package.json" || trimmed.ends_with("/package.json")
}

/// True when `path` looks like a JS/TS module the OXC parser can lower.
/// Excludes JSON (and other non-code assets) that the resolver may return for
/// `exports["./package.json"]` or bare `.json` main fields.
pub(crate) fn is_ts_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    // Declaration + source TypeScript.
    if lower.ends_with(".d.ts")
        || lower.ends_with(".d.mts")
        || lower.ends_with(".d.cts")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".mts")
        || lower.ends_with(".cts")
    {
        return true;
    }
    // Runtime JS (often the only public surface for pure-JS packages).
    lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") || lower.ends_with(".jsx")
}

/// Recursively collect string-valued `exports` specifiers from a `serde_json`
/// value into `out`.
///
/// The exports map may be:
/// - A plain string (e.g. `"exports": "./index.js"`) — single specifier.
/// - An object whose keys are subpath patterns like `"."`, `"./utils"` — each
///   key becomes the specifier we resolve.
/// - An object whose keys are condition names (`"types"`, `"import"`, …) —
///   we recurse into values to extract the underlying path strings.
/// - An array — elements are tried in order.
fn collect_export_specifiers(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            // Bare path like "./index.js".  Skip package.json self-references
            // (both `"./package.json"` and `"package.json"`).
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
                if key.starts_with('.') {
                    // Subpath export key (e.g. ".", "./utils") — this is the
                    // specifier; resolve its value to find the file.
                    //
                    // Skip `"./package.json"` keys: npm packages commonly
                    // re-export the manifest (zod, react, …) so consumers can
                    // `import pkg from "zod/package.json"`.  Resolving that
                    // key yields a JSON file that the TS parser rejects.
                    if !is_package_json_export(key) {
                        out.push(key.clone());
                    }
                    // Resolver handles condition objects via condition_names;
                    // no need to recurse into the value for subpath keys.
                    let _ = val;
                } else {
                    // Condition key ("types", "import", "default", …) — recurse
                    // into the value; the outer key is not a resolvable specifier.
                    collect_export_specifiers(val, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_resolver_does_not_panic() {
        // Smoke test: building the resolver must not panic.
        let _resolver = make_resolver();
    }

    #[test]
    fn collect_export_specifiers_plain_string() {
        let val = serde_json::json!("./index.js");
        let mut out = Vec::new();
        collect_export_specifiers(&val, &mut out);
        assert_eq!(out, vec!["./index.js"]);
    }

    #[test]
    fn collect_export_specifiers_subpath_map() {
        let val = serde_json::json!({
            ".": "./index.js",
            "./utils": "./utils.js"
        });
        let mut out = Vec::new();
        collect_export_specifiers(&val, &mut out);
        // Keys starting with "." are treated as specifiers.
        assert!(out.contains(&".".to_string()));
        assert!(out.contains(&"./utils".to_string()));
    }

    #[test]
    fn collect_export_specifiers_conditions_object() {
        let val = serde_json::json!({
            "types": "./index.d.ts",
            "import": "./index.mjs",
            "require": "./index.cjs"
        });
        let mut out = Vec::new();
        collect_export_specifiers(&val, &mut out);
        // Condition keys recurse into values.
        assert!(out.contains(&"./index.d.ts".to_string()));
        assert!(out.contains(&"./index.mjs".to_string()));
        assert!(out.contains(&"./index.cjs".to_string()));
    }

    #[test]
    fn collect_export_specifiers_skips_package_json() {
        let val = serde_json::json!("./package.json");
        let mut out = Vec::new();
        collect_export_specifiers(&val, &mut out);
        assert!(out.is_empty(), "package.json self-references should be skipped");
    }

    #[test]
    fn collect_export_specifiers_skips_package_json_export_key() {
        // Real-world shape from zod@3.24.2 / many other packages:
        //   "exports": { ".": { "types": "./index.d.ts" }, "./package.json": "./package.json" }
        let val = serde_json::json!({
            ".": { "types": "./index.d.ts", "import": "./index.mjs" },
            "./package.json": "./package.json",
            "./locales/*": "./lib/locales/*"
        });
        let mut out = Vec::new();
        collect_export_specifiers(&val, &mut out);
        assert!(out.contains(&".".to_string()));
        assert!(out.contains(&"./locales/*".to_string()));
        assert!(
            !out.iter().any(|s| is_package_json_export(s)),
            "exports[\"./package.json\"] must not seed the module graph: {out:?}"
        );
    }

    #[test]
    fn is_ts_module_path_accepts_ts_rejects_json() {
        assert!(is_ts_module_path(Path::new("/pkg/index.d.ts")));
        assert!(is_ts_module_path(Path::new("/pkg/mod.ts")));
        assert!(is_ts_module_path(Path::new("/pkg/lib/index.mjs")));
        assert!(!is_ts_module_path(Path::new("/pkg/package.json")));
        assert!(!is_ts_module_path(Path::new("/pkg/data.json")));
        assert!(!is_ts_module_path(Path::new("/pkg/README.md")));
    }

    #[test]
    fn discover_entry_points_nonexistent_root_returns_error() {
        let root = PathBuf::from("/tmp/oxc_entry_test_nonexistent_xyzzy_12345");
        let result = discover_entry_points(&root);
        assert!(
            result.is_err(),
            "nonexistent root should return EntryPointDiscoveryFailed"
        );
    }
}
