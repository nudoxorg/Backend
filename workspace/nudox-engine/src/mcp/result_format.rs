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
    CompactSymbolDoc, DiffVersionsResult, GetOccurrencesResult, IndexPackageResult,
    ListVersionsResult, PackagesResult, QueryResult, QueryResultRow, SchemaResult, SearchResult,
    SelectVersionResult, SemanticSearchResult, SemanticStatus, SymbolsResult, UsagesResult,
};
use crate::wire::{
    DiffVerdict, HitRow, KeyTierLabel, KindTag, PackageDiff, SigToken, TimelineChange,
};

/// The public formatting contract used by the MCP transport adapter.
pub trait MarkdownResult {
    /// Render this typed result as compact, agent-oriented Markdown.
    fn to_markdown(&self) -> String;
}

/// Format a [`SearchResult`] as a ranked table.
pub fn format_search_result(result: &SearchResult) -> String {
    result.to_markdown()
}

/// Format one compact symbol, preserving its exact source when present.
pub fn format_compact_symbol_doc(result: &CompactSymbolDoc) -> String {
    result.to_markdown()
}

/// Format a batch of compact symbols.
pub fn format_symbols_result(result: &SymbolsResult) -> String {
    result.to_markdown()
}

/// Format a [`UsagesResult`] as a navigable symbol table.
pub fn format_usages_result(result: &UsagesResult) -> String {
    result.to_markdown()
}

/// Format a loaded-package listing.
pub fn format_packages_result(result: &PackagesResult) -> String {
    result.to_markdown()
}

/// Format the loaded versions of one package.
pub fn format_list_versions_result(result: &ListVersionsResult) -> String {
    result.to_markdown()
}

/// Format a version-selection outcome.
pub fn format_select_version_result(result: &SelectVersionResult) -> String {
    result.to_markdown()
}

/// Format a package diff, including rekey evidence and uncertainty.
pub fn format_diff_versions_result(result: &DiffVersionsResult) -> String {
    result.to_markdown()
}

/// Format an indexing job's current status.
pub fn format_index_package_result(result: &IndexPackageResult) -> String {
    result.to_markdown()
}

/// Format arbitrary graph-query columns and rows as a safe table.
pub fn format_query_result(result: &QueryResult) -> String {
    result.to_markdown()
}

/// Format the GraphQL schema without altering its bytes.
pub fn format_schema_result(result: &SchemaResult) -> String {
    result.to_markdown()
}

/// Format exact owner-relative occurrence rows.
pub fn format_occurrences_result(result: &GetOccurrencesResult) -> String {
    result.to_markdown()
}

/// Format a semantic-search result while preserving its coverage state.
pub fn format_semantic_search_result(result: &SemanticSearchResult) -> String {
    result.to_markdown()
}

impl MarkdownResult for SearchResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Search", self.hits.len());
        if self.hits.is_empty() {
            out.push_str("(no matches)\n");
        } else {
            let rows = self
                .hits
                .iter()
                .enumerate()
                .map(|(index, hit)| search_row(index + 1, hit))
                .collect::<Vec<_>>();
            table(&mut out, &["#", "declaration", "key"], &rows);
        }
        pagination(&mut out, self.truncated, self.next_cursor.as_deref());
        out
    }
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
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Usages", self.usages.len());
        if self.usages.is_empty() {
            out.push_str("(none)\n");
        } else {
            let rows = self
                .usages
                .iter()
                .map(|usage| vec![usage.signature.clone(), usage.path.clone(), usage.key.0.clone()])
                .collect::<Vec<_>>();
            table(&mut out, &["declaration", "path", "key"], &rows);
        }
        pagination(&mut out, self.truncated, self.next_cursor.as_deref());
        out
    }
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
                write!(
                    out,
                    "package: {} · version: {} · symbols: {}\n",
                    inline(&package.0),
                    inline(version),
                    symbol_count
                )
                .expect("writing to String cannot fail");
            }
            SelectVersionResult::NotLoaded { package, version } => {
                out.push_str("## Version not loaded\n\n");
                write!(
                    out,
                    "package: {} · version: {}\n",
                    inline(&package.0),
                    inline(version)
                )
                .expect("writing to String cannot fail");
            }
            _ => out.push_str("## Version status\n\n(unrecognised status)\n"),
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
                write!(
                    out,
                    "package: {} · from: {} · to: {}\n",
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
            _ => out.push_str("## Diff status\n\n(unrecognised status)\n"),
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
                write!(
                    out,
                    "purl: {} · stage: {} · elapsed: {}s\n",
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
            _ => out.push_str("## Index status\n\n(unrecognised status)\n"),
        }
        out
    }
}

impl MarkdownResult for QueryResult {
    fn to_markdown(&self) -> String {
        let mut out = String::new();
        heading(&mut out, "Query", self.rows.len());
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
                        let name = self.columns.get(index).map(String::as_str).unwrap_or("");
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
        let mut out = String::from("## GraphQL schema\n\n");
        fenced(&mut out, Some("graphql"), &self.schema);
        out
    }
}

impl MarkdownResult for GetOccurrencesResult {
    fn to_markdown(&self) -> String {
        let rows = self
            .occurrences
            .iter()
            .map(|row| crate::mcp::occurrence_format::OccurrenceRow {
                target_key: row.target_key.clone(),
                target: row.target.as_ref().map(|target| {
                    crate::mcp::occurrence_format::SymbolSummary {
                        signature: (!target.signature.is_empty()).then(|| target.signature.clone()),
                        name: None,
                        kind: None,
                        path: target.path.clone(),
                    }
                }),
                owner: crate::mcp::occurrence_format::OccurrenceOwner {
                    key: row.owner.key.0.clone(),
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
            self.next_cursor.as_deref(),
        )
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
            SemanticStatus::Unavailable { reason } => {
                let reason = match reason.as_str() {
                    "NoEmbedder" => crate::mcp::semantic_format::SemanticUnavailable::NoEmbedder,
                    "EmptyCorpus" => crate::mcp::semantic_format::SemanticUnavailable::EmptyCorpus,
                    "ModelFailed" => crate::mcp::semantic_format::SemanticUnavailable::ModelFailed,
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
        let mut result = crate::mcp::semantic_format::SemanticResult::new(
            self.query.clone(),
            status,
            hits,
        );
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

fn search_row(index: usize, hit: &HitRow) -> Vec<String> {
    let declaration = signature(&hit.sig_preview);
    let declaration = if declaration.is_empty() {
        format!("{} {}", kind_label(&hit.kind), hit.display_name)
    } else {
        declaration
    };
    vec![
        index.to_string(),
        declaration,
        SymbolKeyDto::from_wire(&hit.key).0,
    ]
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
        _ => ("unknown".to_owned(), String::new()),
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
        _ => "unknown".to_owned(),
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
    if let Some(key_index) = columns.iter().position(|column| column == "key") {
        if let Some(position) = comment_indices.iter().position(|index| *index == key_index) {
            comment_indices.remove(position);
            comment_indices.insert(0, key_index);
        }
    }

    for row in rows {
        let mut body = String::new();
        for index in &comment_indices {
            let name = columns
                .get(*index)
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("#{index_plus_one}", index_plus_one = *index + 1));
            let value = row.cells.get(*index).map(String::as_str).unwrap_or("");
            push_query_comment(&mut body, &name, value);
        }

        let signature = signature_index
            .and_then(|index| row.cells.get(index))
            .map(String::as_str)
            .unwrap_or("");
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
            let value = row.get(index).map(String::as_str).unwrap_or("");
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
        _ => "",
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
        _ => "unknown",
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
    use crate::mcp::tools::QueryResultRow;
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
            format_search_result(&SearchResult {
                hits: vec![],
                truncated: false,
                next_cursor: None
            }),
            "## Search · 0\n\n(no matches)\n"
        );
        assert!(
            format_usages_result(&UsagesResult {
                usages: vec![],
                truncated: false,
                next_cursor: None
            })
            .contains("(none)")
        );
        assert!(format_packages_result(&PackagesResult { packages: vec![] }).contains("(none)"));
        assert!(
            format_query_result(&QueryResult {
                columns: vec![],
                rows: vec![],
                truncated: false,
                next_cursor: None
            })
            .contains("(no rows)")
        );
        assert!(format_symbols_result(&SymbolsResult { symbols: vec![] }).contains("(none)"));
    }

    #[test]
    fn pagination_uses_cursor_and_omits_false_booleans() {
        let result = UsagesResult {
            usages: vec![],
            truncated: true,
            next_cursor: Some("50|next".into()),
        };
        let rendered = format_usages_result(&result);
        assert!(rendered.contains("next: `50|next`"));
        assert!(!rendered.contains("truncated"));

        let malformed = UsagesResult {
            usages: vec![],
            truncated: true,
            next_cursor: None,
        };
        assert!(format_usages_result(&malformed).contains("truncated without cursor"));
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
        };
        let rendered = format_query_result(&result);
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
        };
        let rendered = format_query_result(&result);
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
        let rendered = format_compact_symbol_doc(&symbol(Some(source)));
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
        let rendered_source = format_compact_symbol_doc(&symbol(Some(&source)));
        assert!(rendered_source.contains(&source));

        let schema = "type X { field: String }\n".repeat(20_000);
        let rendered_schema = format_schema_result(&SchemaResult {
            schema: schema.clone(),
        });
        assert!(rendered_schema.contains(&schema));
        assert_eq!(rendered_schema.matches("type X").count(), 20_000);
    }

    #[test]
    fn status_variants_keep_the_actionable_difference() {
        let selected = format_select_version_result(&SelectVersionResult::Switched {
            package: PackageLineageDto("cargo:demo".into()),
            version: "1.2.3".into(),
            symbol_count: 42,
        });
        assert!(selected.contains("Version selected"));
        assert!(selected.contains("symbols: 42"));

        let missing = format_select_version_result(&SelectVersionResult::NotLoaded {
            package: PackageLineageDto("cargo:demo".into()),
            version: "9.9.9".into(),
        });
        assert!(missing.contains("Version not loaded"));

        let running = format_index_package_result(&IndexPackageResult::Running {
            purl: "pkg:cargo/demo@1.0.0".into(),
            stage: "downloading".into(),
            received_bytes: Some(4),
            total_bytes: Some(10),
            elapsed_seconds: 3,
            joined_existing_job: true,
        });
        assert!(running.contains("4/10 B"));
        assert!(running.contains("attached: existing job"));

        let indexed = format_index_package_result(&IndexPackageResult::Indexed {
            purl: "pkg:cargo/demo@1.0.0".into(),
            package: "cargo:demo".into(),
            version: "1.0.0".into(),
            symbol_count: 4,
            root_key: Some(key_dto().0),
            integrity: integrity(true),
        });
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
        };
        let rendered = format_query_result(&result);
        assert!(rendered.contains("#2"));
        assert!(rendered.contains("extra"));
    }

    #[test]
    fn search_and_diff_keep_navigable_keys() {
        let search = SearchResult {
            hits: vec![HitRow {
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
            }],
            truncated: false,
            next_cursor: None,
        };
        let rendered = format_search_result(&search);
        assert!(rendered.contains("cargo:demo#070707"));
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
        let rendered = format_diff_versions_result(&diff);
        assert!(rendered.contains("removed"));
        assert!(rendered.contains("| declaration |"));
        assert!(rendered.contains("pub struct Thing<T>"));
        assert!(!rendered.contains("| name |"));
        assert!(!rendered.contains("| kind |"));
        assert!(rendered.contains("cargo:demo#080808"));
    }

    #[test]
    fn source_without_newline_gets_only_a_structural_closing_line() {
        let rendered = format_compact_symbol_doc(&symbol(Some("fn main() {}")));
        assert!(rendered.contains("\nfn main() {}\n```"));
        assert_eq!(rendered.matches("fn main() {}").count(), 1);
    }

    #[test]
    fn package_and_version_renderers_omit_derivable_false_markers() {
        let packages = format_packages_result(&PackagesResult {
            packages: vec![crate::mcp::tools::PackageSummary {
                lineage: "cargo:demo".into(),
                name: "demo".into(),
                ecosystem: "cargo".into(),
            }],
        });
        assert!(packages.contains("| package |"));
        assert!(!packages.contains("ecosystem"));

        let versions = format_list_versions_result(&ListVersionsResult {
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
        });
        assert!(versions.contains("current"));
        assert_eq!(versions.matches("current").count(), 1);
        assert!(!versions.contains("is_current"));
    }

    #[test]
    fn semantic_result_carries_exact_documentation_evidence() {
        let rendered = format_semantic_search_result(&SemanticSearchResult {
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
        });

        assert!(rendered.contains("status: ready · 1 match"));
        assert!(rendered.contains(
            "| 1 | `fn retry_with_backoff(request: Request) -> Result<Response>` | `cargo:demo#1111111111111111111111111111111111111111111111111111111111111111` | exact |"
        ));
        assert!(rendered.contains("| exact |"));
        assert!(rendered.contains("Retries a failed request.\nUse `backoff` carefully."));
        assert!(!rendered.contains("score"), "scores stay opt-in");
    }
}
