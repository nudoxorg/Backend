//! Module-graph construction + pass-1 extraction driver (OXC-PLAN §Phase 1.2).
//!
//! Worklist from the entry roots. Per module: parse (`preserve_parens: false`,
//! `SourceType::from_path` with `.d_ts()` fallback), build semantic, extract to
//! an owned [`ModuleFacts`] (dropping the arena), then enqueue resolved edges
//! (`requested_modules` ∪ triple-slash refs, resolved via `resolve_dts`).
//! Errors are classified: `NotFound` → external edge, `Builtin` → node builtin,
//! others → diagnostic.

use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_resolver::ResolveError;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use super::{
    entry,
    error::Package,
    extract::{self, ModuleFacts},
};

/// Build the reachable module graph from `roots` and extract each module into
/// owned [`ModuleFacts`]. One `Allocator` per module (reset/dropped after
/// extraction) keeps `Allocator: !Sync` from leaking across modules.
///
/// Per module: `Parser::new(&alloc, src, SourceType::from_path(p).unwrap_or_else(
/// |_| SourceType::d_ts())).with_options(ParseOptions { preserve_parens: false,
/// ..Default::default() }).parse()`, then `SemanticBuilder::new()
/// .with_check_syntax_error(false).with_build_nodes(true).build(&program)`,
/// then `extract::extract_module(...)`. Edges = `module_record.requested_modules`
/// ∪ triple-slash refs, resolved via `entry::make_resolver().resolve_dts(dir,
/// spec)`; classify `NotFound`/`Builtin`/other.
pub(crate) fn build_and_extract(roots: Vec<PathBuf>) -> Result<Vec<ModuleFacts>, Package> {
    let resolver = entry::make_resolver();

    // Worklist BFS.  Visited set is keyed by the canonical (resolved) path so
    // that different relative spellings of the same file are deduped.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    let mut all_facts: Vec<ModuleFacts> = Vec::new();

    // Seed the queue with the entry roots.  Canonicalise so the dedup set is
    // consistent from the very start.
    for root in roots {
        let canon = canonicalize_best_effort(&root);
        if visited.insert(canon.clone()) {
            queue.push_back(canon);
        }
    }

    while let Some(path) = queue.pop_front() {
        // Skip non-code assets (package.json self-exports, data JSON, …)
        // that slipped past entry discovery or edge resolution.  Parsing
        // JSON as TypeScript yields "Expected a semicolon…" hard failures.
        if !entry::is_ts_module_path(&path) {
            tracing::debug!(
                path = %path.display(),
                "graph: skipping non-TS/JS module path"
            );
            continue;
        }

        // ----------------------------------------------------------------
        // 1.  Read source.  The owned `String` must outlive the arena scope
        //     — we declare it here, outside the block that creates the
        //     `Allocator`, so the borrow checker sees it as longer-lived.
        // ----------------------------------------------------------------
        let source: String = std::fs::read_to_string(&path).map_err(Package::Io)?;

        // ----------------------------------------------------------------
        // 2.  Per-module arena scope.
        //     Everything that borrows `'a` from the allocator lives inside
        //     this block.  `facts` (fully owned, no `'a`) is moved out.
        // ----------------------------------------------------------------
        let facts: ModuleFacts = {
            let allocator = Allocator::new();
            let source_type = SourceType::from_path(&path).unwrap_or_else(|_| SourceType::d_ts());

            let ret = Parser::new(&allocator, &source, source_type)
                .with_options(ParseOptions {
                    preserve_parens: false,
                    ..Default::default()
                })
                .parse();

            // Fatal parse: parser panicked (program will be empty).
            if ret.panicked {
                // Collect the first diagnostic message, if any, for context.
                let detail = ret
                    .diagnostics
                    .iter()
                    .next()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "parser panicked".to_string());
                return Err(Package::ParseFailed {
                    path: path.clone(),
                    detail,
                });
            }

            let sem = SemanticBuilder::new()
                .with_check_syntax_error(false)
                .with_build_nodes(true)
                .build(&ret.program);

            // ----------------------------------------------------------------
            // 3.  Collect edge specifiers before calling extract_module so the
            //     borrow of `ret` is fully resolved before we move `facts` out.
            // ----------------------------------------------------------------

            // 3a. ESM static imports / exports from the module record.
            let mut raw_specifiers: Vec<String> = ret
                .module_record
                .requested_modules
                .keys()
                .map(|s| s.as_str().to_string())
                .collect();

            // 3b. Triple-slash `/// <reference path="…">` and
            //     `/// <reference types="…">` directives, scanned from the
            //     leading comments on the program (they appear before the first
            //     statement).
            //
            //     We also handle `@deno-types` / `@ts-self-types` redirects here:
            //     a leading `// @deno-types="mod"` or `// @ts-self-types="mod"`
            //     comment immediately before an import redirects that import to
            //     `mod` instead.
            let mut deno_types_redirect: Option<String> = None;

            for comment in ret.program.comments.iter() {
                // The OXC `Comment.span` covers the full comment token bytes,
                // including the leading `//` or `/*`.  For a line comment the
                // span text is e.g. `// @deno-types="foo"` or
                // `/// <reference types="bar" />`.
                let span_text = &source[comment.span.start as usize..comment.span.end as usize];

                // Strip the comment sigil(s) to get the raw body text.
                // Line comments: `//` or `///`; block comments: `/* … */`.
                let body = if let Some(after_slashes) = span_text.strip_prefix("///") {
                    after_slashes.trim()
                } else if let Some(after_slashes) = span_text.strip_prefix("//") {
                    after_slashes.trim()
                } else if let Some(inner) = span_text
                    .strip_prefix("/*")
                    .and_then(|s| s.strip_suffix("*/"))
                {
                    inner.trim()
                } else {
                    span_text.trim()
                };

                // Triple-slash `<reference path=…>` / `<reference types=…>`.
                if let Some(rest) = body.strip_prefix("<reference ") {
                    if let Some(spec) = extract_reference_directive(rest) {
                        raw_specifiers.push(spec);
                    }
                } else if let Some(spec) = extract_pragma_redirect(body) {
                    // `@deno-types="…"` / `@ts-self-types="…"` pragma.
                    // Record the most recent one; it applies to the next import.
                    deno_types_redirect = Some(spec);
                }
            }

            // 3c. Apply @deno-types / @ts-self-types redirect (if any) to
            //     raw_specifiers.  Full fidelity would require correlating each
            //     pragma to the immediately-following import; for
            //     graph-reachability purposes (which specifiers to enqueue) we
            //     simply add the redirect target as an additional edge — the
            //     extra `.d.ts` path will be visited and its facts collected.
            //     The import-entry cross-reference is the concern of `link.rs`
            //     (pass 2), not the graph walker.
            if let Some(redirect) = deno_types_redirect {
                raw_specifiers.push(redirect);
            }

            // ----------------------------------------------------------------
            // 4.  Extract the module facts.  This borrows `source`, `semantic`,
            //     `program`, and `module_record` — all of which live until the
            //     end of this block.
            // ----------------------------------------------------------------
            let facts = extract::extract_module(
                &source,
                &sem.semantic,
                &ret.program,
                &path,
                &ret.module_record,
            )
            .map_err(Package::Parse)?;

            // ----------------------------------------------------------------
            // 5.  Resolve edges and enqueue reachable local modules.
            //
            //     We do this inside the arena scope because `requested_modules`
            //     borrows `'a` from the allocator.  The actual resolution only
            //     needs the string values, which we already collected above into
            //     `raw_specifiers`.
            // ----------------------------------------------------------------
            let containing_dir = path.parent().unwrap_or(Path::new("."));

            for spec in &raw_specifiers {
                // Skip bare node builtin shorthand (e.g. "node:fs", "fs") —
                // the resolver will classify these as `Builtin` anyway, but
                // skipping early avoids the syscall.
                match resolver.resolve_dts(&path, spec) {
                    Ok(resolution) => {
                        let resolved = resolution.into_path_buf();
                        // Don't enqueue package.json / JSON assets even if a
                        // relative import or exports map pointed at them.
                        if !entry::is_ts_module_path(&resolved) {
                            continue;
                        }
                        let canon = canonicalize_best_effort(&resolved);
                        if visited.insert(canon.clone()) {
                            queue.push_back(canon);
                        }
                    }
                    Err(ResolveError::NotFound(_)) => {
                        // External package or missing ambient module — skip,
                        // non-fatal.  The graph records the external edge via
                        // `ModuleFacts.imports` (populated by extract_module).
                    }
                    Err(ResolveError::Builtin { .. }) => {
                        // Node built-in (node:fs, path, …) — skip, non-fatal.
                    }
                    Err(ResolveError::PackagePathNotExported {
                        subpath,
                        package_path,
                        ..
                    }) => {
                        // The package.json `exports` map doesn't expose this
                        // subpath.  Non-fatal diagnostic; log via tracing so
                        // it surfaces in verbose mode without failing the build.
                        tracing::debug!(
                            specifier = %spec,
                            %subpath,
                            package = %package_path.display(),
                            "resolve_dts: subpath not exported — skipping edge"
                        );
                    }
                    Err(ResolveError::Ignored(_)) => {
                        // Explicitly marked `false` in the browser field —
                        // skip silently.
                    }
                    Err(other) => {
                        // All other errors (IO, malformed package.json, …) are
                        // non-fatal but worth logging so the caller can diagnose
                        // incomplete graphs.
                        tracing::debug!(
                            specifier = %spec,
                            from = %containing_dir.display(),
                            error = %other,
                            "resolve_dts: non-fatal resolution error — skipping edge"
                        );
                    }
                }
            }

            facts
            // `allocator`, `ret` (program + module_record), `sem` drop here.
        };

        all_facts.push(facts);
    }

    Ok(all_facts)
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Return the canonical (symlink-resolved, normalised) form of `path`.
/// Falls back to the input unchanged if `canonicalize` fails (e.g. the file
/// was just queued from a specifier that turned out not to exist — the
/// caller will error later when trying to read it).
fn canonicalize_best_effort(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Parse the body of a `/// <reference …>` directive comment and return the
/// specifier string, if present.
///
/// `rest` is everything after `"/ <reference "` in the raw comment span text
/// (the parser strips the leading `//` so we see `/ <reference …`).
///
/// Examples:
/// - `path="./types.d.ts" />`  → `"./types.d.ts"`
/// - `types="node" />`         → `"node"`
fn extract_reference_directive(rest: &str) -> Option<String> {
    // Accept both `path=` and `types=` attributes.
    for attr in ["path=", "types="] {
        if let Some(after_attr) = rest.find(attr).map(|i| &rest[i + attr.len()..]) {
            return extract_quoted_string(after_attr);
        }
    }
    None
}

/// Extract the value from a `@deno-types="…"` or `@ts-self-types="…"` pragma
/// that appears in a line comment.
///
/// `text` is the trimmed body of the comment (everything after `// `).
fn extract_pragma_redirect(text: &str) -> Option<String> {
    for prefix in ["@deno-types=", "@ts-self-types="] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return extract_quoted_string(rest);
        }
    }
    None
}

/// Extract the first `"…"` or `'…'`-quoted string from `s`.
fn extract_quoted_string(s: &str) -> Option<String> {
    let s = s.trim();
    let (quote, rest) = if s.starts_with('"') {
        ('"', &s[1..])
    } else if s.starts_with('\'') {
        ('\'', &s[1..])
    } else {
        return None;
    };
    rest.find(quote).map(|end| rest[..end].to_string())
}
