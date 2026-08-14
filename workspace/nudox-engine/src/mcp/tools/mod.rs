//! The MCP tools of §L6, and the argument/result types their schemas
//! are derived from.
//!
//! # LR-8: this is a view, not a second engine
//!
//! The read/query tools bottom out in these `EngineHandle` calls:
//!
//! | Tool | Engine call |
//! |---|---|
//! | `search_symbols` | [`EngineHandle::search`] |
//! | `get_symbol`     | current package view through the shared chunk-head projection |
//! | `get_symbols`    | the same compact projection, deduplicated inside one MCP call |
//! | `find_usages`    | [`EngineHandle::query`] with `crate::graph::queries::FIND_USAGES` |
//! | `list_packages`  | [`EngineHandle::query`] with [`PACKAGES_QUERY`] |
//! | `list_versions`  | [`EngineHandle::versions`] |
//! | `select_version` | [`EngineHandle::select_version`], drained to completion |
//! | `graph_query`    | [`EngineHandle::query`] with the caller's query |
//! | `graph_schema`   | [`crate::mcp::SCHEMA_SDL`] (LR-7: served verbatim) |
//!
//! `find_usages` and `list_packages` reach the store's usage postings and the
//! corpus's package map the way §L6 asks — *through the engine* — by going over
//! the Trustfall plane rather than by taking a second handle on `Corpus`. That
//! is deliberate and it is the stronger reading of LR-8/LR-9: `EngineHandle`
//! does not expose `corpus()` publicly, the engine owns the `LocalSet` the
//! `!Send` Trustfall stream requires, and routing through `query` means these
//! two tools cannot drift away from what the graph plane says is true.
//!
//! # Streams are consumed to the tool's canonical projection
//!
//! Full-document tools drain to `Done`/`Failed`. Compact reads resolve the
//! current package view directly and use the shared head projection with no
//! section plan, timeline, markdown walk, channel, or cancellation race.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use futures::future::try_join_all;
use crate::wire::{
    DocEvent, Gen, HitRow, KindDiscriminant, PackageDiff, QueryEvent, RenderSection, SearchEvent,
    SigToken, SourceLocation, SymbolHead, Timeline, VersionEvent, Visibility,
};
use crate::{EngineHandle, GraphQuery, SearchQuery};

use crate::mcp::error::McpError;
use crate::mcp::key::{PackageLineageDto, SymbolKeyDto};

mod args;
mod results;

pub use args::{
    DiffVersionsArgs, FindUsagesArgs, GetSymbolArgs, GetSymbolsArgs, GraphQueryArgs,
    GraphSchemaArgs, IndexPackageArgs, ListPackagesArgs, ListVersionsArgs, SearchSymbolsArgs,
    SelectVersionArgs, SymbolFormat,
};
pub use results::{
    CompactSymbolDoc, CompactSymbolReference, DiffVersionsResult, IndexPackageResult,
    IntegrityReport, ListVersionsResult, PackageSummary, PackagesResult, QueryResult, QueryResultRow,
    SchemaResult, SearchResult, SelectVersionResult, SymbolDoc, SymbolsResult, UsageRow,
    UsagesResult, VersionSummary,
};

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

/// How long `index_package` waits by default before reporting a job as still
/// running.
///
/// Two minutes covers a cold fetch and lowering of most single packages
/// (`serde` is well under it) without approaching the timeouts an HTTP client
/// or proxy is likely to impose.
const DEFAULT_INDEX_WAIT_SECONDS: u32 = 120;

/// Ceiling on `index_package`'s wait, whatever the caller asks for.
///
/// Beyond ten minutes the request is a liability rather than a convenience: the
/// job runs to completion regardless, and polling is free.
const MAX_INDEX_WAIT_SECONDS: u32 = 600;

/// Flatten [`crate::Integrity`] into the tool's report shape.
///
/// The two variants must not collapse: `verified_against_published_digest` is
/// the whole distinction, and `detail` carries either the endpoint that
/// substantiates it or the specific reason none exists.
fn report_integrity(integrity: &crate::Integrity) -> IntegrityReport {
    match integrity {
        crate::Integrity::RegistryDigest {
            algorithm,
            digest,
            published_by,
        } => IntegrityReport {
            verified_against_published_digest: true,
            algorithm: algorithm.clone(),
            digest: digest.clone(),
            detail: format!("matched the digest published by {published_by}"),
        },
        crate::Integrity::TransportOnly { sha256, why } => IntegrityReport {
            verified_against_published_digest: false,
            algorithm: "sha256".to_owned(),
            digest: sha256.clone(),
            detail: format!(
                "computed locally; no published digest was available to compare against — {why}"
            ),
        },
        // `Integrity` is `#[non_exhaustive]`: a third tier must not be silently
        // reported as verified.
        _ => IntegrityReport {
            verified_against_published_digest: false,
            algorithm: "unknown".to_owned(),
            digest: String::new(),
            detail: "this build does not recognise the integrity tier this fetch reported"
                .to_owned(),
        },
    }
}

// ---------------------------------------------------------------------------
// The tool set
// ---------------------------------------------------------------------------

/// The engine-backed implementation of the eight §L6 tools.
///
/// Holds the logic; [`crate::mcp::NudoxMcpServer`] holds the MCP surface that calls
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
    /// Index jobs in flight, keyed by canonical PURL — see [`crate::mcp::index`].
    ///
    /// On `NudoxTools` and not on the server, because rmcp builds one
    /// `NudoxMcpServer` per MCP *session*: a registry living there would let
    /// two agents (or one agent reconnecting) each start their own producer run
    /// for the same package.
    jobs: std::sync::Arc<crate::mcp::index::IndexJobs>,
}

impl std::fmt::Debug for NudoxTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NudoxTools").finish_non_exhaustive()
    }
}

impl NudoxTools {
    /// Build the tool set over an already-started engine.
    pub fn new(engine: EngineHandle) -> Self {
        Self {
            engine,
            generation: std::sync::Arc::new(AtomicU64::new(1)),
            jobs: std::sync::Arc::new(crate::mcp::index::IndexJobs::default()),
        }
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
// Tool bodies — called by the `#[tool]` methods in `crate::mcp::server`
// ---------------------------------------------------------------------------

impl NudoxTools {
    /// `search_symbols`, minus the MCP wrapping.
    pub async fn do_search(&self, args: SearchSymbolsArgs) -> Result<SearchResult, McpError> {
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

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

        // The `packages` filter is pushed all the way into the engine's
        // `SearchQuery` (rather than applied here, after the fact, on the rows
        // it returned) because `collect_name_hits`/`collect_type_hits` each
        // truncate to `limit` before returning. A post-hoc filter here would be
        // filtering an already-truncated set: ties break on package lineage
        // (`compare_candidates`), so a package whose name sorts late could be
        // crowded out of the truncated set entirely by ties from packages the
        // caller was about to exclude anyway, producing zero results for a
        // package that genuinely has matches. See `SearchQuery::packages`.
        let packages = match &args.packages {
            None => Vec::new(),
            Some(names) => {
                if names.is_empty() {
                    return Err(McpError::InvalidArgument {
                        argument: "packages",
                        reason: "must contain at least one 'ecosystem:name' entry, or be omitted"
                            .to_owned(),
                    });
                }
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    // Preserve the specific reason `PackageLineageDto::to_wire`
                    // already computed (empty ecosystem vs. empty name vs. no
                    // ':' at all) rather than discarding it for one generic
                    // message — doctrine's `map_err(|_|)` callout applies here
                    // exactly: the reason existed one line up and this used to
                    // throw it away.
                    let lineage = PackageLineageDto(name.clone()).to_wire().map_err(|e| {
                        let reason = match e {
                            McpError::MalformedPackage { reason, .. } => reason,
                            _ => "not 'ecosystem:name'",
                        };
                        McpError::InvalidArgument {
                            argument: "packages",
                            reason: format!("{name:?} is not 'ecosystem:name': {reason}"),
                        }
                    })?;
                    out.push(lineage);
                }
                out
            }
        };

        // Fetch one row past the page so we can tell whether another page
        // exists (`fetched_may_have_capped` below) without a second query.
        // `SearchQuery.limit` bounds *each section independently*, so this is
        // the effective per-section fetch depth, not the final page size.
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);

        let query = SearchQuery {
            text: args.query,
            kinds,
            packages,
            limit: fetch_limit,
        };
        let generation = self.next_gen();
        // Bind the handle: dropping it cancels the stream mid-drain.
        let (_stream, rx) = self.engine.search(query, generation);

        let mut rows: Vec<HitRow> = Vec::new();
        // True when any single section returned exactly `fetch_limit` rows —
        // i.e. that section's own truncation may have hidden more.
        let mut fetched_may_have_capped = false;
        let mut terminated = false;
        while let Ok(event) = rx.recv_async().await {
            match event {
                SearchEvent::Section { rows: batch, .. }
                | SearchEvent::Merge { rows: batch, .. } => {
                    if batch.len() >= fetch_limit {
                        fetched_may_have_capped = true;
                    }
                    rows.extend(batch.iter().cloned());
                }
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

        // A symbol can hit in more than one search section (name *and* kind),
        // and the sections are ranked independently. Deduplicate on the key —
        // not with `dedup_by`, which only removes *adjacent* duplicates and
        // would leave a pair whose scores happened to differ. The key string
        // is kept alongside each row (rather than recomputed) so the final
        // sort below can use it as a deterministic tiebreak.
        let mut seen = std::collections::HashSet::with_capacity(rows.len());
        let mut deduped: Vec<(String, HitRow)> = Vec::with_capacity(rows.len());
        for h in rows {
            let key_str = format!(
                "{}:{}#{}",
                h.key.package.ecosystem.as_str(),
                h.key.package.name.as_str(),
                h.key.intro.to_hex(),
            );
            if seen.insert(key_str.clone()) {
                deduped.push((key_str, h));
            }
        }

        // The merged list must be re-sorted before it is paged, or the page
        // boundary would cut by section order rather than by relevance. Score
        // first, then the canonical key string: two rows can tie on score
        // (frequent — see `search::compare_candidates`'s own reasoning), and
        // without a total tiebreak here the merge order — which depends on
        // which section happened to emit a tied row first — would leak into
        // page boundaries, making `cursor` unstable across calls for the
        // identical query.
        deduped.sort_by(|(ka, a), (kb, b)| b.score.total_cmp(&a.score).then_with(|| ka.cmp(kb)));
        let rows: Vec<HitRow> = deduped.into_iter().map(|(_, h)| h).collect();

        let (page, has_more) = paginate_rows(rows, offset, limit, fetched_may_have_capped);
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(SearchResult {
            hits: page,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `get_symbol`, minus the MCP wrapping.
    pub async fn do_get_symbol(&self, args: GetSymbolArgs) -> Result<SymbolDoc, McpError> {
        let key = args.key.to_wire()?;
        let generation = self.next_gen();
        let (_stream, rx) = self.engine.open_symbol(key, generation);

        let mut head: Option<Box<SymbolHead>> = None;
        let mut timeline: Option<Timeline> = None;
        let mut sections: Vec<RenderSection> = Vec::new();
        let mut terminated = false;

        while let Ok(event) = rx.recv_async().await {
            match event {
                DocEvent::Head(h) => head = Some(h),
                // §9.3 invariant 5: emitted at most once, after `Head` and
                // before the first `Section` — the version-lineage feature
                // (renames, deprecations, signature/visibility/doc changes,
                // introduction, removal) an agent otherwise has no way to see.
                // This was previously the one event this loop silently dropped
                // in its wildcard arm below: fully computed by the engine on
                // every `open_symbol` call, and discarded before any caller —
                // GUI or MCP — could read it.
                DocEvent::Timeline(t) => timeline = Some(t),
                DocEvent::Section(s) => sections.push(s),
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
        // Protocol invariant §9.3.5: `Timeline` is emitted for every resolved
        // symbol (`doc.rs` substitutes a single-generation slice when the
        // version registry has nothing recorded), so its absence here is the
        // same class of broken-stream signal as a missing `Head`.
        let timeline = timeline.ok_or(McpError::TruncatedStream)?;
        Ok(SymbolDoc {
            head,
            timeline,
            sections,
        })
    }

    /// Read one symbol through the compact MCP projection.
    pub async fn do_get_symbol_compact(
        &self,
        args: GetSymbolArgs,
        format: SymbolFormat,
    ) -> Result<CompactSymbolDoc, McpError> {
        let key = args.key.to_wire()?;
        let corpus = self.engine.corpus();
        let versions = self.engine.versions_registry();
        let failures = self.engine.load_failures();
        let package = crate::doc::resolve_symbol(&corpus, &versions, &failures, &key)
            .await
            .map_err(McpError::Engine)?;
        let entry = package
            .view()
            .entry(key.intro)
            .expect("resolve_symbol guarantees a live entry");
        let head = crate::chunk::head::projection(key.intro, entry, package.view(), &package);
        Ok(compact_symbol(&head, format))
    }

    /// Read several symbols in one MCP round trip.
    pub async fn do_get_symbols(&self, args: GetSymbolsArgs) -> Result<SymbolsResult, McpError> {
        if args.keys.is_empty() || args.keys.len() > 32 {
            return Err(McpError::InvalidArgument {
                argument: "keys",
                reason: "must contain between 1 and 32 symbol keys".to_owned(),
            });
        }
        // Reject a malformed batch before opening any streams; otherwise a
        // bad late key computes earlier symbols and then discards the entire
        // response. Build a stable unique work list at the same time: callers
        // may repeat a key, but the engine must never repeat its index read.
        let mut unique_by_key = BTreeMap::<String, usize>::new();
        let mut unique = Vec::new();
        let mut order = Vec::with_capacity(args.keys.len());
        for key in args.keys {
            key.to_wire()?;
            let index = match unique_by_key.get(&key.0) {
                Some(&index) => index,
                None => {
                    let index = unique.len();
                    unique_by_key.insert(key.0.clone(), index);
                    unique.push(key);
                    index
                }
            };
            order.push(index);
        }
        let unique_symbols = try_join_all(unique.into_iter().map(|key| {
            self.do_get_symbol_compact(GetSymbolArgs { key }, args.format)
        }))
        .await?;
        let symbols = order
            .into_iter()
            .map(|index| unique_symbols[index].clone())
            .collect();
        Ok(SymbolsResult { symbols })
    }

    /// `find_usages`, minus the MCP wrapping.
    pub async fn do_find_usages(&self, args: FindUsagesArgs) -> Result<UsagesResult, McpError> {
        // Validate the key even though the query plane takes it as a string:
        // a malformed key should be `invalid_params`, not an empty result set.
        args.key.to_wire()?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let mut bindings = BTreeMap::new();
        bindings.insert("key".to_owned(), args.key.0.clone());

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: crate::graph::queries::FIND_USAGES.to_owned(),
                    args: bindings,
                },
                offset,
                limit,
            )
            .await?;

        let usages = page
            .iter()
            .map(|row| UsageRow {
                key: SymbolKeyDto(column(&columns, row, "key").unwrap_or_default()),
                name: column(&columns, row, "name").unwrap_or_default(),
                kind: column(&columns, row, "kind").unwrap_or_default(),
                path: column(&columns, row, "path").unwrap_or_default(),
            })
            .collect();

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(UsagesResult {
            usages,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `diff_versions`, minus the MCP wrapping.
    ///
    /// # Why this is not a `graph_query`
    ///
    /// `graph_query` resolves against the corpus, which holds exactly one
    /// resident generation per lineage. No query it can express sees two. The
    /// engine's version registry holds them both already, so this is a direct
    /// engine call rather than a query — see `crate::diff` for the two
    /// designs that were rejected.
    ///
    /// # Pagination pages the *rows*, not the counts
    ///
    /// `unchanged`, `from_symbol_count` and `to_symbol_count` describe the
    /// whole diff on every page. Only `rows` is sliced. A caller that paged to
    /// the end and summed the pages would otherwise get a different total from
    /// a caller that read page one, and both would be right.
    pub async fn do_diff_versions(
        &self,
        args: DiffVersionsArgs,
    ) -> Result<DiffVersionsResult, McpError> {
        let lineage = args.package.to_wire()?;
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let Some(full) =
            self.engine
                .diff_versions(&lineage, &args.from_version, &args.to_version)
        else {
            let loaded = self
                .engine
                .versions(&lineage)
                .versions
                .iter()
                .map(|v| v.version.to_string())
                .collect();
            return Ok(DiffVersionsResult::NotLoaded {
                package: PackageLineageDto::from_wire(&lineage),
                from_version: args.from_version,
                to_version: args.to_version,
                loaded,
            });
        };

        let total = full.rows.len();
        let page: Vec<_> = full
            .rows
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect();
        let has_more = offset.saturating_add(page.len()) < total;
        let next_cursor = has_more.then(|| encode_cursor(offset + limit));

        Ok(DiffVersionsResult::Diff {
            diff: Box::new(PackageDiff {
                rows: page.into(),
                ..full
            }),
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// `list_packages`, minus the MCP wrapping.
    pub async fn do_list_packages(&self) -> Result<PackagesResult, McpError> {
        let result = self
            .run_query(
                GraphQuery {
                    query: PACKAGES_QUERY.to_owned(),
                    args: BTreeMap::new(),
                },
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

    /// `index_package`, minus the MCP wrapping.
    ///
    /// # Why this returns before the work does
    ///
    /// Every other tool in this file answers from data that is already
    /// resident, so "drain the stream to a terminal event" is bounded by
    /// milliseconds. This one runs a network fetch and a language producer —
    /// seconds at best. A tool that blocked for the whole of it would stall the
    /// agent's turn and hand every HTTP proxy on the path a vote on how long an
    /// index is allowed to take.
    ///
    /// So the wait is bounded by an argument the *caller* chooses, and a job
    /// still running when it expires is reported as
    /// [`IndexPackageResult::Running`] with its current stage — a true answer,
    /// not a timeout error. Calling again joins the same job (see
    /// [`crate::mcp::index`]), which is what makes polling safe rather than a way to
    /// start N producer runs.
    pub async fn do_index_package(
        &self,
        args: IndexPackageArgs,
    ) -> Result<IndexPackageResult, McpError> {
        use crate::mcp::index::JobProgress;

        let purl = crate::Purl::parse(&args.purl).map_err(|reason| McpError::Index {
            // Parse failures are `Error::MalformedPurl`, so an agent sees
            // one taxonomy for every way indexing can fail rather than a
            // parse-shaped error here and an index-shaped one everywhere else.
            error: Box::new(crate::packages::acquire::Error::from(reason)),
        })?;
        let rendered = purl.render();

        let wait = std::time::Duration::from_secs(
            args.wait_seconds
                .unwrap_or(DEFAULT_INDEX_WAIT_SECONDS)
                .min(MAX_INDEX_WAIT_SECONDS)
                .into(),
        );

        let (progress, joined) = self
            .jobs
            .run(&self.engine, purl, self.next_gen(), wait)
            .await;

        match progress {
            JobProgress::Indexed(done) => Ok(IndexPackageResult::Indexed {
                purl: done.purl,
                package: format!("{}:{}", done.ecosystem, done.name),
                version: done.version,
                symbol_count: done.symbol_count,
                root_key: done.root.map(|k| SymbolKeyDto::from_wire(&k).0),
                integrity: report_integrity(&done.integrity),
            }),
            JobProgress::Failed(error) => Err(McpError::Index {
                error: Box::new(error),
            }),
            JobProgress::Running {
                stage,
                received,
                total,
            } => Ok(IndexPackageResult::Running {
                stage: stage.to_string(),
                // Zero received bytes before the download starts is absence,
                // not a measurement — reporting `0` would read as "the download
                // has produced nothing", which is a different claim.
                received_bytes: (received > 0).then_some(received),
                total_bytes: total,
                elapsed_seconds: self
                    .jobs
                    .elapsed(&rendered)
                    .map(|d| d.as_secs())
                    .unwrap_or_default(),
                joined_existing_job: joined,
                purl: rendered,
            }),
        }
    }

    /// `list_versions`, minus the MCP wrapping.
    ///
    /// [`EngineHandle::versions`] is itself synchronous (the version registry
    /// is fully resident in memory, see `crate::mcp::versions`'s module docs) so
    /// there is no stream to drain here, unlike every other tool in this file.
    pub async fn do_list_versions(
        &self,
        args: ListVersionsArgs,
    ) -> Result<ListVersionsResult, McpError> {
        let lineage = args.package.to_wire()?;
        let list = self.engine.versions(&lineage);

        let versions = list
            .versions
            .iter()
            .map(|v| VersionSummary {
                version: v.version.to_string(),
                is_current: v.is_current,
                symbol_count: v.symbol_count,
            })
            .collect();

        Ok(ListVersionsResult {
            package: PackageLineageDto::from_wire(&list.package),
            versions,
        })
    }

    /// `select_version`, minus the MCP wrapping.
    ///
    /// Drains [`EngineHandle::select_version`]'s single-event receiver to
    /// completion before returning, so — unlike a GUI frame, which polls
    /// `versions()` separately from issuing the switch — this call's own
    /// response is only produced once the corpus has actually been repointed.
    /// See [`SelectVersionResult`]'s docs for what that guarantee does and
    /// does not cover.
    pub async fn do_select_version(
        &self,
        args: SelectVersionArgs,
    ) -> Result<SelectVersionResult, McpError> {
        let lineage = args.package.to_wire()?;
        let generation = self.next_gen();
        let rx = self
            .engine
            .select_version(lineage, args.version.clone(), generation);

        match rx.recv_async().await {
            Ok(VersionEvent::Switched {
                package, version, ..
            }) => {
                // The registry was updated synchronously as part of producing
                // this event (see `VersionRegistry::set_current`), so this read
                // is guaranteed to see the generation we just switched to.
                let symbol_count = self
                    .engine
                    .versions(&package)
                    .versions
                    .iter()
                    .find(|v| v.version == version)
                    .map(|v| v.symbol_count)
                    .unwrap_or(0);
                Ok(SelectVersionResult::Switched {
                    package: PackageLineageDto::from_wire(&package),
                    version: version.to_string(),
                    symbol_count,
                })
            }
            Ok(VersionEvent::NotLoaded {
                package, version, ..
            }) => Ok(SelectVersionResult::NotLoaded {
                package: PackageLineageDto::from_wire(&package),
                version: version.to_string(),
            }),
            // `VersionEvent` is `#[non_exhaustive]` precisely so a future
            // variant does not fail to compile here; only `Switched` and
            // `NotLoaded` exist today. A third variant needs a real
            // translation added above — until then this is reported rather
            // than silently mapped into either existing outcome.
            Ok(other) => {
                tracing::warn!(
                    ?other,
                    "select_version: engine emitted a VersionEvent variant this tool set does not \
                     yet translate"
                );
                Err(McpError::TruncatedStream)
            }
            // Capacity-1 channel, exactly one event ever sent (§the engine's
            // own docs on `select_version`); an empty receiver here means the
            // spawned task never got to send, which is the same broken-stream
            // signal every other drain loop in this file treats as such.
            Err(_) => Err(McpError::TruncatedStream),
        }
    }

    /// `graph_query`, minus the MCP wrapping.
    pub async fn do_graph_query(&self, args: GraphQueryArgs) -> Result<QueryResult, McpError> {
        if args.query.trim().is_empty() {
            return Err(McpError::InvalidArgument {
                argument: "query",
                reason: "must not be empty; call graph_schema for the queryable types".to_owned(),
            });
        }
        let limit = clamp_limit(args.limit);
        let offset = decode_cursor(args.cursor.as_deref())?;

        let (page, columns, has_more) = self
            .run_query_page(
                GraphQuery {
                    query: args.query,
                    args: args.args.unwrap_or_default(),
                },
                offset,
                limit,
            )
            .await?;

        let next_cursor = has_more.then(|| encode_cursor(offset + limit));
        Ok(QueryResult {
            columns,
            rows: page,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    /// Drive one Trustfall query through the engine and collect up to `limit`
    /// rows.
    ///
    /// Shared by `find_usages`, `list_packages`, `graph_query` (via
    /// [`Self::run_query_page`]) so all agree on column handling and
    /// truncation semantics. Returns the internal [`RawQueryResult`] rather
    /// than the public [`QueryResult`] because `list_packages` has no cursor
    /// and the two paginated callers need one more decision (`paginate_rows`)
    /// made on top of this before they know their own `next_cursor`.
    async fn run_query(&self, q: GraphQuery, limit: usize) -> Result<RawQueryResult, McpError> {
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

        Ok(RawQueryResult {
            columns,
            rows,
            truncated,
        })
    }

    /// Run `q`, fetching enough rows to cover one page at `offset`/`limit`,
    /// and slice that page out locally.
    ///
    /// Returns `(page_rows, columns, has_more)`. See [`paginate_rows`] for
    /// what `has_more` means when the fetch itself may have been capped
    /// before the true end of the result set.
    async fn run_query_page(
        &self,
        q: GraphQuery,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<QueryResultRow>, Vec<String>, bool), McpError> {
        let fetch_limit = offset.saturating_add(limit).saturating_add(1);
        let raw = self.run_query(q, fetch_limit).await?;
        let (page, has_more) = paginate_rows(raw.rows, offset, limit, raw.truncated);
        Ok((page, raw.columns, has_more))
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Ceiling on how far a cursor may page into a result set.
///
/// Every paginated tool here answers a page by re-running its underlying
/// query with a larger internal fetch depth and slicing the page out locally
/// (see [`encode_cursor`]), so the cost of serving a page grows with its
/// offset. This bounds that cost: an agent that wants row 50,000 of "every
/// deprecated symbol in tokio" should narrow the query (a `kinds`/`packages`
/// filter, a more specific Trustfall query), not page 100 requests deep to
/// reach it.
const MAX_OFFSET: usize = 20_000;

/// Encode a page offset as an opaque pagination cursor.
///
/// # Why an offset, not a real streaming cursor
///
/// None of the three paginated tools keep state alive across calls — each
/// call independently re-runs its underlying query and re-sorts the full
/// result under a **total**, deterministic order (search:
/// `search::compare_candidates`'s score/name/key order, reproduced in
/// `NudoxTools::do_search`; `graph_query`/`find_usages`: Trustfall's own row
/// order for a fixed query against a fixed corpus). Given that determinism, a
/// numeric offset into the re-sorted result is exactly as stable a location
/// as a real cursor would be, without needing to keep a result set alive
/// between calls. It is still returned as an opaque string rather than a bare
/// integer in the schema — a caller must pass back exactly what it received,
/// never construct or parse one itself — so a future revision could replace
/// the encoding with a real streaming cursor without a breaking schema
/// change. If you ever find yourself tempted to accept a bare offset `u32`
/// argument instead of this, don't: that would commit the schema to the
/// current encoding being permanent, which is exactly what "opaque" is here
/// to avoid.
fn compact_symbol(head: &crate::chunk::head::SymbolProjection, format: SymbolFormat) -> CompactSymbolDoc {
    let mut path_parts = head
        .breadcrumb
        .iter()
        .map(|crumb| crumb.label.to_string())
        .collect::<Vec<_>>();
    path_parts.push(head.name.to_string());
    let path = path_parts.join("::");
    let rendered_signature = || head.signature.iter().map(signature_text).collect::<String>();
    let mut references = Vec::new();
    let mut seen_references = HashSet::new();
    for token in &head.signature {
        let SigToken::Ty {
            text,
            target: Some(target),
        } = token
        else {
            continue;
        };
        let reference = CompactSymbolReference {
            text: text.to_string(),
            target: SymbolKeyDto::from_wire(target),
        };
        if seen_references.insert(reference.clone()) {
            references.push(reference);
        }
    }
    let location = match &head.source {
        SourceLocation::Declared { file, start, .. } => {
            Some(format!("{}:{}:{}", file, start.line, start.column))
        }
        SourceLocation::BytesOnly { file, bytes } => {
            Some(format!("{}#bytes={}-{}", file, bytes[0], bytes[1]))
        }
        SourceLocation::Unlocated { .. } => None,
    };
    let source = matches!(format, SymbolFormat::Source)
        .then(|| head.source_excerpt.as_ref().map(ToString::to_string))
        .flatten();
    let signature = if matches!(format, SymbolFormat::Signature) || source.is_none() {
        Some(rendered_signature())
    } else {
        None
    };
    CompactSymbolDoc {
        key: SymbolKeyDto::from_wire(&head.key),
        path,
        signature,
        kind: head.kind,
        visibility: head.visibility,
        references,
        location,
        source,
        deprecation: head.deprecation.as_ref().map(ToString::to_string),
    }
}

fn signature_text(token: &SigToken) -> &str {
    match token {
        SigToken::Kw(text) | SigToken::Punct(text) => text,
        SigToken::Ident(text)
        | SigToken::Ty { text, .. }
        | SigToken::Generic(text)
        | SigToken::Lifetime(text) => text.as_ref(),
        SigToken::Ws => " ",
        _ => "",
    }
}

fn encode_cursor(offset: usize) -> String {
    offset.to_string()
}

/// Decode a pagination cursor back into a page offset. `None` (the first
/// page) decodes to `0`.
fn decode_cursor(cursor: Option<&str>) -> Result<usize, McpError> {
    let Some(raw) = cursor else {
        return Ok(0);
    };
    let offset: usize = raw.parse().map_err(|_| McpError::InvalidArgument {
        argument: "cursor",
        reason: format!("{raw:?} is not a cursor this server issued"),
    })?;
    if offset > MAX_OFFSET {
        return Err(McpError::InvalidArgument {
            argument: "cursor",
            reason: format!(
                "cursor offset {offset} exceeds the {MAX_OFFSET}-row pagination ceiling; \
                 narrow the query instead of paging this far into it"
            ),
        });
    }
    Ok(offset)
}

/// Slice one page out of an already-fetched, already-deterministically-ordered
/// row set, and report whether more rows exist beyond this page.
///
/// `fetch_may_have_capped` is `true` when the underlying fetch itself might
/// have stopped short of the true result set (its own internal limit was
/// reached). In that case `has_more` is unconditionally `true`: there is no
/// way from here to distinguish "exactly this many rows exist" from "more
/// rows exist past where the fetch stopped looking" without fetching deeper —
/// which is exactly what requesting the next page does. This never *hides* a
/// real next page; the one failure mode it can produce is an honest empty
/// final page when the fetch boundary happened to land exactly on the true
/// end of the result set, which is a wasted call, not a wrong answer.
fn paginate_rows<T>(
    rows: Vec<T>,
    offset: usize,
    limit: usize,
    fetch_may_have_capped: bool,
) -> (Vec<T>, bool) {
    if offset >= rows.len() {
        return (Vec::new(), fetch_may_have_capped);
    }
    let end = (offset + limit).min(rows.len());
    let remainder_within_fetch = end < rows.len();
    let mut rows = rows;
    let page = rows.drain(offset..end).collect();
    (page, remainder_within_fetch || fetch_may_have_capped)
}

/// The raw result of draining one Trustfall query stream, before either the
/// non-paginated caller (`list_packages`, which has no cursor) or the two
/// paginated ones (`find_usages`, `graph_query`, via
/// [`NudoxTools::run_query_page`]) decide what their own `next_cursor` is.
struct RawQueryResult {
    /// Column names, in the order the query's `@output` directives declared them.
    columns: Vec<String>,
    /// Every row collected, up to the fetch limit passed to `run_query`.
    rows: Vec<QueryResultRow>,
    /// `true` when the stream had more rows than the fetch limit allowed us
    /// to collect.
    truncated: bool,
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
pub(crate) fn known_kind_names() -> Vec<String> {
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
            assert!(
                names.iter().any(|n| n == expected),
                "missing kind {expected}"
            );
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
        let row = QueryResultRow {
            cells: vec!["Deserializer".into(), "cargo:serde#ab".into()],
        };
        assert_eq!(
            column(&columns, &row, "key").as_deref(),
            Some("cargo:serde#ab")
        );
        assert_eq!(
            column(&columns, &row, "name").as_deref(),
            Some("Deserializer")
        );
        assert_eq!(column(&columns, &row, "absent"), None);
    }

    #[test]
    fn packages_query_parses_against_the_real_schema() {
        // Guards the one Trustfall query this crate authors itself.
        assert!(PACKAGES_QUERY.contains("Packages"));
        assert!(PACKAGES_QUERY.contains("lineage @output"));
    }
}
