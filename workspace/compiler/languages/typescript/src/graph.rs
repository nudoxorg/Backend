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
use oxc_ast::ast::{Argument, Expression, Program, Statement};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use crate::{
    entry::{is_ts_module_path, make_resolver, specifier_to_module_name},
    extract::{ModuleFacts, PackageError, decl::extract_module},
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
                        if resolved.is_file()
                            && is_ts_module_path(&resolved)
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

            // ── Enqueue `require("…")` edges ────────────────────────────────────
            // `module_record.requested_modules` (above) is populated from ES
            // `import`/`export from` syntax only — OXC never looks inside a
            // CommonJS `require()` call for it. A CommonJS file whose whole
            // body is one statement invisible to per-declaration extraction
            // (`debug`'s `src/index.js` is a single `IfStatement`:
            // `if (...) { module.exports = require('./browser.js'); } else {
            // module.exports = require('./node.js'); }`) is otherwise a dead
            // end for graph walking even though the real target is one
            // `require()` call away. Only top-level call sites are scanned —
            // `require()` calls inside a nested function body are as
            // unreachable to this walker as any other nested declaration
            // (see `CommonJsExports`'s doc comment in `extract/decl.rs` for
            // the same boundary applied to export recognition).
            for spec_str in top_level_require_targets(&parse.program) {
                if is_node_builtin(&spec_str) {
                    continue;
                }
                match resolver.resolve(parent_dir, &spec_str) {
                    Ok(res) => {
                        let resolved = res.into_path_buf();
                        if resolved.is_file()
                            && is_ts_module_path(&resolved)
                            && visited.insert(resolved.clone())
                        {
                            queue.push_back(resolved);
                        }
                    }
                    Err(err) => {
                        tracing::debug!(
                            module = %module_path.display(),
                            specifier = %spec_str,
                            error = %err,
                            "resolver could not resolve require() target; skipping edge"
                        );
                    }
                }
            }

            // ── Enqueue `/// <reference path="..." />` targets ─────────────────
            // A `.d.ts` file that composes its API purely through triple-slash
            // reference directives (the classic ambient-declaration authoring
            // style — every file in `@types/node` does this; nothing in that
            // package uses `import`/`export from`) is otherwise invisible to
            // this walker: OXC's `module_record` only tracks ES import/export
            // edges, and a `///` reference directive is, syntactically, just a
            // line comment. Without this, `@types/node` measured as 108 real
            // `.d.ts` files on disk but only 1 ever reached (its `index.d.ts`,
            // itself containing zero declarations of its own) — a "success"
            // with 2 stub entries that is functionally indistinguishable from
            // an empty package. `path=` is a literal relative file path per the
            // TypeScript spec, not a resolver specifier, so it is joined
            // directly rather than run through `main_fields`/`exports`
            // resolution. `lib=`/`types=` directives are deliberately not
            // followed here: `lib=` names one of the TypeScript compiler's own
            // bundled `lib.*.d.ts` files, which does not exist anywhere in the
            // package's source tree, and no `types=` directive appears in the
            // npm corpus this producer has been swept against (see
            // `graph::tests` and the corpus sweep report) — both are left as a
            // documented gap rather than guessed at.
            for target in triple_slash_path_refs(&source) {
                let resolved = parent_dir.join(&target);
                if resolved.is_file()
                    && is_ts_module_path(&resolved)
                    && visited.insert(resolved.clone())
                {
                    queue.push_back(resolved);
                } else if !resolved.is_file() {
                    tracing::debug!(
                        module = %module_path.display(),
                        target = %target,
                        "triple-slash reference path did not resolve to a file; skipping edge"
                    );
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

/// Pull every `path="..."` target out of the file's `/// <reference .../>`
/// directives.
///
/// No XML/HTML parser: the directive is a fixed one-line TypeScript syntax
/// (never nested, never multi-line — the compiler itself only recognizes it
/// inside a single-line `///` comment), so a line-oriented scan is both
/// sufficient and exactly as permissive as `tsc`'s own recognizer. Directives
/// are conventionally the first lines of a file, but nothing here assumes
/// that; every line is checked.
fn triple_slash_path_refs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("///") else {
            continue;
        };
        if !rest.contains("<reference") {
            continue;
        }
        if let Some(path) = reference_attr(rest, "path") {
            out.push(path);
        }
    }
    out
}

/// Extract `name="value"` from one `/// <reference .../>` directive's body.
fn reference_attr(directive_text: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = directive_text.find(&needle)? + needle.len();
    let end = directive_text[start..].find('"')?;
    Some(directive_text[start..start + end].to_string())
}

/// Every specifier passed to a top-level `require(...)` call in `program`:
/// a `VariableDeclaration` initializer (`const x = require('./y');`) or an
/// `ExpressionStatement`'s assignment RHS / bare call
/// (`module.exports = require('./y');`, `require('./y');`). Nested (inside a
/// function body) `require()` calls are not scanned — see the call site's
/// doc comment.
fn top_level_require_targets<'a>(program: &'a Program<'a>) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in program.body.iter() {
        match stmt {
            Statement::VariableDeclaration(v) => {
                for d in v.declarations.iter() {
                    if let Some(init) = &d.init
                        && let Some(spec) = as_require_call(init)
                    {
                        out.push(spec.to_string());
                    }
                }
            }
            Statement::ExpressionStatement(expr_stmt) => match &expr_stmt.expression {
                Expression::AssignmentExpression(assign) => {
                    if let Some(spec) = as_require_call(&assign.right) {
                        out.push(spec.to_string());
                    }
                }
                other => {
                    if let Some(spec) = as_require_call(other) {
                        out.push(spec.to_string());
                    }
                }
            },
            _ => {}
        }
    }
    out
}

/// `expr` reduced to `Some(specifier)` when it is exactly a call
/// `require("specifier")` — callee a bare identifier named `require`, one
/// string-literal argument. Anything else (a computed specifier, a renamed
/// `require`, `require.resolve(...)`) is `None`; a specifier this crate
/// cannot read is not one it should guess at.
fn as_require_call<'a>(expr: &'a Expression<'a>) -> Option<&'a str> {
    let Expression::CallExpression(call) = expr else {
        return None;
    };
    let Expression::Identifier(callee) = &call.callee else {
        return None;
    };
    if callee.name != "require" {
        return None;
    }
    let Some(Argument::StringLiteral(spec)) = call.arguments.first() else {
        return None;
    };
    Some(spec.value.as_str())
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

#[cfg(test)]
mod tests {
    use super::triple_slash_path_refs;

    #[test]
    fn path_directive_yields_its_target() {
        let src = "/// <reference path=\"./globals.d.ts\" />\nexport {};\n";
        assert_eq!(triple_slash_path_refs(src), vec!["./globals.d.ts"]);
    }

    #[test]
    fn lib_and_types_directives_are_not_treated_as_path_targets() {
        let src = concat!(
            "/// <reference lib=\"es2020\" />\n",
            "/// <reference types=\"node\" />\n",
            "export {};\n",
        );
        assert!(triple_slash_path_refs(src).is_empty());
    }

    #[test]
    fn multiple_path_directives_are_all_collected_in_order() {
        let src = concat!(
            "/// <reference path=\"a.d.ts\" />\n",
            "/// <reference path=\"b/c.d.ts\" />\n",
        );
        assert_eq!(triple_slash_path_refs(src), vec!["a.d.ts", "b/c.d.ts"]);
    }

    #[test]
    fn ordinary_comments_and_code_produce_no_targets() {
        let src = "// just a comment\n/** jsdoc */\nexport const x = 1;\n";
        assert!(triple_slash_path_refs(src).is_empty());
    }
}
