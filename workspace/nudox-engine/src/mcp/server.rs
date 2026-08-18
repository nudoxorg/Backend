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
Local documentation and code intelligence for the packages loaded in this nudox \
workspace. Tool results are compact Markdown: a declaration's rendered signature \
is its primary identity, exact source is fenced verbatim, and every symbol row \
carries a stable key (`ecosystem:name#introhex`) and, where renderable, a readable \
address — either can be passed to `read` or `refs`.

Start with `packages` to see what is loaded and at what versions, then `search` \
to find a symbol by name, kind, or natural-language concept — it ranks structural \
and semantic hits together and labels which matched semantically. Use `read` for \
one or many symbols in one round trip (source by default). Use `refs` to find \
what references a symbol (`direction: in`) or what a symbol's own body references \
(`direction: out`).

Pagination is uniform across `search`, `refs`, `graph` and `diff`: when a response \
carries `next_cursor`, pass it back as `cursor` for the next page; treat it as an \
opaque token, never construct one yourself.

Key stability is not uniform: most `SymbolKey`s survive a version switch, because \
their identity is derived from the declaration's content, but a declaration that \
collided during lowering can fall back to an identity keyed on byte offset or \
emission order, which does not survive every release — indistinguishable from \
deletion when it happens. `packages` and `select_version` say more; `diff` re-pairs \
a stale key with what it became.

If `packages` does not list what you need, `index` takes a package URL \
(`pkg:cargo/serde@1.0.196`, `pkg:npm/left-pad@1.3.0`, \
`pkg:maven/com.google.guava/guava@33.0.0-jre`) and fetches, verifies and indexes it \
on demand. It is much slower than every other tool here — it downloads sources and \
runs a compiler front end — so reach for it when a package is genuinely absent, not \
to explore.

When a package has more than one version loaded, use `select_version` to switch \
which one the other tools answer from.

Reach for `graph` only when the tools above cannot express the question — an \
arbitrary Trustfall query over the corpus. Call `schema` first for the exact type, \
property and edge names.";

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
        description = "PREFER THIS FIRST when looking for a symbol. One ranked list of Markdown \
hits combining exact/prefix name-and-kind matches with semantic ranking over documentation — no \
need to choose an index up front. Each hit carries a stable key and, where renderable, an address; \
pass either straight to `read` or `refs`. A hit found only through semantic similarity is marked \
`semantic: true`. By default only top-level declarations are searched (Function, Record, Trait, \
Enum, Impl, Alias, Const, Static, Module, Reexport) — fields, enum variants and parameters are \
held back because they outnumber declarations and crowd out the API surface a first look wants; \
the response says so via `excluded_kinds`. Pass `kinds` naming any of those three (Field, Variant, \
Param) — or any subset of the full twelve — to search them specifically, e.g. `kinds: [\"Param\"]` \
to find where a parameter is used. Narrow further with `packages` (each `ecosystem:name`; see \
`packages`). For structural questions such as \"which types implement this trait\", use `graph` \
instead."
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
        description = "Read 1-32 symbols (keys or addresses) in one round trip as compact Markdown \
records: stable key and path, then exact declaration source (including the body when present) in a \
fenced block, or the rendered signature when source is unavailable — which happens for any \
declaration that exists only after macro expansion (e.g. serde's generated `Deserialize` impls for \
built-in types), where `format=source` and `format=signature` render identically with no flag \
distinguishing the two. Resolved type references, location and deprecation follow only when \
present. `format=signature` omits source for the whole batch; `format=source` (default) includes it \
when available."
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
        description = "PREFER THIS to answer \"who calls this?\" (`direction: in`, the default) or \
to read a symbol's own exact references (`direction: out`). `in` returns symbols holding a \
resolved reference to `key` — callers of a function, users of a type — with each declaration \
signature, path and key. Only references resolved with index-grade confidence or better are \
returned: a name that merely appears in a comment will not show up. `out` returns exact references \
owned by `key`'s own body, grouped by target, with byte spans relative to the owner's declaration \
start. `key` accepts either a stable key or an address."
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
        description = "PREFER THIS FIRST to discover what is loaded before searching. Omit \
`package` to list every loaded package with its `ecosystem:name` lineage key and every loaded \
version (which one is current, its symbol count). Pass `package` to narrow to one; an unloaded \
package returns one entry with an empty version list rather than an error — this is how you tell \
\"not indexed here\" apart from \"does not exist\". The lineage key is what `search`'s `packages` \
filter accepts."
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
        description = "Switch which loaded generation of a package `read`, `search` and `graph` \
answer from. `package` is `ecosystem:name`; `version` must be one of the strings `packages` \
returned. Returns `Switched` (with the new symbol count) or, if the version is not loaded, \
`NotLoaded` — not an error. By the time you receive this response the switch has landed, so \
subsequent calls answer from the new generation. If a key you held before switching returns \
`symbol not found` afterward, treat it as possibly-stale and re-search rather than concluding the \
symbol was removed."
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
        description = "Compare two loaded generations of one package, declaration by declaration. \
`package` is `ecosystem:name`; `from_version` is the OLDER and `to_version` the NEWER, exactly as \
`packages` spells them. If either is not loaded you get `not_loaded` with the versions that are, \
not an error. READ THE VERDICT ON EACH ROW: `removed` is the ONLY value that means a declaration is \
gone, emitted only when the key was derived from its own content. `rekeyed` means one declaration \
got a new key (use `to_key`, stop using `from_key`) — a naive diff would report that as a deletion \
plus an addition. `indeterminate` means it vanished but its key was of a kind that can move on its \
own, so the tool refuses to guess: re-search by name and path before concluding it was deleted."
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
        description = "Add packages to this corpus on demand, then read them with the other tools. \
USE THIS when `packages` does not list something you need to understand. Each entry in `targets` is \
EITHER a package URL OR a path to a package root on this machine — you may mix them in one call. \
A PACKAGE URL is `pkg:<type>/[<namespace>/]<name>@<version>` — types `cargo`, `npm`, `pypi`, \
`golang`, `maven`, `nuget`; the namespace is the groupId for maven \
(`pkg:maven/com.google.guava/guava@33.0.0-jre`), the @scope for npm (`pkg:npm/@types/node@20.11.0`), \
and the module path prefix for golang (`pkg:golang/github.com/pkg/errors@v0.9.1`). THE VERSION IS \
REQUIRED and must be the registry's own spelling; omit it and the error lists what is published. \
A PATH is the directory holding the manifest (`Cargo.toml`, `package.json`, `go.mod`, `pom.xml`, \
`build.gradle`, `pyproject.toml`, a `*.csproj`, `CMakeLists.txt` or `compile_commands.json`) — the \
language is read from which manifest is there, so you do not name it. USE A PATH to index the \
checkout you are working in, or a sibling package in the same monorepo. SET `dependencies: true` to \
also index what those packages declare; it is OFF by default because a dependency closure is \
unbounded, and `depth` (default 1, max 3) bounds how far it walks. Dependencies declared by path — \
a cargo `path = \"../lib\"`, an npm `\"file:../lib\"` or `\"workspace:*\"` — resolve on this \
filesystem with no registry involved. THIS IS SLOW — seconds to minutes per package, because it \
runs a real compiler front end. The `wait_seconds` deadline (default 120, max 600) covers the WHOLE \
batch; targets still going are returned under `running`, which is not a failure — call again with \
the same targets to attach to those jobs rather than start new ones. EVERY TARGET IS REPORTED \
SEPARATELY: read `indexed`, `running` and `failed`, because one bad target does not discard the \
others. In `indexed`, `requested: false` marks a package pulled in as a dependency rather than one \
you named. READ `not_scanned`: it lists dependency declarations that were NOT followed and why — \
an unread manifest format there means nobody looked, NOT that the package has no dependencies."
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
        description = "THE ESCAPE HATCH — use it only when the other tools cannot express the \
question: structural or relational questions such as \"every public function in package X \
returning type Y\" (the `returnedBy` edge), \"what does this type implement\" (`implementedBy`), \
\"all implementors of this trait\" (`implementors`), or a join across packages. Takes a Trustfall \
(GraphQL-subset) query plus string variable bindings referenced as `$name`, and returns an \
agent-facing Markdown relationship table. Select `signature` on symbol rows to retain type context; \
when present, the formatter suppresses derivable `name`/`kind` columns. ALWAYS call `schema` first \
and write the query against the exact type, property and edge names it returns — a guessed field \
name is an error, not an empty result."
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
        description = "Call this before writing any `graph` query. Returns a compact reference \
card — every queryable type, scalar and edge, the key format, worked example queries, and the \
semantic rules that make a query silently wrong if missed. Complete for writing a query; it names \
what it omits, and `full: true` returns the verbatim SDL for those cases. Fixed for the life of the \
server, so one call per session is enough."
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
