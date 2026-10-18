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

use super::codec::{empty_cursor, object, percent_decode, string};
use super::tools::{CATALOG_RESOURCE_URI, catalog_markdown, catalog_tool};
use super::wire_admission::ResponseBudget;
use super::{RpcError, answer_for};
use backend_present::{
    DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Engine, Request, encode_serializable, markdown,
    oversized_fault,
};
use serde::Serialize;
use serde_json::{Value, json};

const WORKSPACE_URI: &str = "backend://workspace/current";
const SCHEMA_URI: &str = "backend://schema/query";
const TOOLS_URI: &str = CATALOG_RESOURCE_URI;
const MARKDOWN: &str = "text/markdown";

/// The card an agent reads before writing its first Trustfall query.
pub(super) const QUERY_SCHEMA_CARD: &str = r#"# backend.query

One typed query language over the current immutable revision. Use it when the
question is a join rather than a lookup: every declaration a module contains,
every neighbour of a name, every project on the shelf with its declarations.
For a single declaration, `backend.document` is cheaper and more complete.

## tool arguments

Send this literal object as the parameters of an MCP `tools/call` request:
On a host exposing only default tools, pass the same object as the arguments
to `backend.call`.

```json
{
  "name": "backend.query",
  "arguments": {
    "query": "{ Declaration { coordinate @output(name: \"at\") name @output(name: \"name\") } }",
    "variables": {},
    "limit": 25,
    "detail": "summary"
  }
}
```

`arguments.query` is a required non-empty GraphQL string. The only optional
arguments are `variables`, `limit`, `cursor`, and `detail`; unknown fields are
refused. `variables` is a JSON object whose keys omit the `$` used in the query.
For the worked `$name` filter below, supply `{"name": "a name copied from a result"}`.
Values may be null, booleans, strings, finite numbers, or bounded nested arrays
of those values. A variable value cannot be a JSON object. Omitting `variables`
or passing null supplies no variables.

`detail` is `summary`, `standard`, or `full` and defaults to `summary`. `limit`
is an integer from 1 to 200 and defaults to 25. An initial page may select a
smaller limit to fit the complete response. Copy `structuredContent.continuation_call`
to continue with the exact query, variables, effective limit, detail and owner
cursor. Long portable proofs appear once in that executable call; small pages
also expose `nextCursor` for compatibility. A cursor from another query or
selected revision is refused.

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
another file, including when the field read's declaring type and field name
match one field this project publishes. A read of a const, static, variant, or
function this project publishes also lands on that declaration, including when the
declaring type and name match. Variable reads that are not a published const,
static, variant, or function still do not join. `referencedBy` is that edge reversed, so
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

Who references one named declaration in the selected revision:

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
`limit_reached` and returns an executable `continuation_call`; copy it to resume
the same immutable query. A resumed page retains its issued limit.
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

/// Lists stable resource addresses without opening the workspace owner.
/// Project outlines are discoverable through the URI template: a package on
/// the shelf does not prove that an outline is published at this revision.
pub(super) fn list_resources(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    let resources = vec![
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
        json!({
            "uri": TOOLS_URI,
            "name": "mcp-tool-catalog",
            "title": "MCP tool catalog",
            "description": "Registered capability-dependent route schemas and compact named-tool call guidance.",
            "mimeType": MARKDOWN
        }),
    ];
    encode_result("resources", ResourceBody { resources })
}

/// Lists addressable documents, outlines, source bytes and route schema cards.
pub(super) fn list_resource_templates(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    encode_result(
        "resource-templates",
        json!({ "resourceTemplates": [
        {
            "uriTemplate":"backend://tools/catalog/{name}{?offset,limit,version}",
            "name":"mcp-route-card", "title":"Exact MCP route cards and help",
            "description":"Read the exact closed-name URI from backend://tools/catalog. Assemble UTF-8 text in order using result._meta[backend/nextUri] and backend/catalogRange, then parse the complete JSON route card. Registered named handlers retain their argument and result schemas; direct-only backend.surface contains expert protocol guidance instead of schemas. Ranges may use text/plain for JSON fragments; application/json marks an unsplit complete card. The version binds every range to one exact descriptor; unknown names or stale versions are refused without owner work."
        },
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
            "description": "An outline selected by project path when declarations are published; otherwise resources/read returns resource-not-found.",
            "mimeType": MARKDOWN
        }
        ,{
            "uriTemplate":"backend://semantic-source/{version}{?request}",
            "name":"captured-semantic-source", "title":"Captured semantic source bytes",
            "description":"Read exact native source bytes through resources/read. URI request is canonical unpadded base64url of {reference,range:{offset,max_bytes}}; the digest path alone grants no authority. Copy result._meta[backend/nextUri] to resume. The measured response may select a smaller range, reported in result._meta[backend/semanticSourceRange]. Every page retains the same reference; admit the complete byte union and canonical digest.",
            "mimeType":"application/json",
            "_meta":{"backend/requestSchema":backend_library::SemanticSourceRequest::json_schema()}
        }
    ] }),
    )
}

/// Reads one resource as the same Markdown its tool returns.
pub(super) fn read_resource(
    engine: &mut dyn Engine,
    params: &Value,
    workspace_context: Option<&str>,
    response_id: &Value,
) -> Result<Value, RpcError> {
    let params = object(params)?;
    let uri = string(params, "uri")?;
    if let Some(selector) = uri.strip_prefix("backend://tools/catalog/") {
        let range = CatalogRead::parse(selector)?;
        let tool =
            catalog_tool(range.name).ok_or_else(|| RpcError::new(-32002, "Resource not found"))?;
        let text = serde_json::to_string(&tool)
            .map_err(|error| RpcError::tool(format!("tool schema card encode failed: {error}")))?;
        return catalog_page(uri, &text, range, response_id);
    }
    let mut text = match uri {
        SCHEMA_URI => QUERY_SCHEMA_CARD.to_owned(),
        TOOLS_URI => catalog_markdown(),
        _ => markdown::answer(&answer_for(engine, &request_for(uri)?)?),
    };
    if uri == WORKSPACE_URI
        && let Some(context) = workspace_context
    {
        text.push_str("\n\n");
        text.push_str(context);
    }
    encode_result(
        "resource",
        ResourceContents {
            contents: [ResourceContent {
                uri,
                mime_type: MARKDOWN,
                text,
            }],
        },
    )
}

// This is static schema metadata, not a native owner continuation. The
// version binds every resumed byte to the same complete descriptor JSON.
const CATALOG_PAGE_BYTES: usize = 8192;

struct CatalogRead<'a> {
    name: &'a str,
    offset: usize,
    limit: usize,
    version: Option<&'a str>,
}
impl<'a> CatalogRead<'a> {
    fn parse(selector: &'a str) -> Result<Self, RpcError> {
        let (name, query) = selector
            .split_once('?')
            .map_or((selector, None), |(name, query)| (name, Some(query)));
        let mut range = Self {
            name,
            offset: 0,
            limit: CATALOG_PAGE_BYTES,
            version: None,
        };
        if let Some(query) = query {
            let mut keys = std::collections::BTreeSet::new();
            for field in query.split('&') {
                let (key, value) = field
                    .split_once('=')
                    .ok_or_else(|| RpcError::invalid("catalog range requires key=value"))?;
                if !keys.insert(key) {
                    return Err(RpcError::invalid("duplicate catalog range argument"));
                }
                match key {
                    "offset" => range.offset = catalog_unsigned(value)?,
                    "limit" => range.limit = catalog_unsigned(value)?,
                    "version"
                        if value.len() == 64
                            && value.bytes().all(|byte| {
                                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
                            }) =>
                    {
                        range.version = Some(value)
                    }
                    _ => {
                        return Err(RpcError::invalid(
                            "unknown or malformed catalog range argument",
                        ));
                    }
                }
            }
            let Some(version) = range.version else {
                return Err(RpcError::invalid(
                    "catalog range requires its returned schema version",
                ));
            };
            let canonical = format!(
                "offset={}&limit={}&version={version}",
                range.offset, range.limit
            );
            if query != canonical {
                return Err(RpcError::invalid(
                    "catalog range must copy its canonical returned URI",
                ));
            }
        }
        if !(1..=CATALOG_PAGE_BYTES).contains(&range.limit) {
            return Err(RpcError::invalid(
                "catalog range limit must be from 1 through 8192 bytes",
            ));
        }
        Ok(range)
    }
}
fn catalog_unsigned(value: &str) -> Result<usize, RpcError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RpcError::invalid(
            "catalog range requires an unsigned byte offset or limit",
        ));
    }
    value
        .parse()
        .map_err(|_| RpcError::invalid("catalog range integer overflow"))
}

fn catalog_page(
    uri: &str,
    text: &str,
    range: CatalogRead<'_>,
    response_id: &Value,
) -> Result<Value, RpcError> {
    let version = blake3::hash(text.as_bytes()).to_hex().to_string();
    if range.version.is_some_and(|selected| selected != version) {
        return Err(RpcError::new(
            -32010,
            "Catalog schema changed; restart its root resource",
        ));
    }
    if range.offset > text.len() || !text.is_char_boundary(range.offset) {
        return Err(RpcError::invalid(
            "catalog offset is outside its exact UTF-8 document",
        ));
    }
    let budget = ResponseBudget::new(response_id);
    let mut limit = range.limit;
    loop {
        let mut end = range.offset.saturating_add(limit).min(text.len());
        while end > range.offset && !text.is_char_boundary(end) {
            end -= 1;
        }
        // A positive range never returns an empty nonterminal page merely
        // because its next complete character uses several UTF-8 bytes.
        if end == range.offset && end < text.len() {
            return Err(RpcError::invalid(
                "catalog range cannot hold its next UTF-8 character",
            ));
        }
        let complete = end == text.len();
        let next = (!complete).then(|| {
            format!(
                "{TOOLS_URI}/{}?offset={end}&limit={limit}&version={version}",
                range.name
            )
        });
        let result = encode_result("resource", json!({
            "contents":[{"uri":uri,"mimeType":if range.offset == 0 && complete {"application/json"} else {"text/plain"},"text":&text[range.offset..end]}],
            "_meta":{"backend/catalogRange":{"schema":"backend-mcp.tool-card-range/v1","name":range.name,"version":version,
                "offset":range.offset,"end":end,"limit":limit,"totalBytes":text.len(),"remainingBytes":text.len()-end,"complete":complete},
                "backend/nextUri":next}
        })).and_then(|result| budget.admit(result));
        match result {
            Ok(result) => return Ok(result),
            Err(error) if super::semantic_pages::oversized(&error) && limit > 4 => {
                limit = limit.div_ceil(2).max(4)
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Serialize)]
struct ResourceContents<'a> {
    contents: [ResourceContent<'a>; 1],
}

#[derive(Serialize)]
struct ResourceContent<'a> {
    uri: &'a str,
    #[serde(rename = "mimeType")]
    mime_type: &'static str,
    text: String,
}

/// Applies the same bounded typed envelope as tool projections to MCP
/// resources. Resource text is context, so an oversized resource is refused
/// with a typed transport fault instead of being silently truncated.
pub(super) fn encode_result<T: Serialize>(
    answer: &'static str,
    body: T,
) -> Result<Value, RpcError> {
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

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn utf8_schema_ranges_reassemble_exact_scalars_and_fence_changed_documents() {
        // Metadata encoder fixture, not an owner publication or native schema.
        let text = r#"{"description":"λ🙂中文 λ🙂中文","type":"object"}"#;
        let name = "backend.status";
        let root = format!("{TOOLS_URI}/{name}");
        let mut uri = root.clone();
        let mut joined = String::new();
        let mut selected = CatalogRead {
            name,
            offset: 0,
            limit: 17,
            version: None,
        };
        loop {
            let result = catalog_page(&uri, text, selected, &json!("λ🙂caller"))
                .expect("bounded actual encoder");
            let range = &result["_meta"]["backend/catalogRange"];
            assert_eq!(range["offset"], joined.len());
            joined.push_str(
                result["contents"][0]["text"]
                    .as_str()
                    .expect("whole UTF-8 scalars"),
            );
            assert_eq!(range["end"], joined.len());
            if range["complete"] == true {
                break;
            }
            uri = result["_meta"]["backend/nextUri"]
                .as_str()
                .expect("exact next URI")
                .to_owned();
            selected = CatalogRead::parse(
                uri.strip_prefix("backend://tools/catalog/")
                    .expect("namespace"),
            )
            .expect("canonical range");
        }
        assert_eq!(joined, text);
        let version = blake3::hash(text.as_bytes()).to_hex().to_string();
        let changed = format!("{text} ");
        let stale = catalog_page(
            &root,
            &changed,
            CatalogRead {
                name,
                offset: 1,
                limit: 17,
                version: Some(&version),
            },
            &json!(9),
        )
        .expect_err("old continuation cannot mix schema revisions");
        assert_eq!(stale.code, -32010);
        let inside_scalar = text.find('🙂').expect("scalar") + 1;
        let invalid = catalog_page(
            &root,
            text,
            CatalogRead {
                name,
                offset: inside_scalar,
                limit: 17,
                version: None,
            },
            &json!(9),
        )
        .expect_err("range cannot start inside UTF-8 scalar");
        assert_eq!(invalid.code, -32602);
    }
}
