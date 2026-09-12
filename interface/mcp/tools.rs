//! Defines tools behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the tools invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The twelve registry rows projected as MCP tools, and the instructions sent once at handshake.
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
            "Prefer this first for a symbol by name, kind or concept; a lane that could not run \
             also returns zero rows, so read the ~lanes line before concluding nothing exists."
        }
        CommandId::Graph => {
            "The relation tool (who calls this, what implements this); read nudox://schema-card \
             once for link kinds, directions and depth limits."
        }
        CommandId::Health => {
            "Call this when a lane says unavailable or an add is refused, to learn whether the \
             capability is unconfigured, unreachable, or detached from this process."
        }
        CommandId::IndexSearch => {
            "Search the registry index before reaching for `add`: no compiler runs, but it only \
             knows what this machine's index observed; read its coverage line."
        }
        CommandId::PackageVersions => {
            "Use this to pin or compare versions with exact registry checksums; yanked rows are \
             marked, never hidden."
        }
        CommandId::PackageProfile => {
            "One call for the whole version history and the latest version of one registry \
             package, when `packages` is about the shelf and this is about the registry."
        }
        CommandId::Source => {
            "Read the code behind a page when the signature and docs are not enough; the excerpt \
             is bounded, so raise `context` rather than asking for a whole file."
        }
        CommandId::Related => {
            "One call for what to read next from a page: linked, sibling, and semantically close \
             declarations, each with its reason."
        }
        CommandId::Explore => {
            "Browse before adding: this reaches the live registries and needs no compiler, but \
             every page carries a provenance line that says whether it is live or cached."
        }
        CommandId::Package => {
            "The one call for everything a registry knows about a package; pass `@version` to \
             read another version's dependencies and install line."
        }
        CommandId::Dependents => {
            "Who uses this package; count and coverage differ per ecosystem and the reply says so."
        }
        CommandId::Owner => {
            "Everything one publisher ships, across ecosystems when `ecosystem` is omitted."
        }
        CommandId::Subscribe => {
            "Follow a package so `releases` reports new versions; filing into a project keeps the \
             reader's folders in sync with what you watch."
        }
        CommandId::Unsubscribe => "Stop following; the shelf is untouched.",
        CommandId::Subscriptions => {
            "What is followed and how many releases the reader has not seen."
        }
        CommandId::Releases => {
            "New versions since last seen; pass `mark_seen` only when the reader has been told."
        }
        CommandId::Projects => "The reader's folders: members and lockfile bindings.",
        CommandId::ProjectCreate => {
            "Bind a folder to a lockfile so `project-sync` keeps its members at the versions the \
             code builds with."
        }
        CommandId::ProjectDelete => "Delete a folder only; its subscriptions remain.",
        CommandId::ProjectAdd => "Put one pinned coordinate into a folder.",
        CommandId::ProjectRemove => "Take one coordinate out of a folder.",
        CommandId::ProjectSync => {
            "Reconcile a bound folder with its lockfile; `compile` also admits adds for members \
             not yet on the shelf, which runs the compiler."
        }
        CommandId::Tree => {
            "What is open across the desktop reader, the terminal, and this session; nodes you \
             open are marked as yours so the reader can find them."
        }
        CommandId::TreeOpen => {
            "Record what you are reading so the reader sees it in their tree; pass `parent` to \
             nest under the node you came from."
        }
        CommandId::TreeClose => "Close a node you opened, or a whole branch with `branch`.",
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
        CommandId::IndexSearch => index_search_schema(),
        CommandId::PackageVersions | CommandId::PackageProfile => object_schema(
            json!({
                "package": {
                    "type": "string",
                    "description": "Exact registry package name, as `serde`.",
                },
            }),
            &["package"],
        ),
        CommandId::Source => object_schema(
            json!({
                "address": { "type": "string", "description": "An address copied verbatim from any result, or a full 65-character key." },
                "context": { "type": "integer", "description": "Lines of context either side of the declaration, 0 to 200, default 12." },
            }),
            &["address"],
        ),
        CommandId::Related => object_schema(
            json!({
                "address": { "type": "string", "description": "An address copied verbatim from any result, or a full 65-character key." },
                "limit": { "type": "integer", "description": "Most rows, 1 to 200, default 25." },
            }),
            &["address"],
        ),
        CommandId::Explore => object_schema(
            json!({
                "query": { "type": "string", "description": "Free text to match; omit to browse the top of the sort." },
                "ecosystem": { "type": "string", "description": "One of `cargo` `npm` `pypi` `go` `maven` `nuget` `cpp`; omit for all." },
                "sort": { "type": "string", "description": "One of `downloads` `updated` `relevance` `name`; default downloads." },
                "page": { "type": "integer", "description": "One-based page, default 1." },
                "limit": { "type": "integer", "description": "Cards per page, 1 to 100, default 40." },
            }),
            &[],
        ),
        CommandId::Package => object_schema(
            json!({
                "package": { "type": "string", "description": "`ecosystem:name`, optionally `@version`, as `cargo:serde` or `cargo:serde@1.0.196`." },
            }),
            &["package"],
        ),
        CommandId::Dependents => object_schema(
            json!({
                "package": { "type": "string", "description": "`ecosystem:name`, as `cargo:serde`." },
                "page": { "type": "integer", "description": "One-based page, default 1." },
                "limit": { "type": "integer", "description": "Cards per page, 1 to 100, default 40." },
            }),
            &["package"],
        ),
        CommandId::Owner => object_schema(
            json!({
                "handle": { "type": "string", "description": "The owner's handle as the registry spells it." },
                "ecosystem": { "type": "string", "description": "Restrict to one ecosystem; omit to ask every registry." },
            }),
            &["handle"],
        ),
        CommandId::Subscribe => object_schema(
            json!({
                "package": { "type": "string", "description": "`ecosystem:name`, as `cargo:serde`." },
                "project": { "type": "string", "description": "A project folder name or identity to file it under." },
            }),
            &["package"],
        ),
        CommandId::Unsubscribe => object_schema(
            json!({
                "package": { "type": "string", "description": "`ecosystem:name`, as `cargo:serde`." },
            }),
            &["package"],
        ),
        CommandId::Subscriptions | CommandId::Projects | CommandId::Tree => object_schema(json!({}), &[]),
        CommandId::Releases => object_schema(
            json!({
                "mark_seen": { "type": "boolean", "description": "Record every listed release as seen." },
            }),
            &[],
        ),
        CommandId::ProjectCreate => object_schema(
            json!({
                "name": { "type": "string", "description": "Folder name, up to 64 bytes." },
                "lockfile": { "type": "string", "description": "Absolute path to a lockfile the folder tracks: Cargo.lock, package-lock.json, pnpm-lock.yaml, yarn.lock, uv.lock, poetry.lock, requirements.txt, go.mod, pom.xml, packages.lock.json, conan.lock, vcpkg.json." },
                "hue": { "type": "string", "description": "Tile hue: caramel teal forest green red olive sand." },
            }),
            &["name"],
        ),
        CommandId::ProjectDelete => object_schema(
            json!({
                "project": { "type": "string", "description": "Folder name or identity." },
            }),
            &["project"],
        ),
        CommandId::ProjectAdd | CommandId::ProjectRemove => object_schema(
            json!({
                "package": { "type": "string", "description": "Pinned coordinate, as `cargo:serde@1.0.196`." },
                "project": { "type": "string", "description": "Folder name or identity." },
            }),
            &["package", "project"],
        ),
        CommandId::ProjectSync => object_schema(
            json!({
                "project": { "type": "string", "description": "Folder name or identity." },
                "compile": { "type": "boolean", "description": "Also admit an add for every member not yet on the shelf." },
            }),
            &["project"],
        ),
        CommandId::TreeOpen => object_schema(
            json!({
                "subject": { "type": "string", "description": "A coordinate, an address, `ecosystem:name`, `@handle`, or search words." },
                "kind": { "type": "string", "description": "Force the subject kind: package page registry owner explore search." },
                "parent": { "type": "integer", "description": "Node identity to nest under, from `tree`." },
                "focus": { "type": "boolean", "description": "Make it the active node; default false so the reader's focus is not stolen." },
                "title": { "type": "string", "description": "Label override." },
            }),
            &["subject"],
        ),
        CommandId::TreeClose => object_schema(
            json!({
                "node": { "type": "integer", "description": "Node identity from `tree`." },
                "branch": { "type": "boolean", "description": "Close every descendant too." },
            }),
            &["node"],
        ),
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

fn index_search_schema() -> Value {
    object_schema(
        json!({
            "query": {
                "type": "string",
                "description": "Package or entity name to match by prefix or exact term, 1 to 128 bytes.",
            },
            "limit": { "type": "integer", "description": "Rows per page, 1 to 100, default 40." },
        }),
        &["query"],
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
