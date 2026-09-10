//! Defines tools behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the tools invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The nine registry rows projected as MCP tools, and the instructions sent once at handshake.
//!
//! A tool's description is the registry sentence verbatim plus exactly one sentence of guidance
//! that only this surface needs: when to reach for the tool, and what its result will not tell you.
//! Nothing here restates the registry in different words, because the CLI and the GUI publish the
//! same rows and a reader who learns one has learned all three.

use interface_library::{COMMANDS, CommandId};
use interface_protocol::mcp::ToolDescriptor;
use serde_json::{Value, json};

/// Sent once, in the `initialize` result, in place of a paragraph in every tool description.
pub const INSTRUCTIONS: &str = "Local code intelligence over the packages on this shelf. \
Every record leads with an address — `cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]` — \
that you pass back verbatim; the 8-hex beside a row is a fingerprint, never input. \
`packages` first: it is the only honest answer to what you can read. \
Missing something? `add` it (`pkg:cargo/serde@1.0.196` or `cargo:serde@1.0.196`) — it runs a compiler, \
so it is slow; use it for absence, not browsing. \
Then `search` (one query, four lanes, each reporting its own coverage) and `show` one address. \
`graph` answers who calls, implements, references. \
Ambiguity returns numbered candidates from `resolve` — pick one, pass it back. \
`outline` maps a package; `health` names a missing capability. \
Faults carry the exact operand.";

/// The one sentence this surface adds to a registry description.
const fn guidance(id: CommandId) -> &'static str {
    match id {
        CommandId::Packages => {
            "Call this first; an absent coordinate means not indexed here, never does not exist."
        }
        CommandId::Add => {
            "Reach for this only when packages lacks something you need: it runs a compiler front \
             end and is far slower than every other tool."
        }
        CommandId::Remove => {
            "Drop a package added by mistake or free its indexes; a package another process is \
             compiling is left alone, not failed."
        }
        CommandId::Show => {
            "Prefer this the moment you hold an address or key: one page carries signature, docs, \
             members and relations, so it usually replaces three graph calls."
        }
        CommandId::Outline => {
            "Use this to learn a package's shape when you do not yet know a name worth searching for."
        }
        CommandId::Resolve => {
            "Call this when show reports ambiguity or you typed a path from memory; it returns \
             numbered candidates and never picks between them."
        }
        CommandId::Search => {
            "Prefer this first for a symbol by name, kind or concept; read the ~lanes line before \
             concluding nothing exists, because a lane that could not run also returns zero rows."
        }
        CommandId::Graph => {
            "The relation tool (who calls this, what implements this); read nudox://schema-card \
             once for link kinds, directions and depth limits."
        }
        CommandId::Health => {
            "Call this when a lane says unavailable or an add is refused, to learn whether the \
             capability is unconfigured, unreachable, or detached from this process."
        }
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn input_schema(id: CommandId) -> Value {
    match id {
        CommandId::Packages | CommandId::Health => object_schema(json!({}), &[]),
        CommandId::Add => object_schema(
            json!({
                "package": {
                    "type": "string",
                    "description": "Pinned package, as `pkg:cargo/serde@1.0.196` or `cargo:serde@1.0.196`.",
                },
            }),
            &["package"],
        ),
        CommandId::Remove | CommandId::Outline => object_schema(
            json!({
                "package": {
                    "type": "string",
                    "description": "Pinned coordinate, as `cargo:serde@1.0.196`.",
                },
            }),
            &["package"],
        ),
        CommandId::Show => object_schema(
            json!({
                "address": {
                    "type": "string",
                    "description": "An address copied verbatim from any result, or a full 65-character key.",
                },
            }),
            &["address"],
        ),
        CommandId::Resolve => object_schema(
            json!({
                "text": {
                    "type": "string",
                    "description": "An address, possibly partial or remembered, to turn into exact candidates.",
                },
            }),
            &["text"],
        ),
        CommandId::Search => search_schema(),
        CommandId::Graph => graph_schema(),
    }
}

fn search_schema() -> Value {
    object_schema(
        json!({
            "query": { "type": "string", "description": "What to look for: a name, a fragment, or a concept." },
            "kinds": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Declaration kinds to admit, as `fn` `struct` `trait` `mod` `enum` `type` `const` `static` `impl` `macro` `use` `field` `variant` `param` `ns`. Fields, variants and parameters are excluded unless you name them.",
            },
            "packages": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Restrict to these pinned coordinates; omit to search every loaded package.",
            },
            "lanes": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Lanes to consult, as `exact` `names` `graph` `semantic`; omit for all four.",
            },
            "limit": { "type": "integer", "description": "Page size, 1 to 200, default 25." },
            "cursor": { "type": "integer", "description": "Continuation echoed by a truncated result." },
        }),
        &["query"],
    )
}

fn graph_schema() -> Value {
    object_schema(
        json!({
            "address": { "type": "string", "description": "Address or full key of the declaration to traverse from." },
            "relation": {
                "type": "string",
                "description": "One relation spelling, which carries its own direction (`calls` versus `called-by`). Omit to traverse every kind. See nudox://schema-card.",
            },
            "depth": { "type": "integer", "description": "Hops, 1 to 4, default 1." },
            "limit": { "type": "integer", "description": "Edge budget, 1 to 200, default 25." },
        }),
        &["address"],
    )
}

/// Every tool row `tools/list` publishes, in registry order.
#[must_use]
pub fn descriptors() -> Vec<ToolDescriptor> {
    COMMANDS
        .into_iter()
        .map(|row| ToolDescriptor {
            name: row.name,
            description: format!("{} {}", row.description, guidance(row.id)),
            input_schema: input_schema(row.id),
        })
        .collect()
}

/// Finds the registry row one tool name projects.
#[must_use]
pub fn command_id(name: &str) -> Option<CommandId> {
    interface_library::spec_named(name).map(|row| row.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registry_row_is_published_with_its_own_sentence_kept_whole() {
        let rows = descriptors();
        assert_eq!(rows.len(), COMMANDS.len());
        for (row, spec) in rows.iter().zip(COMMANDS) {
            assert_eq!(row.name, spec.name);
            assert!(
                row.description.starts_with(spec.description),
                "{} must lead with the registry sentence verbatim",
                spec.name
            );
            assert!(row.description.ends_with('.'));
            assert_eq!(row.input_schema.get("type"), Some(&json!("object")));
            assert_eq!(row.input_schema.get("additionalProperties"), Some(&json!(false)));
        }
    }
}
