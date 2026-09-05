//! The one closed MCP tool registry over the shared raw application command grammar.
//!
//! Tool calls inject their action only here.  CLI and the compatibility application tool retain
//! the same `RawApplicationCommand` decoder, so no surface owns a second command DTO.

use serde::{Deserialize as _, Serialize};
use serde_json::{Map, Value};

use crate::{AdapterError, AdapterErrorCode, AdapterField, command::RawApplicationCommand};

/// One stable MCP tool row and the sole raw action it injects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpTool {
    /// Stable MCP name.
    pub name: &'static str,
    /// Shared raw application action.
    pub action: &'static str,
    /// Concise description projected to MCP clients.
    pub description: &'static str,
}

/// Closed registry in deterministic client-visible order.
pub const MCP_TOOLS: [McpTool; 14] = [
    McpTool {
        name: "generate",
        action: "generate",
        description: "Compile one bounded source with its selected profile and stage.",
    },
    McpTool {
        name: "compile-package",
        action: "compile-package",
        description: "Resolve and compile one pinned package URL through its matching language profile.",
    },
    McpTool {
        name: "snapshot-status",
        action: "status",
        description: "Inspect one retrieval snapshot's residency facts.",
    },
    McpTool {
        name: "search",
        action: "search",
        description: "Search one configured retrieval snapshot.",
    },
    McpTool {
        name: "locality",
        action: "locality",
        description: "Inspect local residency for one snapshot.",
    },
    McpTool {
        name: "remove-index",
        action: "remove-index",
        description: "Idempotently unload one configured retrieval snapshot.",
    },
    McpTool {
        name: "graph",
        action: "graph",
        description: "Request graph retrieval through the configured capability.",
    },
    McpTool {
        name: "vector",
        action: "vector",
        description: "Request vector retrieval through the configured capability.",
    },
    McpTool {
        name: "health",
        action: "health",
        description: "Inspect typed capability health.",
    },
    McpTool {
        name: "recover-local",
        action: "recover-local",
        description: "Request one typed local recovery decision.",
    },
    McpTool {
        name: "recover-inconsistent",
        action: "recover-inconsistent",
        description: "Request recovery after a typed remote authority mismatch.",
    },
    McpTool {
        name: "release-local",
        action: "release-local",
        description: "Request one typed local release decision.",
    },
    McpTool {
        name: "poll-execution",
        action: "poll-execution",
        description: "Poll one service-owned operation.",
    },
    McpTool {
        name: "cancel",
        action: "cancel",
        description: "Cancel one service-owned operation.",
    },
];

/// Finds one registry row by its exact MCP name.
#[must_use]
pub(crate) fn find(name: &str) -> Option<McpTool> {
    MCP_TOOLS.iter().copied().find(|tool| tool.name == name)
}

/// Injects the registry-owned action into one object of shared raw arguments.
///
/// # Errors
///
/// The caller must provide an object and must not attempt to override the registry's action.
pub(crate) fn inject(
    tool: McpTool,
    arguments: Option<Value>,
) -> Result<RawApplicationCommand, AdapterError> {
    let mut arguments = arguments.unwrap_or_else(|| Value::Object(Map::new()));
    let Some(object) = arguments.as_object_mut() else {
        return Err(AdapterError::simple(
            AdapterErrorCode::InvalidShape,
            AdapterField::Arguments,
        ));
    };
    if object.contains_key("action") {
        return Err(AdapterError::simple(
            AdapterErrorCode::TooManyFields,
            AdapterField::Action,
        ));
    }
    object.insert("action".to_owned(), Value::String(tool.action.to_owned()));
    RawApplicationCommand::deserialize(arguments)
        .map_err(|source| AdapterError::invalid_json(AdapterField::Arguments, source))
}

/// Fixed lifecycle instruction fact, intentionally limited to protocol behavior.
pub const MCP_INSTRUCTIONS: &str = "Use tools/list to inspect the typed application commands. Tool results retain their exact application outcome or diagnostic.";

/// The supported MCP protocol revision.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Serialize)]
/// Serializable fixed MCP handshake facts.
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: &'static str,
    capabilities: InitializeCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
    instructions: &'static str,
}

#[derive(Serialize)]
struct InitializeCapabilities {
    tools: ToolsCapability,
}

#[derive(Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Serialize)]
struct ServerInfo {
    name: &'static str,
    version: &'static str,
}

/// Builds the immutable initialize result.
#[must_use]
pub(crate) const fn initialize_result() -> InitializeResult {
    InitializeResult {
        protocol_version: MCP_PROTOCOL_VERSION,
        capabilities: InitializeCapabilities {
            tools: ToolsCapability {
                list_changed: false,
            },
        },
        server_info: ServerInfo {
            name: "nudox",
            version: env!("CARGO_PKG_VERSION"),
        },
        instructions: MCP_INSTRUCTIONS,
    }
}

#[derive(Serialize)]
/// Serializable registry listing assembled only at the transport encoding edge.
pub struct ToolsResult {
    tools: Vec<ToolWire>,
}

#[derive(Serialize)]
struct ToolWire {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: InputSchema,
}

#[derive(Serialize)]
struct InputSchema {
    #[serde(rename = "type")]
    schema_type: &'static str,
}

/// Builds the transport-owned measured tool list vector at the JSON serialization edge.
#[must_use]
pub(crate) fn tools_result() -> ToolsResult {
    ToolsResult {
        tools: MCP_TOOLS
            .iter()
            .map(|tool| ToolWire {
                name: tool.name,
                description: tool.description,
                input_schema: InputSchema {
                    schema_type: "object",
                },
            })
            .collect(),
    }
}
