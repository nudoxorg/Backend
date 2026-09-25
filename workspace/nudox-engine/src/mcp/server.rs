//! The `rmcp` server: the §L6 tools plus the §L6 resource set.
//!
//! This module is the MCP *surface*. Every tool body immediately delegates to
//! the matching `NudoxTools::do_*` method in [`crate::mcp::tools`], which is where
//! the engine calls live. The split exists so that a tool's behaviour can be
//! tested without a transport, and so that `#[tool_router]`'s expansion does
//! not sit on top of the logic.
//!
//! # Tool descriptions are documentation
//!
//! A description string here is the *only* documentation an LLM client ever
//! sees for a tool — there is no README, no rustdoc, and no type signature it
//! can read. Each one says what the tool returns and when to prefer it over
//! the others, without re-inventorying what the response itself already
//! carries. Two things repeated across nearly every tool live in
//! [`INSTRUCTIONS`] instead of in each description: the pagination contract
//! (`next_cursor` → pass back as `cursor`, treat it as opaque) and the
//! key-stability caveat (not every `SymbolKey` survives a version switch).
//!
//! # Thirteen tools consolidated to nine (docs/MCP-SURFACE-PLAN.md §5.1)
//!
//! | merged from | into |
//! |---|---|
//! | `search_symbols` + `semantic_search` | `search` |
//! | `get_symbol` + `get_symbols` | `read` |
//! | `find_usages` + `get_occurrences` | `refs` (`direction: in\|out`) |
//! | `list_packages` + `list_versions` | `packages` |
//!
//! `diff_versions`, `index_package`, `graph_query` and `graph_schema` keep
//! their own tool (renamed `diff`/`index`/`graph`/`schema`); `select_version`
//! is unchanged — folding it in needs per-call version support the address
//! scheme does not yet have (§8's Stage 10), and dropping it would lose
//! capability rather than consolidate it.
//!
//! # Resources
//!
//! §L6 asks for "the schema, and one resource per loaded package":
//!
//! * `nudox://schema` — the verbatim Trustfall SDL, identical to what
//!   `schema(full: true)` returns (the default, argument-less `schema` call
//!   instead returns a compact reference card — see its tool description).
//!   It is a resource *as well as* a tool because MCP clients attach
//!   resources up front, which is exactly when an agent needs the schema —
//!   before it writes its first `graph` query.
//! * `nudox://package/{ecosystem}:{name}` — one per loaded package, resolved
//!   through the same engine call `packages` uses (LR-8). There is no second
//!   path to the corpus.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};

use crate::mcp::account::AccountGate;
use crate::mcp::error::McpError;
use crate::mcp::result_format::MarkdownResult;
use crate::mcp::tools::{
    DiffVersionsArgs, GraphQueryArgs, GraphSchemaArgs, IndexArgs, NudoxTools, PackagesArgs,
    PackagesResult, ReadArgs, RefsArgs, SchemaResult, SearchSymbolsArgs, SelectVersionArgs,
};

/// URI of the schema resource.
pub const SCHEMA_URI: &str = "nudox://schema";

/// URI prefix of the per-package resources.
pub const PACKAGE_URI_PREFIX: &str = "nudox://package/";

/// Instructions surfaced to the MCP client at initialisation.
///
/// The one place an agent is told how the tools relate to each other before
/// it has called any of them — and the one place the pagination and
/// key-stability explainers live, instead of being copy-pasted into every
/// tool description that needs them (docs/MCP-SURFACE-PLAN.md §2.2).
const INSTRUCTIONS: &str = "\
Code intelligence for loaded packages. Results are compact Markdown. Every symbol \
row has a key `ecosystem:name#<64 hex>` and often an address; pass either to \
`read` or `refs`. Whitespace, quotes, `::`, and a colon before the hex are accepted.

`packages` → `search` → `read`. `refs` direction `in` is callers, `out` is the \
body's own references. Pass `next_cursor` back as `cursor`. `index` adds a missing \
package (`pkg:cargo/serde@1.0.196` or a local manifest path) and is slow. \
`select_version` switches the generation other tools read. `diff` compares two \
loaded versions; only verdict `removed` means deleted. `graph` is the escape \
hatch — call `schema` once per session first. An empty graph page names the edge \
and what to query instead.";

/// The MCP server `lindsey` hosts (§L6).
///
/// `Clone` because rmcp builds one instance per MCP session; the clone is two
/// `Arc` bumps and a shared route table.
#[derive(Clone)]
pub struct NudoxMcpServer {
    /// The engine-backed tool logic (LR-8).
    tools: NudoxTools,
    /// The account gate every tool call must pass (see [`crate::mcp::account`]).
    ///
    /// Not `Option`. A server with no gate would be a server that serves a paid
    /// product for free, and making that unrepresentable costs one mandatory
    /// constructor argument.
    gate: AccountGate,
    /// The rmcp route table, generated by `#[tool_router]`.
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for NudoxMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NudoxMcpServer")
            .field("account", &self.gate.posture().tag())
            .finish_non_exhaustive()
    }
}

impl NudoxMcpServer {
    /// Build a server over an already-started engine and an account gate.
    ///
    /// The gate is a *parameter*, not a default, because the two things this
    /// server could get wrong about billing are opposite and only one of them
    /// is recoverable: serving without metering loses revenue silently and
    /// forever, while refusing to serve is loud and gets fixed in a minute.
    /// A caller that genuinely wants no metering asks for
    /// [`AccountGate::unmetered`] and states a reason.
    pub fn new(engine: crate::EngineHandle, gate: AccountGate) -> Self {
        Self::from_tools(NudoxTools::new(engine), gate)
    }

    /// Build a server over an existing tool set and an account gate.
    pub fn from_tools(tools: NudoxTools, gate: AccountGate) -> Self {
        Self {
            tools,
            gate,
            tool_router: Self::tool_router(),
        }
    }

    /// The tool set this server exposes.
    pub fn tools(&self) -> &NudoxTools {
        &self.tools
    }

    /// The account gate this server admits tool calls through.
    pub fn gate(&self) -> &AccountGate {
        &self.gate
    }

    /// Admit a billable tool call, then run it.
    ///
    /// # Why every tool body goes through this one function
    ///
    /// Every `#[tool]` method is a one-line delegation to
    /// `NudoxTools::do_*`. Copies of "check the gate, then increment the
    /// ledger, then call the tool" create chances to miss one — and an
    /// unmetered tool is invisible in review because it looks
    /// exactly like the others minus a line that is not there.
    ///
    /// So the admission and the work are welded together here. A new tool that
    /// skipped this would have to hand-roll the `McpError` → `ErrorData`
    /// conversion and the `Json` wrapper, which is visibly different from every
    /// sibling rather than subtly missing from one.
    ///
    /// # Why the future is a parameter
    ///
    /// `run` is created by the caller before the gate is consulted, which is
    /// safe because Rust futures are lazy: nothing in `self.tools.do_search(…)`
    /// executes until it is polled, and it is only polled after
    /// [`AccountGate::admit`] returns a permit. Taking a closure instead would
    /// force every call site to spell out a `move ||` for no gain.
    ///
    /// The permit is held for the duration of the call rather than dropped
    /// immediately, so "this work ran under an admission" is a lifetime rather
    /// than a comment.
    async fn metered<T: MarkdownResult>(
        &self,
        run: impl Future<Output = Result<T, McpError>>,
    ) -> Result<CallToolResult, ErrorData> {
        let _permit = self.gate.admit()?;
        let value = run.await?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            value.to_markdown(),
        )]))
    }

    /// Every resource this server serves right now.
    ///
    /// Recomputed per call rather than cached: packages load asynchronously
    /// after start-up (LR-10 paints before the data arrives), so a cached list
    /// would report an empty workspace forever to a client that connected
    /// during seeding.
    async fn resources(&self) -> Result<Vec<Resource>, McpError> {
        let mut resources = vec![
            Resource::new(SCHEMA_URI, "Trustfall schema")
                .with_title("nudox graph schema")
                .with_description(
                    "The GraphQL SDL that `graph` queries are validated against. \
Read this before writing a query.",
                )
                .with_mime_type("application/graphql"),
        ];

        for pkg in self.tools.do_list_packages().await?.packages {
            resources.push(
                Resource::new(
                    format!("{PACKAGE_URI_PREFIX}{}", pkg.lineage),
                    pkg.name.clone(),
                )
                .with_title(pkg.lineage.clone())
                .with_description(format!(
                    "Package {} from the {} ecosystem. Its lineage key `{}` prefixes every \
symbol key it declares, and is what `search`'s `packages` filter accepts.",
                    pkg.name, pkg.ecosystem, pkg.lineage
                ))
                .with_mime_type("text/markdown"),
            );
        }

        Ok(resources)
    }

    /// List the resources visible to an MCP client at this moment.
    ///
    /// This is the transport-independent equivalent of `resources/list` and
    /// exists so integration tests and embedded clients can exercise the same
    /// resource projection without manufacturing an rmcp request context.
    pub async fn list_resources_snapshot(&self) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(self.resources().await?))
    }

    /// Read one resource through the same projection used by `resources/read`.
    pub async fn read_resource_uri(
        &self,
        uri: impl Into<String>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = uri.into();

        if uri == SCHEMA_URI {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                crate::mcp::SCHEMA_SDL,
                &uri,
            )]));
        }

        if let Some(lineage) = uri.strip_prefix(PACKAGE_URI_PREFIX) {
            let packages = self.tools.do_list_packages().await?;
            let found = packages
                .packages
                .into_iter()
                .find(|p| p.lineage == lineage)
                .ok_or_else(|| McpError::UnknownResource(uri.clone()))?;
            let body = PackagesResult {
                packages: vec![found],
            }
            .to_markdown();
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                body, &uri,
            )]));
        }

        Err(McpError::UnknownResource(uri).into())
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router(router = tool_router)]
impl NudoxMcpServer {
    /// Find symbols by name, kind or concept — structural and semantic
    /// ranking in one call.
    #[tool(
        name = "search",
        description = "Find symbols by name, kind, or concept. Each hit has a key and usually an \
address for `read` or `refs`. Fields, variants, and params are omitted unless `kinds` names them. \
Filter with `packages` (`ecosystem:name`). Use `graph` for implementors and similar edges."
    )]
    pub async fn search(
        &self,
        Parameters(args): Parameters<SearchSymbolsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_unified_search(args)).await
    }

    /// Read one or more symbols through the compact source-first projection.
    #[tool(
        name = "read",
        description = "Read 1-32 symbols in one call. Default `format=source` returns the declaration \
text when a real span exists, otherwise the signature. `format=signature` skips source."
    )]
    pub async fn read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_read(args)).await
    }

    /// Find what references a symbol, or what a symbol's own body references.
    #[tool(
        name = "refs",
        description = "Who references this symbol (`direction: in`, default) or what its body \
references (`direction: out`). Comments are not references. `key` may be a key or an address."
    )]
    pub async fn refs(
        &self,
        Parameters(args): Parameters<RefsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_refs(args)).await
    }

    /// List loaded packages and their loaded versions.
    #[tool(
        name = "packages",
        description = "List loaded packages, versions, and which version is current. An unknown \
`package` returns an empty version list, not an error."
    )]
    pub async fn packages(
        &self,
        Parameters(args): Parameters<PackagesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_packages(args)).await
    }

    /// Switch which loaded generation of a package the other tools answer from.
    #[tool(
        name = "select_version",
        description = "Make one loaded version current for later calls. Unknown versions return \
`NotLoaded`, not an error. A key that then disappears may be stale — search again."
    )]
    pub async fn select_version(
        &self,
        Parameters(args): Parameters<SelectVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_select_version(args)).await
    }

    /// Compare two loaded generations of one package.
    #[tool(
        name = "diff",
        description = "Diff two loaded versions. `removed` is the only verdict that means deleted. \
`rekeyed` means use `to_key`. `indeterminate` means search by name before calling it a deletion."
    )]
    pub async fn diff(
        &self,
        Parameters(args): Parameters<DiffVersionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_diff_versions(args)).await
    }

    /// Fetch, produce and index a package named by a package URL.
    #[tool(
        name = "index",
        description = "Index package URLs (`pkg:cargo/name@version`, also npm, pypi, golang, maven, \
nuget) or local manifest directories. Slow. Read `indexed`, `running`, and `failed` separately. \
`dependencies: true` walks one level unless `depth` says otherwise."
    )]
    pub async fn index(
        &self,
        Parameters(args): Parameters<IndexArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_index(args)).await
    }

    /// Run an arbitrary Trustfall query over the corpus.
    #[tool(
        name = "graph",
        description = "Trustfall query for edges the other tools cannot express (`returnedBy`, \
`implementors`, `subtypes`). Call `schema` first. An empty page names the reason and the edge to \
try instead. A guessed field name is an error, not an empty page."
    )]
    pub async fn graph(
        &self,
        Parameters(args): Parameters<GraphQueryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_graph_query(args)).await
    }

    /// Return the reference for what `graph` is checked against.
    #[tool(
        name = "schema",
        description = "Compact graph reference card: types, edges, and one example. Once per \
session. `full: true` returns the SDL."
    )]
    pub async fn schema(
        &self,
        Parameters(args): Parameters<GraphSchemaArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Metered like every other tool. It answers from a constant rather
        // than from the engine, but `docs/auth.md` bills a `tool_call`, and an agent
        // cannot tell which of our tools happen to be cheap for us to serve.
        // Exempting it would put a hole in the meter whose size is set by how
        // often agents call `schema`, which is once per session and
        // therefore not small.
        self.metered(async {
            Ok(if args.full {
                SchemaResult {
                    schema: crate::mcp::SCHEMA_SDL.to_owned(),
                    full: true,
                }
            } else {
                SchemaResult {
                    schema: crate::mcp::SCHEMA_CARD.to_owned(),
                    full: false,
                }
            })
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NudoxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::LATEST)
        .with_server_info(Implementation::from_build_env())
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_resources_snapshot().await
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_resource_uri(request.uri).await
    }
}
