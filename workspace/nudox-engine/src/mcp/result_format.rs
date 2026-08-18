//! Compact Markdown projections for typed MCP results.
//!
//! This module deliberately sits after the typed tool layer.  The typed
//! structs remain the source of truth; this file only chooses a representation
//! for an agent-facing text response.  It is standalone so the MCP server can
//! wire it at the transport boundary without making the engine/tool methods
//! depend on Markdown or on rmcp's result types.
//!
//! The renderer has three invariants:
//!
//! * repeated relationships are tables or one-line metadata, not repeated
//!   object wrappers;
//! * exact source and schema payloads are never truncated or normalised; and
//! * values supplied by a package or query cannot create a new Markdown row,
//!   heading, or code fence.

use std::fmt::Write as _;

use crate::mcp::key::SymbolKeyDto;
use crate::mcp::tools::{
    CompactSymbolDoc, DiffVersionsResult, EdgeCoverageNote, GetOccurrencesResult,
    IndexPackageResult, IndexResult, ListVersionsResult, LoadedPackagesResult, PackagesResult,
    QueryResult, QueryResultRow, ReferenceCoverage, RefsResult, SchemaResult,
    SearchResult, SelectVersionResult, SemanticSearchResult, SemanticStatus, SymbolsResult,
    UsageRow, UsagesResult,
};
use crate::wire::{DiffVerdict, KeyTierLabel, KindTag, PackageDiff, SigToken, TimelineChange};

/// The public formatting contract used by the MCP transport adapter.
pub trait MarkdownResult {
    /// Render this typed result as compact, agent-oriented Markdown.
    fn to_markdown(&self) -> String;
}

impl MarkdownResult for SearchResult {
    /// One two-line record per hit: the address, then the declaration indented
    /// beneath it.
    ///
    /// # Why not a table
    ///
    /// A Markdown table has to escape its cells, and both fields carried here
    /// are ones that must survive **verbatim**:
    ///
    /// * the **address** is what the agent passes back, and a Rust impl
    ///   segment contains a lifetime apostrophe, which the address grammar
    ///   escapes as `\'` — [`escape_table_cell`] then doubles the backslash to
    ///   `\\'`, so an agent copying the cell gets a string that no longer
    ///   parses;
    /// * the **signature** is the declaration's primary identity, and modern
    ///   Python and TypeScript signatures are full of union pipes
    ///   (`str | bytes | None`), every one of which a cell must escape to
    ///   `\|` — so the model reads, and echoes onward, something that is not
    ///   the signature.
    ///
    /// Outside a cell neither needs escaping at all. The table chrome was also
    /// pure cost: measured over these same responses it never paid for itself
    /// at any row count, because the per-row floor is the pipes themselves.
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Search", self.hits.len());
        if let Some(status) = &self.semantic {
            // A signal, not a sentence (§6.4): only printed when semantic
            // coverage is not the default-complete case, so an ordinary
            // structural search pays nothing for it.
            writeln!(out, "{}", semantic_signal(status)).expect("String write");
        }
        if let Some(excluded) = &self.excluded_kinds {
            // Same discipline as `semantic_signal`: a signal, printed only
            // when the default scope actually narrowed something, so an
            // explicit `kinds` call pays nothing for it.
            writeln!(out, "{}", excluded_kinds_signal(excluded)).expect("String write");
        }
        if self.hits.is_empty() {
            out.push_str("(no matches)\n");
        } else {
            for hit in &self.hits {
                let identity = hit
                    .address
                    .clone()
                    .unwrap_or_else(|| SymbolKeyDto::from_wire(&hit.hit.key).0);
                let declaration = signature(&hit.hit.sig_preview);
                let declaration = if declaration.is_empty() {
                    format!("{} {}", kind_label(&hit.hit.kind), hit.hit.display_name)
                } else {
                    declaration
                };
                let marker = if hit.semantic { " [semantic]" } else { "" };
                let _ = writeln!(out, "{identity}\n  {declaration}{marker}");
                if let Some(documentation) = &hit.documentation {
                    let _ = writeln!(out, "  doc: {}", inline(documentation));
                }
            }
            out.push('\n');
        }
        pagination(&mut out, self.truncated, self.next_cursor.as_deref());
        out
    }
}

/// Render a [`SemanticStatus`] as a compact machine-readable signal rather
/// than a sentence — the `~sem:` prefix marks it as a coverage note, not a
/// row of data (docs/MCP-SURFACE-PLAN.md §6.4).
fn semantic_signal(status: &SemanticStatus) -> String {
    match status {
        SemanticStatus::Ready => "~sem:ready".to_owned(),
        SemanticStatus::Building { covered, total } => {
            format!("~sem:building({covered}/{total})")
        }
        // The remedy rides along despite this being the terse surface. A
        // coverage note that says only `NoModelConfigured` is a name for a
        // state, and the reader who needs it is by definition the one who
        // does not already know what that state implies — which is how
        // `~sem:unavailable(NoEmbedder)` sent people to diagnose a broken
        // embedder that had never been built in. Unavailability is also rare,
        // so the extra text is not a per-result cost the common case pays.
        SemanticStatus::Unavailable { reason, remedy } => {
            format!("~sem:unavailable({reason}) — {remedy}")
        }
    }
}

/// Render `SearchResult::excluded_kinds` as a compact signal, same `~`
/// convention as [`semantic_signal`]: this is coverage metadata about the
/// page, not a row of data, and it names what was held back so the reader
/// learns what to pass to `kinds` without re-deriving the kind vocabulary
/// from the schema.
fn excluded_kinds_signal(excluded: &[KindTag]) -> String {
    let names: Vec<String> = excluded.iter().map(kind_label).collect();
    format!(
        "~scope:default (excluded {}; pass `kinds` to include them)",
        names.join(", ")
    )
}

impl MarkdownResult for CompactSymbolDoc {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        append_symbol(&mut out, self, 2);
        out
    }
}

impl MarkdownResult for SymbolsResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Symbols", self.symbols.len());
        if self.symbols.is_empty() {
            out.push_str("(none)\n");
            return out;
        }

        for symbol in &self.symbols {
            out.push('\n');
            append_symbol(&mut out, symbol, 3);
        }
        out
    }
}

impl MarkdownResult for UsagesResult {
    /// One record per usage: the address (or key), then the declaration and
    /// path indented beneath it — never a table. Same reasoning as
    /// [`SearchResult`]: a table cell must escape `|` and double `\`, which
    /// corrupts both an address's escaped lifetime apostrophes and a
    /// Python/TypeScript union signature.
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Usages", self.usages.len());
        if self.usages.is_empty() {
            out.push_str("(none)\n");
        } else {
            render_usage_records(&mut out, &self.usages);
        }
        pagination(&mut out, self.truncated, self.next_cursor.as_deref());
        out
    }
}

/// Shared by [`UsagesResult`] and `RefsResult::In`.
fn render_usage_records(out: &mut String, usages: &[UsageRow]) {
    for usage in usages {
        let identity = usage.address.clone().unwrap_or_else(|| usage.key.0.clone());
        let _ = writeln!(out, "{identity}\n  {}\n  {}", usage.signature, usage.path);
    }
    out.push('\n');
}

impl MarkdownResult for PackagesResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Packages", self.packages.len());
        if self.packages.is_empty() {
            out.push_str("(none)\n");
        } else {
            // `lineage` is the canonical `ecosystem:name` value.  The name and
            // ecosystem fields are derivable from it and are not repeated.
            let rows = self
                .packages
                .iter()
                .map(|package| vec![package.lineage.clone()])
                .collect::<Vec<_>>();
            table(&mut out, &["package"], &rows);
        }
        out
    }
}

impl MarkdownResult for ListVersionsResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        write!(
            out,
            "## Versions · {}\n\npackage: {}\n\n",
            self.versions.len(),
            inline(&self.package.0)
        )
        .expect("writing to String cannot fail");
        if self.versions.is_empty() {
            out.push_str("(none)\n");
            return out;
        }

        let rows = self
            .versions
            .iter()
            .map(|version| {
                vec![
                    version.version.clone(),
                    if version.is_current {
                        "current".to_owned()
                    } else {
                        String::new()
                    },
                    version.symbol_count.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        table(&mut out, &["version", "", "symbols"], &rows);
        out
    }
}

impl MarkdownResult for LoadedPackagesResult {
    /// One heading per package (`lineage`; name/ecosystem are derivable and
    /// not repeated), with its loaded versions as a nested table — merges
    /// [`PackagesResult`]'s single-column listing and [`ListVersionsResult`]'s
    /// version table into one response.
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Packages", self.packages.len());
        if self.packages.is_empty() {
            out.push_str("(none)\n");
            return out;
        }
        for package in &self.packages {
            writeln!(out, "package: {}", inline(&package.lineage)).expect("String write");
            if package.versions.is_empty() {
                out.push_str("(no versions loaded)\n\n");
                continue;
            }
            let rows = package
                .versions
                .iter()
                .map(|version| {
                    vec![
                        version.version.clone(),
                        if version.is_current {
                            "current".to_owned()
                        } else {
                            String::new()
                        },
                        version.symbol_count.to_string(),
                    ]
                })
                .collect::<Vec<_>>();
            table(&mut out, &["version", "", "symbols"], &rows);
        }
        out
    }
}

impl MarkdownResult for SelectVersionResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        match self {
            SelectVersionResult::Switched {
                package,
                version,
                symbol_count,
            } => {
                out.push_str("## Version selected\n\n");
                writeln!(
                    out,
                    "package: {} · version: {} · symbols: {}",
                    inline(&package.0),
                    inline(version),
                    symbol_count
                )
                .expect("writing to String cannot fail");
            }
            SelectVersionResult::NotLoaded { package, version } => {
                out.push_str("## Version not loaded\n\n");
                writeln!(
                    out,
                    "package: {} · version: {}",
                    inline(&package.0),
                    inline(version)
                )
                .expect("writing to String cannot fail");
            }
        }
        out
    }
}

impl MarkdownResult for DiffVersionsResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        match self {
            DiffVersionsResult::Diff {
                diff,
                truncated,
                next_cursor,
            } => format_diff(&mut out, diff, *truncated, next_cursor.as_deref()),
            DiffVersionsResult::NotLoaded {
                package,
                from_version,
                to_version,
                loaded,
            } => {
                out.push_str("## Diff unavailable\n\n");
                writeln!(
                    out,
                    "package: {} · from: {} · to: {}",
                    inline(&package.0),
                    inline(from_version),
                    inline(to_version)
                )
                .expect("writing to String cannot fail");
                if loaded.is_empty() {
                    out.push_str("loaded: none\n");
                } else {
                    out.push_str("loaded: ");
                    for (index, version) in loaded.iter().enumerate() {
                        if index > 0 {
                            out.push_str(", ");
                        }
                        out.push_str(&inline(version));
                    }
                    out.push('\n');
                }
            }
        }
        out
    }
}

impl MarkdownResult for IndexPackageResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        match self {
            IndexPackageResult::Indexed {
                purl,
                package,
                version,
                symbol_count,
                root_key,
                integrity,
            } => {
                out.push_str("## Index complete\n\n");
                write!(
                    out,
                    "package: {} · version: {} · symbols: {}\npurl: {}\n",
                    inline(package),
                    inline(version),
                    symbol_count,
                    inline(purl)
                )
                .expect("writing to String cannot fail");
                if let Some(root_key) = root_key {
                    writeln!(out, "root: {}", inline(root_key)).expect("String write");
                }
                writeln!(
                    out,
                    "integrity: {} {} {}",
                    if integrity.verified_against_published_digest {
                        "verified"
                    } else {
                        "local"
                    },
                    escape_table_cell(&integrity.algorithm),
                    inline(&integrity.digest)
                )
                .expect("String write");
                writeln!(out, "detail: {}", inline(&integrity.detail)).expect("String write");
            }
            IndexPackageResult::Running {
                purl,
                stage,
                received_bytes,
                total_bytes,
                elapsed_seconds,
                joined_existing_job,
            } => {
                out.push_str("## Index running\n\n");
                writeln!(
                    out,
                    "purl: {} · stage: {} · elapsed: {}s",
                    inline(purl),
                    inline(stage),
                    elapsed_seconds
                )
                .expect("writing to String cannot fail");
                if let Some(progress) = byte_progress(*received_bytes, *total_bytes) {
                    writeln!(out, "download: {progress}").expect("String write");
                }
                if *joined_existing_job {
                    out.push_str("attached: existing job\n");
                }
            }
        }
        out
    }
}

impl MarkdownResult for IndexResult {
    /// One record per line, never a table.
    ///
    /// A target is a PURL or a filesystem path, and both routinely contain the
    /// characters a Markdown table cell has to escape. A reader — human or
    /// model — that copies a mangled path back into the next `index` call gets
    /// a failure about a target it never wrote.
    fn to_markdown(&self) -> String {
        let mut out = String::new();

        if !self.indexed.is_empty() {
            writeln!(out, "## Indexed ({})\n", self.indexed.len()).expect("String write");
            for package in &self.indexed {
                writeln!(
                    out,
                    "{} · version: {} · symbols: {}{}",
                    inline(&package.package),
                    inline(&package.version),
                    package.symbol_count,
                    if package.requested {
                        ""
                    } else {
                        " · pulled in as a dependency"
                    },
                )
                .expect("String write");
                writeln!(out, "  from: {}", inline(&package.target)).expect("String write");
                if let Some(root_key) = &package.root_key {
                    writeln!(out, "  root: {}", inline(root_key)).expect("String write");
                }
                writeln!(out, "  integrity: {}", inline(&package.integrity.detail))
                    .expect("String write");
            }
            out.push('\n');
        }

        if !self.running.is_empty() {
            writeln!(out, "## Still running ({})\n", self.running.len()).expect("String write");
            out.push_str(
                "Not a failure. Call `index` again with these targets to attach to the \
                 same jobs.\n\n",
            );
            for job in &self.running {
                writeln!(
                    out,
                    "{} · stage: {}{}",
                    inline(&job.target),
                    inline(&job.stage),
                    if job.joined_existing_job {
                        " · attached to an existing job"
                    } else {
                        ""
                    },
                )
                .expect("String write");
            }
            out.push('\n');
        }

        if !self.failed.is_empty() {
            writeln!(out, "## Failed ({})\n", self.failed.len()).expect("String write");
            for failure in &self.failed {
                writeln!(out, "{}", inline(&failure.target)).expect("String write");
                writeln!(out, "  {}", inline(&failure.error)).expect("String write");
            }
            out.push('\n');
        }

        if !self.not_scanned.is_empty() {
            out.push_str("## Dependencies not followed\n\n");
            // Spelled out because the whole reason this section exists is that
            // its absence reads as "there were none".
            out.push_str(
                "These are declarations that were NOT read — not packages known to have \
                 no dependencies.\n\n",
            );
            for note in &self.not_scanned {
                writeln!(out, "{}", inline(&note.target)).expect("String write");
                writeln!(out, "  {}", inline(&note.reason)).expect("String write");
            }
            out.push('\n');
        }

        if out.is_empty() {
            out.push_str("## Index\n\n(nothing to report)\n");
        }
        out
    }
}

/// Render an [`EdgeCoverageNote`] as a compact machine-readable signal, the
/// same `~`-prefixed convention [`semantic_signal`]/[`coverage_signal`] use:
/// this is metadata about why the page is empty, not a row of query data, and
/// it must not be mistaken for one by whatever reads the Markdown.
fn edge_coverage_signal(note: &EdgeCoverageNote) -> String {
    format!(
        "~graph:edge_empty({} → try {}) — {}",
        note.edge, note.answers_instead, note.reason
    )
}

impl MarkdownResult for QueryResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Query", self.rows.len());
        if let Some(note) = &self.edge_coverage {
            // A signal, not a sentence (§6.4): only printed on the empty page
            // it explains, so a query that got rows pays nothing for it.
            writeln!(out, "{}", edge_coverage_signal(note)).expect("String write");
        }
        if self.rows.is_empty() {
            out.push_str("(no rows)\n");
        } else {
            let has_signature = self.columns.iter().any(|column| column == "signature");
            let width = self.columns.len().max(
                self.rows
                    .iter()
                    .map(|row| row.cells.len())
                    .max()
                    .unwrap_or(0),
            );
            if has_signature {
                render_signature_query(&mut out, &self.columns, &self.rows, width);
            } else {
                let headers = (0..width)
                    .map(|index| {
                        let name = self.columns.get(index).map_or("", String::as_str);
                        if name.is_empty() {
                            format!("#{index_plus_one}", index_plus_one = index + 1)
                        } else {
                            name.to_owned()
                        }
                    })
                    .collect::<Vec<_>>();
                let rows = self
                    .rows
                    .iter()
                    .map(|row| {
                        (0..width)
                            .map(|index| row.cells.get(index).cloned().unwrap_or_default())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                if self.columns.iter().any(|column| column == "name")
                    && self.columns.iter().any(|column| column == "kind")
                {
                    out.push_str(
                        "note: symbol rows omit `signature`; request it for type-complete declarations.\n\n",
                    );
                }
                table_owned(&mut out, &headers, &rows);
            }
        }
        pagination(&mut out, self.truncated, self.next_cursor.as_deref());
        out
    }
}

impl MarkdownResult for SchemaResult {
    fn to_markdown(&self) -> String {
        if self.full {
            let mut out = String::from("## GraphQL schema (full SDL)\n\n");
            fenced(&mut out, Some("graphql"), &self.schema);
            out
        } else {
            // The card is itself Markdown (prose plus embedded ```graphql
            // fences for the worked examples), not a single GraphQL
            // document, so it is passed through unfenced rather than
            // wrapped in a fence whose language tag would be wrong.
            let mut out = String::from(
                "## GraphQL schema (compact reference card — call again with \
                 `full: true` for the complete SDL)\n\n",
            );
            out.push_str(&self.schema);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    }
}

impl MarkdownResult for GetOccurrencesResult {
    fn to_markdown(&self) -> String {
        render_occurrences_markdown(&self.occurrences, self.next_cursor.as_deref())
    }
}

/// Shared by [`GetOccurrencesResult`] and `RefsResult::Out`. Owner/target
/// identity headings prefer the address (§4) over the bare key when one was
/// rendered — headings use backtick code spans, not table cells, so an
/// address's escaped apostrophes survive intact (unlike a table cell; see
/// [`UsagesResult`]'s own doc comment for why that distinction matters).
fn render_occurrences_markdown(
    occurrences: &[crate::mcp::tools::OccurrenceRow],
    next_cursor: Option<&str>,
) -> String {
    let rows = occurrences
        .iter()
        .map(|row| crate::mcp::occurrence_format::OccurrenceRow {
            target_key: row
                .target
                .as_ref()
                .and_then(|target| target.address.clone())
                .unwrap_or_else(|| row.target_key.clone()),
            target: row.target.as_ref().map(|target| {
                crate::mcp::occurrence_format::SymbolSummary {
                    signature: (!target.signature.is_empty()).then(|| target.signature.clone()),
                    name: None,
                    kind: None,
                    path: target.path.clone(),
                }
            }),
            owner: crate::mcp::occurrence_format::OccurrenceOwner {
                key: row
                    .owner
                    .address
                    .clone()
                    .unwrap_or_else(|| row.owner.key.0.clone()),
                summary: crate::mcp::occurrence_format::SymbolSummary {
                    signature: (!row.owner.signature.is_empty())
                        .then(|| row.owner.signature.clone()),
                    name: None,
                    kind: None,
                    path: row.owner.path.clone(),
                },
            },
            reference_kind: row.reference_kind.clone(),
            confidence: row.confidence.clone(),
            span_start: i64::from(row.span_start),
            span_end: i64::from(row.span_end),
        })
        .collect::<Vec<_>>();
    crate::mcp::occurrence_format::render_occurrences(
        &rows,
        &crate::mcp::occurrence_format::OccurrenceStatus::Complete,
        next_cursor,
    )
}

impl MarkdownResult for RefsResult {
    fn to_markdown(&self) -> String {
        match self {
            RefsResult::In {
                usages,
                truncated,
                next_cursor,
                coverage,
            } => {
                let mut out = String::new();
                heading(&mut out, "Refs (in)", usages.len());
                if let Some(coverage) = coverage {
                    // Same reasoning as `semantic_signal` below: a signal
                    // line, not a sentence, printed only when the page is not
                    // authoritative — the ordinary, fully-recorded case pays
                    // nothing for it.
                    writeln!(out, "{}", coverage_signal(coverage)).expect("String write");
                }
                if usages.is_empty() {
                    out.push_str("(none)\n");
                } else {
                    render_usage_records(&mut out, usages);
                }
                pagination(&mut out, *truncated, next_cursor.as_deref());
                out
            }
            RefsResult::Out {
                occurrences,
                next_cursor,
                coverage,
                ..
            } => {
                let mut out = String::new();
                if let Some(coverage) = coverage {
                    writeln!(out, "{}", coverage_signal(coverage)).expect("String write");
                }
                out.push_str(&render_occurrences_markdown(
                    occurrences,
                    next_cursor.as_deref(),
                ));
                out
            }
        }
    }
}

/// Render a [`ReferenceCoverage`] as a compact machine-readable signal, the
/// same `~`-prefixed convention [`semantic_signal`] uses for exactly the same
/// reason: this is metadata about whether the page can be trusted, not a row
/// of reference data, and the prefix keeps an agent from confusing the two.
///
/// `Recorded` is never actually reached through `do_refs` — `RefsResult`'s
/// `coverage` field is `None` in that case (see the field's own doc comment)
/// — but the match stays exhaustive rather than assuming callers always go
/// through that collapse.
fn coverage_signal(coverage: &ReferenceCoverage) -> String {
    match coverage {
        ReferenceCoverage::Recorded => "~refs:recorded".to_owned(),
        ReferenceCoverage::NotRecorded { language } => {
            format!("~refs:not_recorded({language})")
        }
    }
}

impl MarkdownResult for SemanticSearchResult {
    fn to_markdown(&self) -> String {
        let status = match &self.status {
            SemanticStatus::Ready => crate::mcp::semantic_format::SemanticStatus::Ready,
            SemanticStatus::Building { covered, total } => {
                crate::mcp::semantic_format::SemanticStatus::Building {
                    covered: *covered,
                    total: *total,
                }
            }
            // `remedy` is deliberately not read here: the mirror enum below
            // derives its own remedy text (`semantic_format::unavailable_remedy`),
            // and that is the copy the rendering guard test pins. Threading
            // this one through as well would give the same sentence two
            // sources within a single call.
            SemanticStatus::Unavailable { reason, .. } => {
                // "NoRuntime" can no longer arrive here — the engine's
                // `Unavailable::NoRuntime` variant is gone along with the
                // `onnx` cargo feature it named the absence of — but a
                // catch-all still degrades to `ModelFailed` rather than
                // panicking, the same posture this match already took for any
                // future engine reason it does not yet know the name of.
                let reason = match reason.as_str() {
                    "NoModelConfigured" => {
                        crate::mcp::semantic_format::SemanticUnavailable::NoModelConfigured
                    }
                    "EmptyCorpus" => crate::mcp::semantic_format::SemanticUnavailable::EmptyCorpus,
                    _ => crate::mcp::semantic_format::SemanticUnavailable::ModelFailed,
                };
                crate::mcp::semantic_format::SemanticStatus::Unavailable(reason)
            }
        };
        let hits = self
            .hits
            .iter()
            .map(|hit| {
                let semantic_hit = crate::mcp::semantic_format::SemanticHit::new(
                    hit.key.0.clone(),
                    hit.display_name.clone(),
                    kind_label(&hit.kind),
                    hit.signature.clone(),
                )
                .with_score(hit.score);
                match hit.documentation.as_deref() {
                    Some(documentation) => semantic_hit.with_evidence(
                        crate::mcp::semantic_format::DocumentationEvidence::from_documentation(
                            documentation,
                            1_200,
                        ),
                    ),
                    None => semantic_hit,
                }
            })
            .collect();
        let mut result =
            crate::mcp::semantic_format::SemanticResult::new(self.query.clone(), status, hits);
        if self.truncated {
            result = result.with_facets([crate::mcp::semantic_format::SemanticFacet::new(
                "next",
                self.next_cursor.clone().unwrap_or_default(),
            )]);
        }
        crate::mcp::semantic_format::render_semantic_markdown(&result)
    }
}

fn heading(out: &mut String, label: &str, count: usize) {
    writeln!(out, "## {label} · {count}\n").expect("String write");
}

fn append_symbol(out: &mut String, symbol: &CompactSymbolDoc, level: u8) {
    let hashes = "#".repeat(level as usize);
    writeln!(out, "{hashes} {}\n", inline(&symbol.path)).expect("String write");
    writeln!(out, "key: {}", inline(&symbol.key.0)).expect("String write");
    if let Some(location) = &symbol.location {
        writeln!(out, "location: {}", inline(location)).expect("String write");
    }
    if let Some(deprecation) = &symbol.deprecation {
        writeln!(out, "deprecated: {}", inline(deprecation)).expect("String write");
    }
    out.push('\n');
    if let Some(source) = &symbol.source {
        fenced(out, None, source);
    } else if let Some(signature) = &symbol.signature {
        fenced(out, None, signature);
    } else {
        out.push_str("(declaration unavailable)\n");
    }
    if !symbol.references.is_empty() {
        out.push_str("\nreferences:\n");
        let rows = symbol
            .references
            .iter()
            .map(|reference| vec![reference.text.clone(), reference.target.0.clone()])
            .collect::<Vec<_>>();
        table(out, &["type", "target"], &rows);
    }
}

fn format_diff(out: &mut String, diff: &PackageDiff, truncated: bool, cursor: Option<&str>) {
    writeln!(
        out,
        "## Diff · {} · {} → {}\n",
        inline(&package_lineage(&diff.package)),
        inline(&diff.from_version),
        inline(&diff.to_version)
    )
    .expect("String write");
    writeln!(
        out,
        "summary: {} changed · {} unchanged\n",
        diff.rows.len(),
        diff.unchanged
    )
    .expect("String write");
    if diff.rows.is_empty() {
        out.push_str("(no changes)\n");
    } else {
        let rows = diff
            .rows
            .iter()
            .map(|row| {
                let (status, detail) = diff_verdict(&row.verdict);
                let declaration = if row.signature.is_empty() {
                    format!("{} {}", row.kind, row.path)
                } else {
                    row.signature.to_string()
                };
                vec![
                    declaration,
                    row.path.to_string(),
                    status,
                    detail,
                    SymbolKeyDto::from_wire(&row.key).0,
                ]
            })
            .collect::<Vec<_>>();
        table(
            out,
            &["declaration", "path", "change", "detail", "key"],
            &rows,
        );
    }
    pagination(out, truncated, cursor);
}

fn package_lineage(package: &crate::wire::PackageLineageId) -> String {
    format!("{}:{}", package.ecosystem.as_str(), package.name.as_str())
}

fn diff_verdict(verdict: &DiffVerdict) -> (String, String) {
    match verdict {
        DiffVerdict::Added => ("added".to_owned(), String::new()),
        DiffVerdict::Removed => ("removed".to_owned(), String::new()),
        DiffVerdict::Rekeyed {
            from_key,
            to_key,
            from_tier,
            to_tier,
        } => (
            "rekeyed".to_owned(),
            format!(
                "{} ({}) → {} ({})",
                SymbolKeyDto::from_wire(from_key).0,
                key_tier_label(*from_tier),
                SymbolKeyDto::from_wire(to_key).0,
                key_tier_label(*to_tier)
            ),
        ),
        DiffVerdict::Changed { change } => ("changed".to_owned(), timeline_change(change)),
        DiffVerdict::Indeterminate { tier } => (
            "indeterminate".to_owned(),
            format!("tier: {}", key_tier_label(*tier)),
        ),
    }
}

fn timeline_change(change: &TimelineChange) -> String {
    match change {
        TimelineChange::Present => "present".to_owned(),
        TimelineChange::Introduced => "introduced".to_owned(),
        TimelineChange::Renamed { from } => format!("renamed from {from}"),
        TimelineChange::Deprecated => "deprecated".to_owned(),
        TimelineChange::Undeprecated => "undeprecated".to_owned(),
        TimelineChange::SignatureChanged => "signature".to_owned(),
        TimelineChange::VisibilityChanged => "visibility".to_owned(),
        TimelineChange::DocsChanged => "docs".to_owned(),
        TimelineChange::Unchanged => "unchanged".to_owned(),
        TimelineChange::Removed => "removed".to_owned(),
    }
}

fn table(out: &mut String, headers: &[&str], rows: &[Vec<String>]) {
    let dynamic_headers = headers
        .iter()
        .map(|header| (*header).to_owned())
        .collect::<Vec<_>>();
    table_owned(out, &dynamic_headers, rows);
}

/// Render graph rows that carry a declaration signature as source-shaped
/// records. The signature is the code; every other selected field remains
/// immediately above it as a comment, with `key` first when present. This
/// keeps graph queries lossless without turning a declaration into a wall of
/// derivable columns. Queries without a signature remain relationship tables.
fn render_signature_query(
    out: &mut String,
    columns: &[String],
    rows: &[QueryResultRow],
    width: usize,
) {
    let signature_index = columns.iter().position(|column| column == "signature");
    let mut comment_indices = (0..width)
        .filter(|index| Some(*index) != signature_index)
        .collect::<Vec<_>>();
    if let Some(key_index) = columns.iter().position(|column| column == "key")
        && let Some(position) = comment_indices.iter().position(|index| *index == key_index) {
            comment_indices.remove(position);
            comment_indices.insert(0, key_index);
        }

    for row in rows {
        let mut body = String::new();
        for index in &comment_indices {
            let name = columns
                .get(*index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("#{index_plus_one}", index_plus_one = *index + 1));
            let value = row.cells.get(*index).map_or("", String::as_str);
            push_query_comment(&mut body, &name, value);
        }

        let signature = signature_index
            .and_then(|index| row.cells.get(index))
            .map_or("", String::as_str);
        if signature.is_empty() {
            body.push_str("// declaration: unavailable\n");
        } else {
            body.push_str(signature);
            if !signature.ends_with('\n') {
                body.push('\n');
            }
        }
        fenced(out, Some("text"), &body);
        out.push('\n');
    }
}

fn push_query_comment(out: &mut String, name: &str, value: &str) {
    let label = escape_table_cell(name);
    let value = value.replace('\r', "␍");
    let mut lines = value.split('\n');
    let first = lines.next().unwrap_or("");
    writeln!(out, "// {label}: {first}").expect("String write");
    for line in lines {
        writeln!(out, "// {line}").expect("String write");
    }
}

fn table_owned(out: &mut String, headers: &[String], rows: &[Vec<String>]) {
    out.push('|');
    for header in headers {
        write!(out, " {} |", escape_table_cell(header)).expect("String write");
    }
    out.push('\n');
    out.push('|');
    for _ in headers {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in rows {
        out.push('|');
        for index in 0..headers.len() {
            let value = row.get(index).map_or("", String::as_str);
            write!(out, " {} |", escape_table_cell(value)).expect("String write");
        }
        out.push('\n');
    }
    out.push('\n');
}

fn pagination(out: &mut String, truncated: bool, cursor: Option<&str>) {
    match cursor {
        Some(cursor) => writeln!(out, "next: {}", inline(cursor)).expect("String write"),
        None if truncated => out.push_str("next: unavailable (truncated without cursor)\n"),
        None => {}
    }
}

fn fenced(out: &mut String, language: Option<&str>, text: &str) {
    let fence_len = longest_backtick_run(text).saturating_add(1).max(3);
    let fence = "`".repeat(fence_len);
    out.push_str(&fence);
    if let Some(language) = language.filter(|language| safe_fence_language(language)) {
        out.push_str(language);
    }
    out.push('\n');
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn safe_fence_language(language: &str) -> bool {
    !language.is_empty()
        && language.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '-' | '+' | '#' | '.' | ':')
        })
}

/// Render an untrusted one-line value as a robust Markdown code span.
fn inline(value: &str) -> String {
    let value = value.replace('\r', "␍").replace('\n', "␊");
    let fence_len = longest_backtick_run(&value).saturating_add(1).max(1);
    let fence = "`".repeat(fence_len);
    if value.starts_with(' ') || value.ends_with(' ') {
        format!("{fence} {value} {fence}")
    } else {
        format!("{fence}{value}{fence}")
    }
}

fn escape_table_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '|' => escaped.push_str("\\|"),
            '\r' | '\n' => escaped.push('↵'),
            character if character.is_control() && character != '\t' => {
                write!(escaped, "\\u{{{:x}}}", character as u32).expect("String write");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn signature(tokens: &[SigToken]) -> String {
    tokens.iter().map(signature_token).collect()
}

fn signature_token(token: &SigToken) -> &str {
    match token {
        SigToken::Kw(text) | SigToken::Punct(text) => text,
        SigToken::Ident(text)
        | SigToken::Ty { text, .. }
        | SigToken::Generic(text)
        | SigToken::Lifetime(text) => text,
        SigToken::Ws => " ",
    }
}

fn kind_label(kind: &KindTag) -> String {
    match kind {
        KindTag::Known(kind) => format!("{kind:?}"),
        KindTag::Unknown(raw) => format!("unknown:{raw}"),
    }
}

fn key_tier_label(tier: KeyTierLabel) -> &'static str {
    match tier {
        KeyTierLabel::Structural => "structural",
        KeyTierLabel::Span => "span",
        KeyTierLabel::Ordinal => "ordinal",
        KeyTierLabel::Unrecorded => "unrecorded",
    }
}

fn byte_progress(received: Option<u64>, total: Option<u64>) -> Option<String> {
    match (received, total) {
        (Some(received), Some(total)) => Some(format!("{received}/{total} B")),
        (Some(received), None) => Some(format!("{received} B")),
        (None, Some(total)) => Some(format!("?/{total} B")),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::key::PackageLineageDto;
    use crate::mcp::tools::{QueryResultRow, SearchHitDoc};
    use crate::wire::{
        DiffRow, EcosystemId, GenerationId, IntroId, PackageLineageId, PackageName, Provenance,
        SharedStr, SymbolKey, Visibility,
    };
    use std::sync::Arc;

    fn key_dto() -> SymbolKeyDto {
        SymbolKeyDto(
            "cargo:demo#1111111111111111111111111111111111111111111111111111111111111111".into(),
        )
    }

    fn wire_key(byte: u8) -> SymbolKey {
        SymbolKey::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo")),
            IntroId::from_raw([byte; 32]),
        )
    }

    fn symbol(source: Option<&str>) -> CompactSymbolDoc {
        CompactSymbolDoc {
            key: key_dto(),
            address: None,
            path: "demo::Thing".into(),
            signature: Some("pub struct Thing<T>".into()),
            kind: KindTag::Unknown(777),
            visibility: Visibility::Public,
            references: vec![],
            location: Some("src/lib.rs:4:1".into()),
            source: source.map(str::to_owned),
            deprecation: None,
        }
    }

    fn integrity(verified: bool) -> crate::mcp::tools::IntegrityReport {
        crate::mcp::tools::IntegrityReport {
            verified_against_published_digest: verified,
            algorithm: "sha256".into(),
            digest: "deadbeef".into(),
            detail: "published by registry".into(),
        }
    }

    #[test]
    fn empty_results_are_short_and_well_formed() {
        assert_eq!(
            SearchResult {
                hits: vec![],
                semantic: None,
                excluded_kinds: None,
                truncated: false,
                next_cursor: None
            }
            .to_markdown(),
            "## Search · 0\n\n(no matches)\n"
        );
        assert!(
            UsagesResult {
                usages: vec![],
                truncated: false,
                next_cursor: None
            }
            .to_markdown()
            .contains("(none)")
        );
        assert!(
            PackagesResult { packages: vec![] }
                .to_markdown()
                .contains("(none)")
        );
        assert!(
            QueryResult {
                columns: vec![],
                rows: vec![],
                truncated: false,
                next_cursor: None,
                edge_coverage: None,
            }
            .to_markdown()
            .contains("(no rows)")
        );
        assert!(
            SymbolsResult { symbols: vec![] }
                .to_markdown()
                .contains("(none)")
        );
    }

    #[test]
    fn pagination_uses_cursor_and_omits_false_booleans() {
        let result = UsagesResult {
            usages: vec![],
            truncated: true,
            next_cursor: Some("50|next".into()),
        };
        let rendered = result.to_markdown();
        assert!(rendered.contains("next: `50|next`"));
        assert!(!rendered.contains("truncated"));

        let malformed = UsagesResult {
            usages: vec![],
            truncated: true,
            next_cursor: None,
        };
        assert!(malformed.to_markdown().contains("truncated without cursor"));
    }

    #[test]
    fn table_values_cannot_break_rows_or_inject_headings() {
        let result = QueryResult {
            columns: vec!["column|\n#".into(), "other".into()],
            rows: vec![QueryResultRow {
                cells: vec!["value|\n### injected\n\\slash".into(), "`ticks`".into()],
            }],
            truncated: false,
            next_cursor: None,
            edge_coverage: None,
        };
        let rendered = result.to_markdown();
        assert!(rendered.contains("column\\|↵#"));
        assert!(rendered.contains("value\\|↵### injected↵\\\\slash"));
        assert!(!rendered.contains("\n### injected"));
    }

    #[test]
    fn signature_queries_keep_selected_fields_as_comments_above_code() {
        let result = QueryResult {
            columns: vec![
                "key".into(),
                "signature".into(),
                "kind".into(),
                "name".into(),
                "path".into(),
            ],
            rows: vec![QueryResultRow {
                cells: vec![
                    "fixture:demo#key".into(),
                    "pub struct Point".into(),
                    "Record".into(),
                    "Point".into(),
                    "demo::Point".into(),
                ],
            }],
            truncated: false,
            next_cursor: None,
            edge_coverage: None,
        };
        let rendered = result.to_markdown();
        assert!(rendered.contains("```text"));
        assert!(rendered.contains("// key: fixture:demo#key"));
        assert!(rendered.contains("// kind: Record"));
        assert!(rendered.contains("// name: Point"));
        assert!(rendered.contains("// path: demo::Point"));
        assert!(rendered.contains("\npub struct Point\n"));
        assert!(!rendered.contains("| declaration |"));
        assert!(!rendered.contains("| kind |"));
    }

    #[test]
    fn exact_source_uses_a_longer_fence_and_preserves_bytes() {
        let source = "pub fn x() {\n    let text = ` ``` `;\n}\n";
        let rendered = symbol(Some(source)).to_markdown();
        assert!(rendered.contains("pub fn x() {\n    let text = ` ``` `;\n}\n"));
        assert!(
            rendered.contains("````"),
            "the fence must exceed the source run: {rendered}"
        );
        assert!(
            !rendered.contains("signature:"),
            "source makes signature derivable"
        );
    }

    #[test]
    fn long_source_and_schema_are_not_truncated() {
        let source = "x".repeat(100_000);
        let rendered_source = symbol(Some(&source)).to_markdown();
        assert!(rendered_source.contains(&source));

        let schema = "type X { field: String }\n".repeat(20_000);
        let rendered_schema = SchemaResult {
            schema: schema.clone(),
            full: true,
        }
        .to_markdown();
        assert!(rendered_schema.contains(&schema));
        assert_eq!(rendered_schema.matches("type X").count(), 20_000);
    }

    #[test]
    fn status_variants_keep_the_actionable_difference() {
        let selected = SelectVersionResult::Switched {
            package: PackageLineageDto("cargo:demo".into()),
            version: "1.2.3".into(),
            symbol_count: 42,
        }
        .to_markdown();
        assert!(selected.contains("Version selected"));
        assert!(selected.contains("symbols: 42"));

        let missing = SelectVersionResult::NotLoaded {
            package: PackageLineageDto("cargo:demo".into()),
            version: "9.9.9".into(),
        }
        .to_markdown();
        assert!(missing.contains("Version not loaded"));

        let running = IndexPackageResult::Running {
            purl: "pkg:cargo/demo@1.0.0".into(),
            stage: "downloading".into(),
            received_bytes: Some(4),
            total_bytes: Some(10),
            elapsed_seconds: 3,
            joined_existing_job: true,
        }
        .to_markdown();
        assert!(running.contains("4/10 B"));
        assert!(running.contains("attached: existing job"));

        let indexed = IndexPackageResult::Indexed {
            purl: "pkg:cargo/demo@1.0.0".into(),
            package: "cargo:demo".into(),
            version: "1.0.0".into(),
            symbol_count: 4,
            root_key: Some(key_dto().0),
            integrity: integrity(true),
        }
        .to_markdown();
        assert!(indexed.contains("integrity: verified"));
    }

    #[test]
    fn query_handles_misaligned_rows_without_dropping_cells() {
        let result = QueryResult {
            columns: vec!["a".into()],
            rows: vec![QueryResultRow {
                cells: vec!["1".into(), "extra".into()],
            }],
            truncated: false,
            next_cursor: None,
            edge_coverage: None,
        };
        let rendered = result.to_markdown();
        assert!(rendered.contains("#2"));
        assert!(rendered.contains("extra"));
    }

    #[test]
    fn search_and_diff_keep_navigable_keys() {
        let search = SearchResult {
            hits: vec![SearchHitDoc {
                hit: crate::wire::HitRow {
                    key: wire_key(7),
                    display_name: SharedStr::from("demo::Thing"),
                    sig_preview: vec![
                        SigToken::Kw("pub"),
                        SigToken::Ws,
                        SigToken::Ident(SharedStr::from("Thing")),
                    ],
                    kind: KindTag::Unknown(44),
                    provenance: Provenance::SyncedLocal {
                        generation: GenerationId(2),
                    },
                    score: 0.5,
                },
                address: Some("cargo:demo::Thing[struct]".to_owned()),
                semantic: false,
                documentation: None,
            }],
            semantic: None,
            excluded_kinds: None,
            truncated: false,
            next_cursor: None,
        };
        let rendered = search.to_markdown();
        // The renderer prefers the address over the raw key as the
        // navigable identity when one was rendered (§4) — pre-existing
        // behaviour this test's assertion had drifted out of sync with.
        assert!(rendered.contains("cargo:demo::Thing[struct]"));
        assert!(rendered.contains("pub Thing"));

        let diff = DiffVersionsResult::Diff {
            diff: Box::new(PackageDiff {
                package: PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo")),
                from_version: SharedStr::from("1.0"),
                to_version: SharedStr::from("2.0"),
                rows: Arc::from(vec![DiffRow {
                    key: wire_key(8),
                    path: SharedStr::from("demo::Thing"),
                    name: SharedStr::from("Thing"),
                    kind: SharedStr::from("Record"),
                    signature: SharedStr::from("pub struct Thing<T>"),
                    verdict: DiffVerdict::Removed,
                }]),
                unchanged: 1,
                from_symbol_count: 2,
                to_symbol_count: 1,
            }),
            truncated: false,
            next_cursor: None,
        };
        let rendered = diff.to_markdown();
        assert!(rendered.contains("removed"));
        assert!(rendered.contains("| declaration |"));
        assert!(rendered.contains("pub struct Thing<T>"));
        assert!(!rendered.contains("| name |"));
        assert!(!rendered.contains("| kind |"));
        assert!(rendered.contains("cargo:demo#080808"));
    }

    #[test]
    fn source_without_newline_gets_only_a_structural_closing_line() {
        let rendered = symbol(Some("fn main() {}")).to_markdown();
        assert!(rendered.contains("\nfn main() {}\n```"));
        assert_eq!(rendered.matches("fn main() {}").count(), 1);
    }

    #[test]
    fn package_and_version_renderers_omit_derivable_false_markers() {
        let packages = PackagesResult {
            packages: vec![crate::mcp::tools::PackageSummary {
                lineage: "cargo:demo".into(),
                name: "demo".into(),
                ecosystem: "cargo".into(),
            }],
        }
        .to_markdown();
        assert!(packages.contains("| package |"));
        assert!(!packages.contains("ecosystem"));

        let versions = ListVersionsResult {
            package: PackageLineageDto("cargo:demo".into()),
            versions: vec![
                crate::mcp::tools::VersionSummary {
                    version: "2.0".into(),
                    is_current: true,
                    symbol_count: 2,
                },
                crate::mcp::tools::VersionSummary {
                    version: "1.0".into(),
                    is_current: false,
                    symbol_count: 1,
                },
            ],
        }
        .to_markdown();
        assert!(versions.contains("current"));
        assert_eq!(versions.matches("current").count(), 1);
        assert!(!versions.contains("is_current"));
    }

    #[test]
    fn semantic_result_carries_exact_documentation_evidence() {
        let rendered = SemanticSearchResult {
            query: "retry failed requests".into(),
            status: SemanticStatus::Ready,
            hits: vec![crate::mcp::tools::SemanticHitRow {
                key: key_dto(),
                display_name: "retry_with_backoff".into(),
                kind: KindTag::Unknown(44),
                signature: "fn retry_with_backoff(request: Request) -> Result<Response>".into(),
                score: 0.93,
                documentation: Some("Retries a failed request.\nUse `backoff` carefully.".into()),
            }],
            truncated: false,
            next_cursor: None,
        }
        .to_markdown();

        assert!(rendered.contains("status: ready · 1 match"));
        assert!(rendered.contains(
            "| 1 | `fn retry_with_backoff(request: Request) -> Result<Response>` | `cargo:demo#1111111111111111111111111111111111111111111111111111111111111111` | exact |"
        ));
        assert!(rendered.contains("| exact |"));
        assert!(rendered.contains("Retries a failed request.\nUse `backoff` carefully."));
        assert!(!rendered.contains("score"), "scores stay opt-in");
    }
}
