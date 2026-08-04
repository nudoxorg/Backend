//! Module-graph construction: discover entry points, walk imports, extract.
//!
//! Ported from `workspace/compiler/compile/typescript/oxc/graph.rs`.
//! Key invariant: **one `Allocator` per module, scoped and dropped inside the
//! extraction block**. By the time this function returns, every module's AST
//! arena has been freed; only `Vec<ModuleFacts>` — fully owned, no arena refs —
//! survives.
//!
//! # One-pass guarantee
//! We walk the module graph breadth-first, calling `extract::decl::extract_module`
//! once per module. The returned `ModuleFacts` are accumulated into `Vec<ModuleFacts>`.
//! No intermediate path→members map is built.

use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use crate::{
    entry::{is_ts_module_path, make_resolver, specifier_to_module_name},
    extract::{decl::extract_module, ModuleFacts, PackageError},
};

// ── Public API ─────────────────────────────────────────────────────────────────

/// Walk the module graph starting from `entry_points` and return owned
/// `ModuleFacts` for every reachable module.
///
/// `root` is the package root used to anchor resolver calls.
/// Each module gets its own `Allocator`; all arenas are dropped before this
/// function returns.
pub fn build_and_extract(
    entry_points: &[PathBuf],
    root: &Path,
) -> Result<Vec<ModuleFacts>, PackageError> {
    let resolver = make_resolver();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut results: Vec<ModuleFacts> = Vec::new();

    for ep in entry_points {
        if visited.insert(ep.clone()) {
            queue.push_back(ep.clone());
        }
    }

    while let Some(module_path) = queue.pop_front() {
        let source = match std::fs::read_to_string(&module_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %module_path.display(),
                    error = %e,
                    "failed to read source; skipping module"
                );
                continue;
            }
        };

        // ── One Allocator, scoped to this block ───────────────────────────────
        let module_facts: ModuleFacts = {
            let allocator = Allocator::default();
            let source_type = source_type_for(&module_path);

            let parse = Parser::new(&allocator, &source, source_type)
                .with_options(ParseOptions {
                    parse_regular_expression: false,
                    allow_return_outside_function: false,
                    ..ParseOptions::default()
                })
                .parse();

            if parse.panicked {
                tracing::warn!(
                    path = %module_path.display(),
                    "OXC parser panicked; skipping module"
                );
                continue;
            }

            // UNCERTAINTY: `.with_scope_tree_child_ids(false)` was present in a draft
            // but the method name in 0.139.0 may differ. Removed to avoid a compile error.
            // If scope-tree child ids are needed for JSDoc lookup, add back via the
            // correct method name.
            // Note: JSDoc parsing is automatic when the `jsdoc` feature is enabled.
            // `SemanticBuilder` in 0.139.0 has no `with_jsdoc` method.
            let semantic_result = SemanticBuilder::new()
                .with_check_syntax_error(false)
                .build(&parse.program);

            let semantic = semantic_result.semantic;
            // In 0.139.0 the module record lives on the parse result, not on Semantic.
            let module_record = &parse.module_record;

            // ── Enqueue imports ───────────────────────────────────────────────
            let parent_dir = module_path.parent().unwrap_or(root);
            for (specifier, _) in module_record.requested_modules.iter() {
                let spec_str = specifier.as_str();
                // Skip node built-ins and non-relative/non-absolute specifiers
                // that the resolver can't locate on disk.
                if is_node_builtin(spec_str) {
                    continue;
                }
                match resolver.resolve(parent_dir, spec_str) {
                    Ok(res) => {
                        let resolved = res.into_path_buf();
                        if resolved.is_file() && is_ts_module_path(&resolved)
                            && visited.insert(resolved.clone())
                        {
                            queue.push_back(resolved);
                        }
                    }
                    Err(err) => {
                        tracing::debug!(
                            module = %module_path.display(),
                            specifier = spec_str,
                            error = %err,
                            "resolver could not resolve import; skipping edge"
                        );
                    }
                }
            }

            // ── Extract to owned ──────────────────────────────────────────────
            let module_name = path_to_module_name(&module_path, root);
            extract_module(
                &source,
                &semantic,
                &parse.program,
                &module_path,
                module_record,
                module_name,
            )
            // `allocator`, `parse`, `semantic`, `module_record` all dropped here.
        };

        results.push(module_facts);
    }

    Ok(results)
}

// ── Helpers ─────────────────────────────────────────────────────────────────────

fn source_type_for(path: &Path) -> SourceType {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if name.ends_with(".tsx") {
        SourceType::tsx()
    } else if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
        // Declaration files are TypeScript but never JSX; flag them as module.
        SourceType::ts()
    } else if name.ends_with(".ts") || name.ends_with(".mts") || name.ends_with(".cts") {
        SourceType::ts()
    } else {
        SourceType::mjs()
    }
}

fn path_to_module_name(path: &Path, root: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let s = relative.to_string_lossy();
    // Use entry.rs's suffix-stripping helper on the last component only.
    specifier_to_module_name(&s)
}

fn is_node_builtin(specifier: &str) -> bool {
    // Node built-ins start with `node:` or are well-known bare specifiers.
    if specifier.starts_with("node:") {
        return true;
    }
    // Bare specifiers without a leading `.` or `/` are typically npm packages
    // or builtins; the resolver handles npm packages, but if the resolver fails
    // that's logged at debug level and we skip. Builtins are filtered here by
    // the `node:` prefix check above.
    false
}
