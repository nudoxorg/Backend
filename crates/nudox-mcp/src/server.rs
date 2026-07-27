//! The `rmcp` server: tool dispatch plus the §L6 resource set.
//!
//! # Resources
//!
//! §L6 asks for "the schema, and one resource per loaded package". Both are
//! served here:
//!
//! * `nudox://schema` — the Trustfall SDL, identical to what `graph_schema`
//!   returns. It is exposed as a *resource* as well as a tool because MCP
//!   clients attach resources to a conversation up front, which is exactly when
//!   an agent needs the schema — before it writes its first `graph_query`.
//! * `nudox://package/{ecosystem}:{name}` — one per loaded package, listed
//!   dynamically from the corpus so the set tracks what is actually loaded.
//!
//! The package list is resolved through [`NudoxTools::do_list_packages`], so
//! resources bottom out in the same engine call the `list_packages` tool does
//! (LR-8). There is no second path to the corpus.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    ListResourcesResult, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, tool_handler};

use crate::error::McpError;
use crate::tools::NudoxTools;

/// URI of the schema resource.
pub const SCHEMA_URI: &str = "nudox://schema";

/// URI prefix of the per-package resources.
pub const PACKAGE_URI_PREFIX: &str = "nudox://package/";

/// Instructions surfaced to the MCP client at initialisation.
///
/// This is the one place an agent is told how the tools relate to each other
/// before it has called any of them, so it says which to reach for first.
const INSTRUCTIONS: &str = "\
Local documentation and code-intelligence for the packages loaded in this \
nudox workspace. All answers come from IR produced on this machine or verified \
against a remote generation; every result carries a `provenance` field saying \
which.

Start with `list_packages` to see what is loaded, then `search_symbols` to \
find a symbol by name, then `get_symbol` to read it in full. Use `find_usages` \
to find a symbol's callers. Every one of these speaks the same key format, \
`ecosystem:name#introhex`, so a key from one tool goes straight into another.

Reach for `graph_query` only when those four cannot express the question — it \
is an arbitrary Trustfall query over the corpus, and you must call \
`graph_schema` first to learn the exact type, property and edge names.";

/// The MCP server `lindsey` hosts.
///
/// A thin shell over [`NudoxTools`]: this type owns the `ServerHandler`
/// obligations (server info, resources) and delegates every tool call to the
/// router.
#[derive(Clone, Debug)]
pub struct NudoxMcpServer {
    /// The engine-backed tool set (LR-8).
    tools: NudoxTools,
}

impl NudoxMcpServer {
    /// Build a server over an already-started engine.
    pub fn new(engine: nudox_engine::EngineHandle) -> Self {
        Self { tools: NudoxTools::new(engine) }
    }

    /// Build a server over an existing tool set.
    pub fn from_tools(tools: NudoxTools) -> Self {
        Self { tools }
    }

    /// The tool set this server exposes.
    pub fn tools(&self) -> &NudoxTools {
        &self.tools
    }

    /// The route table, for `#[tool_handler]`.
    fn tool_router(&self) -> &ToolRouter<NudoxTools> {
        &self.tools.tool_router
    }

    /// Every resource this server serves right now.
    ///
    /// Recomputed per call rather than cached: packages load asynchronously
    /// after start-up (LR-10 paints before data arrives), so a cached list
    /// would report an empty workspace forever if a client happened to connect
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
                    "Package {} from the {} ecosystem. Its lineage key `{}` is the prefix of \
every symbol key it declares, and is what `search_symbols`'s `packages` filter accepts.",
                    pkg.name, pkg.ecosystem, pkg.lineage
                ))
                .with_mime_type("application/json"),
            );
        }

        Ok(resources)
    }
}

#[tool_handler(router = self.tool_router())]
impl ServerHandler for NudoxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            server_info: rmcp::model::Implementation::from_build_env(),
            instructions: Some(INSTRUCTIONS.to_owned()),
            meta: None,
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = self.resources().await?;
        Ok(ListResourcesResult { resources, next_cursor: None, meta: None })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = request.uri;

        if uri == SCHEMA_URI {
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(crate::SCHEMA_SDL, &uri)],
                meta: None,
            });
        }

        if let Some(lineage) = uri.strip_prefix(PACKAGE_URI_PREFIX) {
            let packages = self.tools.do_list_packages().await.map_err(McpError::from_engine)?;
            let found = packages
                .packages
                .into_iter()
                .find(|p| p.lineage == lineage)
                .ok_or_else(|| McpError::UnknownResource(uri.clone()))?;
            let body = serde_json::to_string_pretty(&found).map_err(|e| {
                ErrorData::internal_error(format!("failed to encode package resource: {e}"), None)
            })?;
            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(body, &uri)],
                meta: None,
            });
        }

        Err(McpError::UnknownResource(uri).into())
    }
}
