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
//! can read. Each one therefore says what the tool returns, when to prefer it
//! over the others, and what a `SymbolKey` looks like. The typed tools
//! (`search_symbols`, `semantic_search`, `get_symbol`, `find_usages`, `get_occurrences`, `list_packages`,
//! `list_versions`, `select_version`) announce themselves as the first choice
//! and `graph_query` announces itself as the escape hatch, per §L6.
//!
//! # Resources
//!
//! §L6 asks for "the schema, and one resource per loaded package":
//!
//! * `nudox://schema` — the Trustfall SDL, identical to what `graph_schema`
//!   returns. It is a resource *as well as* a tool because MCP clients attach
//!   resources up front, which is exactly when an agent needs the schema —
//!   before it writes its first `graph_query`.
//! * `nudox://package/{ecosystem}:{name}` — one per loaded package, resolved
//!   through the same engine call `list_packages` uses (LR-8). There is no
//!   second path to the corpus.

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
use crate::mcp::tools::{
    DiffVersionsArgs, FindUsagesArgs, GetOccurrencesArgs, GetSymbolArgs, GetSymbolsArgs,
    GraphQueryArgs, GraphSchemaArgs, IndexPackageArgs, ListPackagesArgs, ListVersionsArgs,
    NudoxTools, PackagesResult, SchemaResult, SearchSymbolsArgs, SelectVersionArgs,
    SemanticSearchArgs, SymbolFormat,
};
use crate::mcp::result_format::MarkdownResult;

/// URI of the schema resource.
pub const SCHEMA_URI: &str = "nudox://schema";

/// URI prefix of the per-package resources.
pub const PACKAGE_URI_PREFIX: &str = "nudox://package/";

/// Instructions surfaced to the MCP client at initialisation.
///
/// The one place an agent is told how the tools relate to each other before it
/// has called any of them, so it says which to reach for first.
const INSTRUCTIONS: &str = "\
Local documentation and code intelligence for the packages loaded in this nudox \
workspace. Tool results are compact Markdown: a declaration's rendered signature \
is its primary identity, exact source is fenced verbatim, and tables are reserved \
for repeated relationships. Every stable key is reusable across the tools.

Start with `list_packages` to see what is loaded, then `search_symbols` to find \
a symbol by name, or `semantic_search` for a concept/behavior. Use `get_symbols` to read one or \
more without repeated context round trips. Use `find_usages` to find symbols referring to a \
target, and `get_occurrences` for exact owner-relative references inside one symbol. All these \
tools speak the same key format, \
`ecosystem:name#introhex`, so a key returned by one goes straight into another. \
`get_symbol` is source-first and compact; use `list_versions` and \
`diff_versions` for package history.

If `list_packages` does not list a package you need, the corpus is not fixed: \
`index_package` takes a package URL (`pkg:cargo/serde@1.0.196`, \
`pkg:npm/left-pad@1.3.0`, `pkg:maven/com.google.guava/guava@33.0.0-jre`) and \
fetches, verifies and indexes it on demand. It is much slower than every other \
tool here — it downloads sources and runs a compiler front end — so reach for \
it when you genuinely need a package that is absent, not to explore.

When a package has more than one version loaded, use `list_versions` to see \
them and `select_version` to switch which one the other tools answer from. \
Most `SymbolKey`s survive a version switch, but not all — read \
`select_version`'s description before relying on one across a switch.

Reach for `graph_query` only when those tools cannot express the question. It \
runs an arbitrary Trustfall query over the corpus, and you must call \
`graph_schema` first to learn the exact type, property and edge names.";

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
                    "The GraphQL SDL that `graph_query` queries are validated against. \
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
symbol key it declares, and is what `search_symbols`'s `packages` filter accepts.",
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
    /// Find symbols by name or kind across the loaded corpus.
    #[tool(
        name = "search_symbols",
        description = "PREFER THIS FIRST when looking for a symbol by name. Searches the \
loaded local corpus by symbol name and by kind, returning ranked Markdown hits — each with its \
stable key and complete rendered declaration signature. A key looks like \
`ecosystem:name#introhex`, for example \
`cargo:serde#3f1a…` with 64 hex characters after the `#`; pass it straight to `get_symbol` to \
read the symbol or to `find_usages` to find its callers. Narrow with `kinds` (Function, Record, \
Trait, Enum, Impl, Alias, Field, Const, Static, Module, Variant, Reexport, Param) and with \
`packages` (each `ecosystem:name`; see `list_packages`). This matches names and kinds, not \
documentation prose — for structural questions such as \"which types implement this trait\" or \
\"what does this function reference\", use `graph_query`. Results are paginated: if `next_cursor` \
is present in the response, more results exist — pass it back as `cursor` to get the next page. \
Treat `cursor` as opaque; never construct one yourself."
    )]
    pub async fn search_symbols(
        &self,
        Parameters(args): Parameters<SearchSymbolsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_search(args)).await
    }

    /// Find documented public APIs by concept or behavior.
    #[tool(
        name = "semantic_search",
        description = "Use for natural-language questions such as `retry failed requests` or `which API parses URLs`. Searches embeddings of public symbol paths, kinds and documentation. Results are ranked Markdown rows with complete declaration signatures and stable keys; documentation evidence is fenced below the relationship table. Use `get_symbol` for exact source. The status line distinguishes complete coverage, partial indexing and an unavailable model. Narrow with `kinds` or `packages`; pagination cursors are opaque. This is semantic retrieval, not a claim that the implementation body was searched."
    )]
    pub async fn semantic_search(
        &self,
        Parameters(args): Parameters<SemanticSearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_semantic_search(args)).await
    }

    /// Read one symbol through the compact source-first projection.
    #[tool(
        name = "get_symbol",
        description = "Read one symbol as a compact source-first Markdown record: stable key and path, then exact declaration source (including the body when present) in a fenced block. When source is unavailable, the complete rendered signature is fenced instead. Resolved type references, location and deprecation follow only when present; kind and visibility are derivable from the declaration and are not repeated. Prefer `get_symbols` when reading more than one key."
    )]
    pub async fn get_symbol(
        &self,
        Parameters(args): Parameters<GetSymbolArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_get_symbol_compact(args, SymbolFormat::Source))
            .await
    }

    /// Read several symbols through one compact projection.
    #[tool(
        name = "get_symbols",
        description = "PREFER THIS when reading multiple symbols. Accepts 1–32 stable keys and \
returns compact Markdown records in the same order in one MCP round trip. Each record keeps its path \
and key outside a fenced declaration/signature snippet; relationship references are tables. \
`format=signature` omits source; `format=source` includes the exact declaration and body."
    )]
    pub async fn get_symbols(
        &self,
        Parameters(args): Parameters<GetSymbolsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_get_symbols(args)).await
    }

    /// Find the symbols that reference a given symbol.
    #[tool(
        name = "find_usages",
        description = "PREFER THIS to answer \"who calls this?\" or \"what breaks if I change \
this?\". Given a symbol key, returns a relationship table of symbols holding a resolved reference \
to it — callers of a function, users of a type — with each complete declaration signature, path \
and key, so you can follow the chain \
with `get_symbol`. Only references the producer resolved with index-grade confidence or better \
are returned, so results are precise rather than textual: a name that merely appears in a comment \
or belongs to an unrelated identifier will not show up. `key` must be `ecosystem:name#introhex`. \
Paginated the same way as `search_symbols`: check `next_cursor` and pass it back as `cursor` for \
more."
    )]
    pub async fn find_usages(
        &self,
        Parameters(args): Parameters<FindUsagesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_find_usages(args)).await
    }

    /// Read exact references owned by one symbol.
    #[tool(
        name = "get_occurrences",
        description = "Read exact references contained in the symbol identified by `key`, grouped by target and owner declaration signature. Each relationship row labels bytes relative to the owner's declaration span, not an absolute file offset. The target key is retained even when its package is unloaded. Use `find_usages` for the different question of which symbols refer to a target."
    )]
    pub async fn get_occurrences(
        &self,
        Parameters(args): Parameters<GetOccurrencesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_get_occurrences(args)).await
    }

    /// List the packages currently loaded.
    #[tool(
        name = "list_packages",
        description = "PREFER THIS FIRST to discover what is available before searching. Returns \
every package loaded into the local corpus with its `ecosystem:name` lineage key, display name \
and ecosystem. The lineage key prefixes every symbol key in that package and is exactly what \
`search_symbols`'s `packages` filter accepts. If a package you expect is missing then it has not \
been produced or loaded yet and no other tool will find symbols from it — this is how you tell \
\"not indexed here\" apart from \"does not exist\"."
    )]
    pub async fn list_packages(
        &self,
        Parameters(_args): Parameters<ListPackagesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_list_packages()).await
    }

    /// List every loaded generation of a package.
    #[tool(
        name = "list_versions",
        description = "List every loaded generation of one package, newest first, with which one \
is current and its symbol count. Several real packages in this corpus have more than one version \
loaded at once (for example memchr, pydantic, guava, jackson-databind) — call this to see them \
before assuming there is only one. `package` is `ecosystem:name` (see `list_packages`). An unloaded \
package returns zero versions rather than an error. HONESTY NOTE: most symbol keys are stable \
across the versions this lists, but not all are — some declarations mint a key that depends on \
byte offsets or emission order and does not survive every release. This tool cannot tell you which \
ones in a given package are affected; see `select_version`'s description before switching."
    )]
    pub async fn list_versions(
        &self,
        Parameters(args): Parameters<ListVersionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_list_versions(args)).await
    }

    /// Switch which loaded generation of a package the other tools answer from.
    #[tool(
        name = "select_version",
        description = "Switch which loaded generation of a package `get_symbol`, `search_symbols` \
and `graph_query` answer from. `package` is `ecosystem:name`; `version` must be one of the strings \
`list_versions` returned — call that first. Returns `Switched` (with the new symbol count) or, if \
the version is not loaded, `NotLoaded` — not an error, since the engine only holds what it was \
asked to load. By the time you receive this tool's response the switch has actually landed, so \
subsequent calls answer from the new generation. HONESTY NOTE, read before relying on this across a \
switch: most `SymbolKey`s survive a version change unchanged, because their identity is derived \
from the declaration's content — but not every declaration's key is content-derived. Some fall back \
to an identity keyed on byte offset or emission order during lowering, and that kind of key can \
silently stop resolving after a switch — indistinguishable from the symbol having been deleted, \
because this tool has no way to tell you in advance which declarations in a package are affected. \
If a key you held before switching returns `symbol not found` afterward, treat it as possibly-stale \
and re-fetch it with `search_symbols` rather than concluding the symbol was removed."
    )]
    pub async fn select_version(
        &self,
        Parameters(args): Parameters<SelectVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_select_version(args)).await
    }

    /// Compare two loaded generations of one package.
    #[tool(
        name = "diff_versions",
        description = "Compare two loaded generations of one package and get back what changed, \
declaration by declaration — the question `select_version` could previously only answer by \
switching, querying, switching back and diffing by hand. `package` is `ecosystem:name`; \
`from_version` is the OLDER and `to_version` the NEWER, exactly as `list_versions` spells them \
(call it first; several packages here have two generations loaded). If either is not loaded you \
get `not_loaded` with the list of ones that are, not an error. READ THE VERDICT ON EACH ROW: \
`removed` is the ONLY value that means a declaration is gone, and it is emitted only when the \
symbol's key was derived from its own content. `rekeyed` means one declaration got a new key \
(use `to_key`, stop using `from_key`) — a naive diff would report that as a deletion plus an \
addition, which is the single most misleading thing a diff can say. `indeterminate` means it \
vanished but its key was of a kind that can move on its own, so the tool refuses to guess: \
re-search by name and path before concluding it was deleted. Rows are paginated like \
`search_symbols`; the counts describe the whole diff on every page."
    )]
    pub async fn diff_versions(
        &self,
        Parameters(args): Parameters<DiffVersionsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_diff_versions(args)).await
    }

    /// Fetch, produce and index a package named by a package URL.
    #[tool(
        name = "index_package",
        description = "Add a package to this corpus on demand, by package URL, and then read it \
with the other tools. USE THIS when `list_packages` does not list a dependency you need to \
understand — a crate you just added, a transitive dependency whose API you are checking, a library \
you are choosing between. It fetches the package's SOURCES from its own registry, verifies them, \
runs the language producer, and inserts the result into the running corpus, after which \
`search_symbols`, `get_symbol`, `find_usages` and `graph_query` answer from it exactly as they do \
for anything else. `purl` is `pkg:<type>/[<namespace>/]<name>@<version>` — types `cargo`, `npm`, \
`pypi`, `golang`, `maven`, `nuget`. The namespace is the groupId for maven \
(`pkg:maven/com.google.guava/guava@33.0.0-jre`), the @scope for npm \
(`pkg:npm/@types/node@20.11.0`), and the module path prefix for golang \
(`pkg:golang/github.com/pkg/errors@v0.9.1`); cargo, pypi and nuget names have no namespace. THE \
VERSION IS REQUIRED and must be the registry's own spelling; omit it and the error lists what is \
published. THIS IS SLOW — seconds to minutes, because it downloads and then runs a real compiler \
front end. It waits `wait_seconds` (default 120, max 600) and then returns \
`status: \"running\"` with the stage it reached; that is not a failure and not a timeout — call it \
again with the same `purl` to keep waiting, and it will attach to the same job rather than start a \
second one. READ THE `integrity` FIELD on success: \
`verified_against_published_digest: true` means the registry published a digest of those exact \
bytes and it matched, which holds for cargo, npm, pypi and maven; `false` means no digest was \
available to compare against (golang and nuget) and the documentation rests on transport trust \
alone."
    )]
    pub async fn index_package(
        &self,
        Parameters(args): Parameters<IndexPackageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_index_package(args)).await
    }

    /// Run an arbitrary Trustfall query over the corpus.
    #[tool(
        name = "graph_query",
        description = "THE ESCAPE HATCH — use it only when the typed tools cannot express the \
question. `search_symbols`, `get_symbol`, `find_usages`, `list_packages`, `list_versions` and \
`select_version` are faster, cheaper and better shaped for what they cover; come here for \
structural or relational questions they do not, such as \"every public function in package X \
returning type Y\" (the `returnedBy` edge), \"what does this type implement\" (`implementedBy`), \
\"all implementors of this trait\" (`implementors`), or a join across packages. Takes a \
Trustfall (GraphQL-subset) query plus string variable bindings referenced as `$name`, and returns \
an agent-facing Markdown relationship table. Select `signature` on symbol rows to retain type context; \
when present, the formatter suppresses derivable `name` and `kind` columns. ALWAYS call `graph_schema` \
first and write the query against the exact type, property and edge names it returns — queries are \
validated against that schema, so a guessed field name is an error rather than an empty result. \
Paginated the same way as `search_symbols`: \
check `next_cursor` and pass it back as `cursor` for more."
    )]
    pub async fn graph_query(
        &self,
        Parameters(args): Parameters<GraphQueryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.metered(self.tools.do_graph_query(args)).await
    }

    /// Return the Trustfall schema `graph_query` is checked against.
    #[tool(
        name = "graph_schema",
        description = "Returns, verbatim, the GraphQL SDL schema that `graph_query` queries are \
validated against. Call this before writing any `graph_query`: it documents every queryable type \
(Package; Symbol and its Function, Record, Trait, Impl, Enum, Field, Const and Alias \
implementors; Occurrence), every scalar property, every edge (members, parent, usages, mentions, \
implementors, occurrencesOf, and the type-reference edges implementedBy, subtypes, returnedBy, \
acceptedBy, heldBy and signatureTypes) and the cost of each traversal. It also documents the \
`ecosystem:name#introhex` key format that every other tool consumes. The schema is fixed for the \
life of the server, so one call per session is enough."
    )]
    pub async fn graph_schema(
        &self,
        Parameters(_args): Parameters<GraphSchemaArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Metered like every other tool. It answers from a constant rather
        // than from the engine, but `docs/auth.md` bills a `tool_call`, and an agent
        // cannot tell which of our tools happen to be cheap for us to serve.
        // Exempting it would put a hole in the meter whose size is set by how
        // often agents call `graph_schema`, which is once per session and
        // therefore not small.
        self.metered(async {
            Ok(SchemaResult {
                schema: crate::mcp::SCHEMA_SDL.to_owned(),
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
