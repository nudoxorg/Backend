//! Resources: the things an agent reads once and keeps, rendered as Markdown.
//!
//! A resource is not a tool result. An agent attaches it to its context and
//! re-reads it, so it has to be worth its tokens every time — which ruled out
//! what this surface used to serve, a pretty-printed wire reply full of
//! certificate claims and identity digests. Every resource here is the same
//! Markdown the matching tool returns, produced by the same shared renderer.
//!
//! One resource is not a projection of a reply at all: `backend://schema/query`
//! is the card for the typed Trustfall lane. It carries the whole schema and a
//! handful of worked queries, and the queries are fenced so that a test can
//! lift them straight out of the card and execute them against a live index —
//! the card cannot promise a query the engine will not run.

use super::codec::{empty_cursor, object, percent_decode, percent_encode, string};
use super::{RpcError, answer_for};
use backend_present::{
    Answer, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Engine, Request, encode_serializable, markdown,
    oversized_fault,
};
use serde::Serialize;
use serde_json::{Value, json};

const WORKSPACE_URI: &str = "backend://workspace/current";
const SCHEMA_URI: &str = "backend://schema/query";
const MARKDOWN: &str = "text/markdown";

/// The card an agent reads before writing its first Trustfall query.
pub(super) const QUERY_SCHEMA_CARD: &str = r#"# backend.query

One typed query language over the current immutable revision. Use it when the
question is a join rather than a lookup: every declaration a module contains,
every neighbour of a name, every project on the shelf with its declarations.
For a single declaration, `backend.document` is cheaper and more complete.

Rows come back under `structuredContent.rows`, and `terminal` says whether the
page is `complete`, hit its `limit_reached` bound, or was `cancelled`.

## starting edges

`Project` — one row per package on the shelf.
`Declaration` — one row per declaration in the revision.
`ExternalTarget` — one row per declaration resolved into another package.
`Item` — every row, of any kind.

## fields on a row

`id`, `kind`, `coordinate`, `name`, `signature`, `documentation`, `score`,
`state`, `revision`, `profile`, `packageUrl`.

`coordinate` is the exact string every other tool accepts, so a query is a way
to *find* coordinates and `backend.document` is the way to read them.

## edges from a row

`project`, `parent`, `children`, `related`, `referencedBy`, `sameProject`.
`related` walks outward. A call, a type reference, an import, or a field read
of a declaration in this same project lands on that declaration, including
another file. Variable reads do not. `referencedBy` is that edge reversed, so
the declaration names its callers and mention sites across files. A stable reference
into another publication stays an external node and is not joined. Any other
foreign target stays an external node; when the compiler recorded a display
spelling, that spelling is the node's name. An edge that may be
empty needs `@optional`, or its row is dropped.

## worked queries

Every declaration with its kind and signature:

```graphql
{
  Declaration {
    coordinate @output(name: "at")
    name @output(name: "name")
    kind @output(name: "kind")
    signature @output(name: "signature")
  }
}
```

Every project on the shelf:

```graphql
{
  Project {
    coordinate @output(name: "root")
    name @output(name: "project")
    state @output(name: "state")
  }
}
```

Each declaration and what it is nested inside:

```graphql
{
  Declaration {
    coordinate @output(name: "at")
    parent @optional {
      name @output(name: "parent")
      kind @output(name: "parentKind")
    }
  }
}
```

Who in this package references one named declaration:

```graphql
{
  Declaration {
    name @filter(op: "=", value: ["$name"])
    referencedBy @optional {
      name @output(name: "caller")
      coordinate @output(name: "callerAt")
      kind @output(name: "callerKind")
    }
  }
}
```

Each declaration and its neighbours in the graph:

```graphql
{
  Declaration {
    name @output(name: "declaration")
    related @optional {
      name @output(name: "neighbour")
      coordinate @output(name: "neighbourAt")
    }
  }
}
```

## bounds

`limit` is 1 to 200 and defaults to 25. A query that reaches its bound says
`limit_reached` and returns an opaque `nextCursor`; pass that cursor to resume
the same immutable query rather than restarting at page one.
"#;

/// Lifts every fenced `graphql` block out of the schema card.
#[cfg(test)]
pub(super) fn worked_queries(card: &str) -> Vec<&str> {
    let mut queries = Vec::new();
    let mut rest = card;
    while let Some((_, after)) = rest.split_once("```graphql\n") {
        let Some((query, remainder)) = after.split_once("\n```") else {
            break;
        };
        queries.push(query);
        rest = remainder;
    }
    queries
}

/// Lists the workspace status, the query card, and one outline per project.
pub(super) fn list_resources(engine: &mut dyn Engine, params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    let mut resources = vec![
        json!({
            "uri": WORKSPACE_URI,
            "name": "workspace-status",
            "title": "Workspace status",
            "description": "Readiness, lane coverage, and the capability rollup in a few lines.",
            "mimeType": MARKDOWN
        }),
        json!({
            "uri": SCHEMA_URI,
            "name": "query-schema",
            "title": "backend.query schema",
            "description": "Starting edges, fields, and worked Trustfall queries that run unchanged.",
            "mimeType": MARKDOWN
        }),
    ];
    let Answer::Shelf(shelf) = answer_for(engine, &Request::Shelf)? else {
        return encode_result("resources", ResourceBody { resources });
    };
    resources.extend(shelf.entries().iter().map(|entry| {
        let coordinate = entry.identity().coordinate().as_str();
        json!({
            "uri": format!("backend://outline/{}", percent_encode(coordinate)),
            "name": entry.identity().name(),
            "title": format!("{} outline", entry.identity().name()),
            "description": "The package's containment tree, named and pinned to this revision.",
            "mimeType": MARKDOWN
        })
    }));
    encode_result("resources", ResourceBody { resources })
}

/// Lists the two addressable resource families.
pub(super) fn list_resource_templates(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    encode_result(
        "resource-templates",
        json!({ "resourceTemplates": [
        {
            "uriTemplate": "backend://document/{coordinate}",
            "name": "declaration-document",
            "title": "Declaration document",
            "description": "One declaration page selected by an exact coordinate.",
            "mimeType": MARKDOWN
        },
        {
            "uriTemplate": "backend://outline/{path}",
            "name": "project-outline",
            "title": "Project outline",
            "description": "One package outline selected by project path.",
            "mimeType": MARKDOWN
        }
    ] }),
    )
}

/// Reads one resource as the same Markdown its tool returns.
pub(super) fn read_resource(engine: &mut dyn Engine, params: &Value) -> Result<Value, RpcError> {
    let params = object(params)?;
    let uri = string(params, "uri")?;
    let text = if uri == SCHEMA_URI {
        QUERY_SCHEMA_CARD.to_owned()
    } else {
        markdown::answer(&answer_for(engine, &request_for(uri)?)?)
    };
    encode_result(
        "resource",
        json!({
            "contents": [{ "uri": uri, "mimeType": MARKDOWN, "text": text }]
        }),
    )
}

/// Applies the same bounded typed envelope as tool projections to MCP
/// resources. Resource text is context, so an oversized resource is refused
/// with a typed transport fault instead of being silently truncated.
fn encode_result<T: Serialize>(answer: &'static str, body: T) -> Result<Value, RpcError> {
    let payload = encode_serializable(
        answer,
        Detail::Summary,
        None,
        body,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
    serde_json::from_slice(&payload.bytes).map_err(|error| {
        RpcError::tool(format!("typed resource projection decode failed: {error}"))
    })
}

#[derive(Serialize)]
struct ResourceBody {
    resources: Vec<Value>,
}

fn request_for(uri: &str) -> Result<Request, RpcError> {
    if uri == WORKSPACE_URI {
        return Ok(Request::Status);
    }
    if let Some(coordinate) = uri.strip_prefix("backend://document/") {
        return percent_decode(coordinate).map(Request::Page);
    }
    if let Some(path) = uri.strip_prefix("backend://outline/") {
        return percent_decode(path).map(Request::Outline);
    }
    Err(RpcError::new(-32002, "Resource not found"))
}
