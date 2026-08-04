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

use crate::extract::PackageError;

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
        Err(ResolveError::Builtin { .. })
        | Err(ResolveError::NotFound(_))
        | Err(ResolveError::PackagePathNotExported { .. }) => {}
        Err(other) => {
            tracing::debug!(
                root = %root.display(),
                error = %other,
                "resolver.resolve(root, \".\") failed; trying fallbacks"
            );
        }
    }

    if let Ok(roots) = exports_entry_points(resolver, root) {
        found.extend(roots);
    }

    if found.is_empty() {
        for rel in &["mod.ts", "index.ts", "src/mod.ts", "src/index.ts", "index.d.ts"] {
            let candidate = root.join(rel);
            if candidate.is_file() {
                found.insert(candidate);
                break;
            }
        }
    }

    if found.is_empty() {
        return Err(PackageError::EntryPointDiscoveryFailed { path: root.to_path_buf() });
    }

    Ok(found.into_iter().collect())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn exports_entry_points(
    resolver: &Resolver,
    package_root: &Path,
) -> Result<Vec<PathBuf>, PackageError> {
    let manifest_path = package_root.join("package.json");
    if !manifest_path.is_file() {
        return Ok(vec![]);
    }

    let content = fs::read_to_string(&manifest_path).map_err(PackageError::Io)?;
    let manifest: serde_json::Value =
        serde_json::from_str(&content).map_err(PackageError::Serialization)?;

    let Some(exports) = manifest.get("exports") else {
        return Ok(vec![]);
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
            Err(ResolveError::Builtin { .. })
            | Err(ResolveError::NotFound(_))
            | Err(ResolveError::PackagePathNotExported { .. })
            | Err(ResolveError::Ignored(_)) => {}
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

fn is_package_json_export(spec: &str) -> bool {
    let trimmed = spec.trim_start_matches("./").trim_start_matches('/');
    trimmed == "package.json" || trimmed.ends_with("/package.json")
}

/// True when `path` is a JS/TS module the OXC parser can handle.
pub(crate) fn is_ts_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".d.ts")
        || lower.ends_with(".d.mts")
        || lower.ends_with(".d.cts")
        || lower.ends_with(".ts")
        || lower.ends_with(".tsx")
        || lower.ends_with(".mts")
        || lower.ends_with(".cts")
        || lower.ends_with(".js")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
        || lower.ends_with(".jsx")
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
                if key.starts_with('.') {
                    if !is_package_json_export(key) {
                        out.push(key.clone());
                    }
                } else {
                    collect_export_specifiers(val, out);
                }
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
    if name.is_empty() { clean.to_string() } else { name.to_string() }
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
