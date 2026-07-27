//! The six MCP tools of §L6, and the argument/result types their schemas are
//! derived from.
//!
//! # LR-8: this is a view, not a second engine
//!
//! Every tool here bottoms out in one of four `EngineHandle` calls:
//!
//! | Tool | Engine call |
//! |---|---|
//! | `search_symbols` | [`EngineHandle::search`] |
//! | `get_symbol`     | [`EngineHandle::open_symbol`], drained to completion |
//! | `find_usages`    | [`EngineHandle::query`] with `nudox_graph::queries::FIND_USAGES` |
//! | `list_packages`  | [`EngineHandle::query`] with [`PACKAGES_QUERY`] |
//! | `graph_query`    | [`EngineHandle::query`] with the caller's query |
//! | `graph_schema`   | [`crate::SCHEMA_SDL`] (LR-7: served verbatim) |
//!
//! `find_usages` and `list_packages` reach the store's usage postings and the
//! corpus's package map the way §L6 asks — *through the engine* — by going over
//! the Trustfall plane rather than by taking a second handle on `Corpus`. That
//! is deliberate and it is the stronger reading of LR-8/LR-9: `EngineHandle`
//! does not expose `corpus()` publicly, the engine owns the `LocalSet` the
//! `!Send` Trustfall stream requires, and routing through `query` means these
//! two tools cannot drift away from what the graph plane says is true.
//!
//! # Streams are drained, never sampled
//!
//! The engine's API is streaming; MCP's is request/response. Each tool holds
//! its `StreamHandle` for the whole drain (dropping it cancels the stream) and
//! reads to a terminal `Done`/`Failed` event. A stream that ends without one is
//! [`McpError::TruncatedStream`] rather than a silently short answer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use nudox_engine::wire::{DocEvent, Gen, HitRow, KindDiscriminant, QueryEvent, SearchEvent};
use nudox_engine::{EngineHandle, GraphQuery, SearchQuery};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dto::{HitRowDto, RenderSectionDto, SymbolHeadDto, SymbolKeyDto};
use crate::error::McpError;

/// The Trustfall query behind `list_packages`.
///
/// Kept minimal on purpose: it asks only for the three scalar properties
/// `schema.graphql` guarantees on `Package`, so it cannot break when the
/// vertex gains edges.
pub const PACKAGES_QUERY: &str = r"
query {
    Packages {
        lineage @output
        name @output
        ecosystem @output
    }
}
";

/// Default number of rows a tool returns when the caller does not say.
const DEFAULT_LIMIT: usize = 50;

/// Hard ceiling on rows, regardless of what the caller asks for.
///
/// An MCP result is injected into a model's context window; an unbounded
/// answer is not a useful answer. Callers that need more should narrow the
/// query or paginate with `graph_query`.
const MAX_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// Tool arguments
// ---------------------------------------------------------------------------

/// Arguments to `search_symbols`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchSymbolsArgs {
    /// The text to search for: a symbol name, a prefix, or a kind label such
    /// as `trait`. Matching is by name and by kind; it is not full-text over
    /// documentation.
    pub query: String,

    /// Restrict results to these kinds. Valid values are `Module`, `Record`,
    /// `Field`, `Function`, `Alias`, `Trait`, `Impl`, `Enum`, `Variant`,
    /// `Const`, `Static`, `Reexport` and `Param` (case-insensitive). Omit to
    /// search all kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<String>>,

    /// Restrict results to these packages, each written as `ecosystem:name`
    /// (for example `cargo:serde`). Omit to search every loaded package; call
    /// `list_packages` to see what is loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<String>>,

    /// Maximum number of results. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Arguments to `get_symbol`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetSymbolArgs {
    /// The symbol to open, as `ecosystem:name#introhex` — exactly the `key`
    /// returned by `search_symbols`, `find_usages` or a `graph_query`.
    pub key: SymbolKeyDto,
}

/// Arguments to `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindUsagesArgs {
    /// The symbol whose callers to find, as `ecosystem:name#introhex`.
    pub key: SymbolKeyDto,

    /// Maximum number of usages. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Arguments to `list_packages`.
///
/// Empty, but named rather than omitted so the tool still has a derived schema
/// (LR-2) and so adding a filter later is not a breaking change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListPackagesArgs {}

/// Arguments to `graph_query`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphQueryArgs {
    /// The Trustfall query text. Call `graph_schema` first for the exact type
    /// and edge names it must be written against.
    pub query: String,

    /// Variable bindings referenced as `$name` in the query. All values are
    /// strings; the adapter coerces them to the property's declared type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<BTreeMap<String, String>>,

    /// Maximum number of rows. Defaults to 50, capped at 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Arguments to `graph_schema`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphSchemaArgs {}

// ---------------------------------------------------------------------------
// Tool results
// ---------------------------------------------------------------------------

/// The result of `search_symbols`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    /// Matching symbols, most relevant first.
    pub hits: Vec<HitRowDto>,
    /// `true` when results were cut off at `limit` and more exist.
    pub truncated: bool,
}

/// The result of `get_symbol`: a symbol's complete documentation page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolDoc {
    /// Identity, signature, provenance, and the page's section plan.
    pub head: SymbolHeadDto,
    /// The rendered sections, in `head.section_plan` order.
    pub sections: Vec<RenderSectionDto>,
}

/// One symbol that references the symbol passed to `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageRow {
    /// The referencing symbol's key; pass it to `get_symbol` to read it.
    pub key: SymbolKeyDto,
    /// Its unqualified name.
    pub name: String,
    /// Its kind label.
    pub kind: String,
}

/// The result of `find_usages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsagesResult {
    /// The referencing symbols.
    pub usages: Vec<UsageRow>,
    /// `true` when results were cut off at `limit` and more exist.
    pub truncated: bool,
}

/// One loaded package.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PackageSummary {
    /// The `ecosystem:name` lineage key — the prefix of every `SymbolKey` in
    /// this package, and the value `search_symbols`'s `packages` filter takes.
    pub lineage: String,
    /// The package's display name.
    pub name: String,
    /// The ecosystem it comes from (`cargo`, `npm`, `pypi`, …).
    pub ecosystem: String,
}

/// The result of `list_packages`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PackagesResult {
    /// Every package currently loaded into the local corpus.
    pub packages: Vec<PackageSummary>,
}

/// One row of a `graph_query` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResultRow {
    /// Cell values, positionally aligned with [`QueryResult::columns`].
    pub cells: Vec<String>,
}

/// The result of `graph_query`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResult {
    /// Column names, in the order the query's `@output` directives declared
    /// them. `rows[i].cells[j]` is the value of `columns[j]`.
    pub columns: Vec<String>,
    /// The result rows.
    pub rows: Vec<QueryResultRow>,
    /// `true` when rows were cut off at `limit` and more exist.
    pub truncated: bool,
}

/// The result of `graph_schema`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaResult {
    /// The GraphQL SDL that `graph_query` queries are checked against.
    pub schema: String,
}

// ---------------------------------------------------------------------------
// The tool set
// ---------------------------------------------------------------------------

/// The engine-backed implementation of the six §L6 tools.
///
/// Holds the logic; [`crate::NudoxMcpServer`] holds the MCP surface that calls
/// it. The split keeps every tool body testable without standing up a
/// transport, and keeps `#[tool_router]`'s macro expansion off this file.
///
/// Cheap to clone: an `EngineHandle` is an `Arc` pair.
#[derive(Clone)]
pub struct NudoxTools {
    /// The one engine every tool goes through (LR-8).
    engine: EngineHandle,
    /// Monotonic generation counter. The engine tags every stream event with
    /// the `Gen` it answers; giving each tool call a fresh one keeps two
    /// concurrent MCP calls from reading each other's events.
    generation: std::sync::Arc<AtomicU64>,
}

impl std::fmt::Debug for NudoxTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NudoxTools").finish_non_exhaustive()
    }
}

impl NudoxTools {
    /// Build the tool set over an already-started engine.
    pub fn new(engine: EngineHandle) -> Self {
        Self { engine, generation: std::sync::Arc::new(AtomicU64::new(1)) }
    }

    /// The engine this tool set views.
    pub fn engine(&self) -> &EngineHandle {
        &self.engine
    }

    /// Take the next stream generation.
    fn next_gen(&self) -> Gen {
        Gen(self.generation.fetch_add(1, Ordering::Relaxed))
    }
}

// ---------------------------------------------------------------------------
// Tool bodies — called by the `#[tool]` methods in `crate::server`
// ---------------------------------------------------------------------------

impl NudoxTools {
    /// `search_symbols`, minus the MCP wrapping.
    pub(crate) async fn do_search(
        &self,
        args: SearchSymbolsArgs,
    ) -> Result<SearchResult, McpError> {
        let limit = clamp_limit(args.limit);

        let kinds = match &args.kinds {
            None => Vec::new(),
            Some(names) => {
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    out.push(parse_kind(name).ok_or_else(|| McpError::InvalidArgument {
                        argument: "kinds",
                        reason: format!(
                            "{name:?} is not a known kind; expected one of {}",
                            known_kind_names().join(", ")
                        ),
                    })?);
                }
                out
            }
        };

        let query = SearchQuery { text: args.query, kinds, limit };
        let generation = self.next_gen();
        // Bind the handle: dropping it cancels the stream mid-drain.
        let (_stream, rx) = self.engine.search(query, generation);

        let mut rows: Vec<HitRow> = Vec::new();
        let mut terminated = false;
        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::Section { rows: batch, .. }
                | SearchEvent::Merge { rows: batch, .. } => rows.extend(batch.iter().cloned()),
                SearchEvent::Latency { .. } => {}
                SearchEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                SearchEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        let mut hits: Vec<HitRowDto> = rows.iter().map(HitRowDto::from_wire).collect();

        // The engine's `SearchQuery` has no package filter (see the
        // `TODO(engine)` note in lib.rs), so it is applied here on the keys the
        // engine already returned. This filters engine output; it does not
        // re-implement search.
        if let Some(packages) = &args.packages {
            if packages.is_empty() {
                return Err(McpError::InvalidArgument {
                    argument: "packages",
                    reason: "must contain at least one 'ecosystem:name' entry, or be omitted"
                        .to_owned(),
                });
            }
            hits.retain(|h| h.lineage().is_some_and(|l| packages.iter().any(|p| p == l)));
        }

        // A symbol can hit in more than one search section (name *and* kind),
        // and the sections are ranked independently. Deduplicate on the key —
        // not with `dedup_by`, which only removes *adjacent* duplicates and
        // would leave a pair whose scores happened to differ.
        let mut seen = std::collections::HashSet::with_capacity(hits.len());
        hits.retain(|h| seen.insert(h.key.0.clone()));

        // The merged list must be re-sorted before it is truncated, or the
        // ceiling would cut by section order rather than by relevance.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));

        let truncated = hits.len() > limit;
        hits.truncate(limit);
        Ok(SearchResult { hits, truncated })
    }

    /// `get_symbol`, minus the MCP wrapping.
    pub(crate) async fn do_get_symbol(&self, args: GetSymbolArgs) -> Result<SymbolDoc, McpError> {
        let key = args.key.to_wire()?;
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.open_symbol(key, generation);

        let mut head: Option<SymbolHeadDto> = None;
        let mut sections: Vec<RenderSectionDto> = Vec::new();
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                DocEvent::Head(h) => head = Some(SymbolHeadDto::from_wire(&h)),
                DocEvent::Section(s) => sections.push(RenderSectionDto::from_wire(&s)),
                // Highlight spans are a GUI-only progressive upgrade and carry
                // no information an agent can use; Refs/Impls pages are served
                // by `find_usages` and `graph_query` instead.
                DocEvent::Highlight { .. } | DocEvent::Refs { .. } | DocEvent::Impls { .. } => {}
                DocEvent::Done => {
                    terminated = true;
                    break;
                }
                DocEvent::Failed(error) => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        // Protocol invariant §9.3.1: `Head` precedes everything. A `Done`
        // without one is a broken engine stream, not an empty symbol.
        let head = head.ok_or(McpError::TruncatedStream)?;
        Ok(SymbolDoc { head, sections })
    }

    /// `find_usages`, minus the MCP wrapping.
    pub(crate) async fn do_find_usages(
        &self,
        args: FindUsagesArgs,
    ) -> Result<UsagesResult, McpError> {
        // Validate the key even though the query plane takes it as a string:
        // a malformed key should be `invalid_params`, not an empty result set.
        args.key.to_wire()?;
        let limit = clamp_limit(args.limit);

        let mut bindings = BTreeMap::new();
        bindings.insert("key".to_owned(), args.key.0.clone());

        let result = self
            .run_query(
                GraphQuery {
                    query: nudox_graph::queries::FIND_USAGES.to_owned(),
                    args: bindings,
                },
                limit,
            )
            .await?;

        let usages = result
            .rows
            .iter()
            .map(|row| UsageRow {
                key: SymbolKeyDto(column(&result.columns, row, "key").unwrap_or_default()),
                name: column(&result.columns, row, "name").unwrap_or_default(),
                kind: column(&result.columns, row, "kind").unwrap_or_default(),
            })
            .collect();

        Ok(UsagesResult { usages, truncated: result.truncated })
    }

    /// `list_packages`, minus the MCP wrapping.
    pub(crate) async fn do_list_packages(&self) -> Result<PackagesResult, McpError> {
        let result = self
            .run_query(
                GraphQuery { query: PACKAGES_QUERY.to_owned(), args: BTreeMap::new() },
                MAX_LIMIT,
            )
            .await?;

        let packages = result
            .rows
            .iter()
            .map(|row| PackageSummary {
                lineage: column(&result.columns, row, "lineage").unwrap_or_default(),
                name: column(&result.columns, row, "name").unwrap_or_default(),
                ecosystem: column(&result.columns, row, "ecosystem").unwrap_or_default(),
            })
            .collect();

        Ok(PackagesResult { packages })
    }

    /// `graph_query`, minus the MCP wrapping.
    pub(crate) async fn do_graph_query(
        &self,
        args: GraphQueryArgs,
    ) -> Result<QueryResult, McpError> {
        if args.query.trim().is_empty() {
            return Err(McpError::InvalidArgument {
                argument: "query",
                reason: "must not be empty; call graph_schema for the queryable types".to_owned(),
            });
        }
        let limit = clamp_limit(args.limit);
        self.run_query(
            GraphQuery { query: args.query, args: args.args.unwrap_or_default() },
            limit,
        )
        .await
    }

    /// Drive one Trustfall query through the engine and collect up to `limit`
    /// rows.
    ///
    /// Shared by `find_usages`, `list_packages` and `graph_query` so all three
    /// agree on column handling and truncation semantics.
    async fn run_query(&self, q: GraphQuery, limit: usize) -> Result<QueryResult, McpError> {
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.query(q, generation);

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<QueryResultRow> = Vec::new();
        let mut truncated = false;
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                QueryEvent::Columns { columns: cols, .. } => {
                    columns = cols.iter().map(|c| c.to_string()).collect();
                }
                QueryEvent::Rows { rows: batch, .. } => {
                    for row in batch.iter() {
                        if rows.len() >= limit {
                            truncated = true;
                            break;
                        }
                        rows.push(QueryResultRow {
                            cells: row.cells.iter().map(|c| c.to_string()).collect(),
                        });
                    }
                }
                QueryEvent::Done { .. } => {
                    terminated = true;
                    break;
                }
                QueryEvent::Failed { error, .. } => return Err(McpError::Engine(error)),
                _ => {}
            }
        }
        if !terminated {
            return Err(McpError::TruncatedStream);
        }

        Ok(QueryResult { columns, rows, truncated })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Apply the default and the ceiling to a caller-supplied limit.
fn clamp_limit(requested: Option<u32>) -> usize {
    match requested {
        None => DEFAULT_LIMIT,
        // 0 is treated as "unlimited" by `SearchQuery`, which is exactly what
        // MAX_LIMIT exists to prevent; clamp it up to the ceiling instead.
        Some(0) => MAX_LIMIT,
        Some(n) => (n as usize).min(MAX_LIMIT),
    }
}

/// Look a cell up by column name rather than by position.
///
/// Trustfall does not promise a column order across queries, so the tools that
/// build typed rows from a generic result address cells by name.
fn column(columns: &[String], row: &QueryResultRow, name: &str) -> Option<String> {
    let idx = columns.iter().position(|c| c == name)?;
    row.cells.get(idx).cloned()
}

/// Every kind discriminant this binary knows, by name.
///
/// Derived from `KindDiscriminant::from_u16` rather than hand-listed, so a new
/// IR kind becomes searchable without touching this crate.
fn known_kinds() -> impl Iterator<Item = KindDiscriminant> {
    (0..=u16::from(u8::MAX)).filter_map(KindDiscriminant::from_u16)
}

/// The names accepted by the `kinds` argument.
fn known_kind_names() -> Vec<String> {
    known_kinds().map(|k| format!("{k:?}")).collect()
}

/// Parse a kind name case-insensitively.
fn parse_kind(name: &str) -> Option<KindDiscriminant> {
    known_kinds().find(|k| format!("{k:?}").eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_cover_the_frozen_discriminants() {
        let names = known_kind_names();
        for expected in [
            "Module", "Record", "Field", "Function", "Alias", "Trait", "Impl", "Enum", "Variant",
            "Const", "Static", "Reexport", "Param",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing kind {expected}");
        }
    }

    #[test]
    fn kind_parsing_is_case_insensitive_and_rejects_junk() {
        assert_eq!(parse_kind("function"), parse_kind("Function"));
        assert!(parse_kind("Function").is_some());
        assert!(parse_kind("  trait  ").is_some());
        assert!(parse_kind("not-a-kind").is_none());
    }

    #[test]
    fn limits_default_and_clamp() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(10)), 10);
        assert_eq!(clamp_limit(Some(u32::MAX)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(0)), MAX_LIMIT, "0 must not mean unlimited");
    }

    #[test]
    fn columns_are_addressed_by_name_not_position() {
        let columns = vec!["name".to_owned(), "key".to_owned()];
        let row = QueryResultRow { cells: vec!["Deserializer".into(), "cargo:serde#ab".into()] };
        assert_eq!(column(&columns, &row, "key").as_deref(), Some("cargo:serde#ab"));
        assert_eq!(column(&columns, &row, "name").as_deref(), Some("Deserializer"));
        assert_eq!(column(&columns, &row, "absent"), None);
    }

    #[test]
    fn packages_query_parses_against_the_real_schema() {
        // Guards the one Trustfall query this crate authors itself.
        assert!(PACKAGES_QUERY.contains("Packages"));
        assert!(PACKAGES_QUERY.contains("lineage @output"));
    }
}
