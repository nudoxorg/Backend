//! LR-2: every tool argument and result has a `schemars`-derived schema, and
//! every result survives a JSON serialisation round trip.
//!
//! These are the tests that would catch the failure mode LR-2 exists to
//! prevent: a tool whose payload is shaped by hand-written JSON rather than by
//! a Rust type, which then drifts from what the engine actually produces.
//!
//! After the wire-type migration the result types (`SearchResult`, `SymbolDoc`)
//! serialise wire types directly via their `Serialize` + `JsonSchema` derives.
//! `Deserialize` is not derived for those types (it is impossible for
//! `SigToken::Kw(&'static str)`), so round-trip tests use `Serialize` only
//! and compare JSON strings rather than deserialising back.

use nudox_engine::mcp::SymbolKeyDto;
use nudox_engine::mcp::tools::{
    CompactSymbolDoc, CompactSymbolReference, DiffVersionsResult, FindUsagesArgs, GetSymbolArgs,
    GetSymbolsArgs, GraphQueryArgs, GraphSchemaArgs, IndexPackageResult, ListPackagesArgs,
    ListVersionsArgs, ListVersionsResult, PackageSummary, PackagesResult, QueryResult,
    QueryResultRow, ReferenceCoverage, RefsResult, SchemaResult, SearchHitDoc, SearchResult,
    SearchSymbolsArgs,
    SelectVersionArgs, SelectVersionResult, SemanticStatus, SymbolDoc, SymbolFormat, SymbolsResult,
    UsageRow, UsagesResult, VersionSummary,
};
use nudox_engine::wire::{
    CalloutLevel, GenerationId, HitRow, KindDiscriminant, KindTag, LangId, MemberRow, Provenance,
    RenderSection, SectionId, SectionKind, SectionPlan, SigToken, SizeHint, SourceLocation,
    SymbolHead, Timeline, TimelineChange, TimelineRow, Visibility,
};
use schemars::schema_for;
use serde::Serialize;

/// A schema must be a JSON object with at least a `type` or a composition
/// keyword — an empty schema documents nothing and would let any payload
/// through.
fn assert_schema_is_meaningful(schema: &schemars::Schema, what: &str) {
    let value = serde_json::to_value(schema).expect("a derived schema must serialise");
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("{what}: schema must be an object"));
    assert!(
        object.contains_key("type")
            || object.contains_key("oneOf")
            || object.contains_key("anyOf")
            || object.contains_key("$ref"),
        "{what}: schema is empty — it would accept anything: {value}"
    );
}

/// Serialise a value and assert the JSON is non-empty.
///
/// `Deserialize` is not available for wire types containing `SigToken`, so we
/// do a one-way serialise check here rather than a full round-trip.
fn assert_serialises<T: Serialize>(value: &T, what: &str) {
    let json = serde_json::to_string(value).unwrap_or_else(|e| panic!("{what}: serialise: {e}"));
    assert!(!json.is_empty(), "{what}: serialised to empty string");
}

/// Serialise, deserialise, and require the value to survive unchanged.
///
/// Used for types that are fully `Serialize + Deserialize` (args and simple
/// result types that do not contain `SigToken`).
fn assert_round_trips<T>(value: &T, what: &str)
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).unwrap_or_else(|e| panic!("{what}: serialise: {e}"));
    let back: T =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("{what}: deserialise: {e} ({json})"));
    assert_eq!(value, &back, "{what}: round trip changed the value");
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[test]
fn every_tool_argument_type_has_a_derived_schema() {
    assert_schema_is_meaningful(&schema_for!(SearchSymbolsArgs), "search_symbols args");
    assert_schema_is_meaningful(&schema_for!(GetSymbolArgs), "get_symbol args");
    assert_schema_is_meaningful(&schema_for!(GetSymbolsArgs), "get_symbols args");
    assert_schema_is_meaningful(&schema_for!(FindUsagesArgs), "find_usages args");
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::GetOccurrencesArgs),
        "get_occurrences args",
    );
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::SemanticSearchArgs),
        "semantic_search args",
    );
    assert_schema_is_meaningful(&schema_for!(ListPackagesArgs), "list_packages args");
    assert_schema_is_meaningful(&schema_for!(ListVersionsArgs), "list_versions args");
    assert_schema_is_meaningful(&schema_for!(SelectVersionArgs), "select_version args");
    assert_schema_is_meaningful(&schema_for!(GraphQueryArgs), "graph_query args");
    assert_schema_is_meaningful(&schema_for!(GraphSchemaArgs), "graph_schema args");

    // The consolidated tool surface (docs/MCP-SURFACE-PLAN.md §5.1): `search`,
    // `read`, `refs`, `packages`.
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::ReadArgs),
        "read args",
    );
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::RefsArgs),
        "refs args",
    );
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::PackagesArgs),
        "packages args",
    );
}

#[test]
fn every_tool_result_type_has_a_derived_schema() {
    assert_schema_is_meaningful(&schema_for!(SearchResult), "search_symbols result");
    assert_schema_is_meaningful(&schema_for!(SymbolDoc), "get_symbol result");
    assert_schema_is_meaningful(&schema_for!(CompactSymbolDoc), "compact get_symbol result");
    assert_schema_is_meaningful(&schema_for!(SymbolsResult), "get_symbols result");
    assert_schema_is_meaningful(&schema_for!(UsagesResult), "find_usages result");
    assert_schema_is_meaningful(&schema_for!(PackagesResult), "list_packages result");
    assert_schema_is_meaningful(&schema_for!(ListVersionsResult), "list_versions result");
    assert_schema_is_meaningful(&schema_for!(SelectVersionResult), "select_version result");
    assert_schema_is_meaningful(&schema_for!(QueryResult), "graph_query result");
    assert_schema_is_meaningful(&schema_for!(SchemaResult), "graph_schema result");
    assert_schema_is_meaningful(&schema_for!(DiffVersionsResult), "diff_versions result");
    assert_schema_is_meaningful(&schema_for!(RefsResult), "refs result");
    assert_schema_is_meaningful(
        &schema_for!(nudox_engine::mcp::tools::LoadedPackagesResult),
        "packages result",
    );
}

/// Every tool result schema must declare a root `type` of `"object"`.
///
/// # Why this test exists
///
/// rmcp validates `outputSchema` at **server construction** and the MCP spec
/// requires a root object, so a result type that does not say so does not
/// disable one tool — it panics `NudoxMcpServer::serve`, taking the whole
/// endpoint down. `SelectVersionResult` shipped exactly that defect today: a
/// `#[serde(tag = "kind")]` enum, for which schemars emits a bare `oneOf` with
/// no root `type`. Every `tools`-level test passed, because none of them
/// construct a server; only `tests/endpoint.rs` did, and it failed with
/// "Schema is missing 'type' field" on all six of its cases.
///
/// Asserting on the *schema* rather than on server construction is the point:
/// this names which type is wrong, where the endpoint failure only says that
/// one of them is.
#[test]
fn every_tool_result_schema_declares_a_root_object_type() {
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "SearchResult",
            serde_json::to_value(schema_for!(SearchResult)).unwrap(),
        ),
        (
            "SymbolDoc",
            serde_json::to_value(schema_for!(SymbolDoc)).unwrap(),
        ),
        (
            "UsagesResult",
            serde_json::to_value(schema_for!(UsagesResult)).unwrap(),
        ),
        (
            "PackagesResult",
            serde_json::to_value(schema_for!(PackagesResult)).unwrap(),
        ),
        (
            "ListVersionsResult",
            serde_json::to_value(schema_for!(ListVersionsResult)).unwrap(),
        ),
        (
            "SelectVersionResult",
            serde_json::to_value(schema_for!(SelectVersionResult)).unwrap(),
        ),
        (
            "DiffVersionsResult",
            serde_json::to_value(schema_for!(DiffVersionsResult)).unwrap(),
        ),
        (
            "QueryResult",
            serde_json::to_value(schema_for!(QueryResult)).unwrap(),
        ),
        (
            "SchemaResult",
            serde_json::to_value(schema_for!(SchemaResult)).unwrap(),
        ),
        (
            "IndexPackageResult",
            serde_json::to_value(schema_for!(IndexPackageResult)).unwrap(),
        ),
        (
            "SemanticStatus",
            serde_json::to_value(schema_for!(SemanticStatus)).unwrap(),
        ),
        (
            "RefsResult",
            serde_json::to_value(schema_for!(RefsResult)).unwrap(),
        ),
        (
            "ReferenceCoverage",
            serde_json::to_value(schema_for!(ReferenceCoverage)).unwrap(),
        ),
    ];
    for (name, schema) in cases {
        assert_eq!(
            schema.get("type").and_then(|t| t.as_str()),
            Some("object"),
            "{name}'s schema has no root `type: \"object\"`; rmcp rejects that \
             at server construction and the whole endpoint panics. A tagged \
             enum needs `#[schemars(extend(\"type\" = \"object\"))]`. schema: \
             {schema}"
        );
    }
}

/// The list above must not be the only thing standing between a new result type
/// and a panicking server.
///
/// # Why this exists
///
/// `every_tool_result_schema_declares_a_root_object_type` was written on
/// 2026-08-09 after `SelectVersionResult` shipped a bare `oneOf` and took down
/// every endpoint test. It was written as a **hand-maintained list**. Hours
/// later `IndexPackageResult` was added by a different track, was not on the
/// list, and reproduced the identical failure: the guard passed, and
/// `NudoxMcpServer::new` panicked at router construction — killing
/// `tests/endpoint.rs` (0/6), `tests/host_lifecycle.rs` (0/7) and the GUI
/// screenshot harness, none of which touch the offending type.
///
/// A guard that enumerates its own subjects by hand does not fail when someone
/// forgets it; it goes quiet. That is the failure mode this repository keeps
/// finding, so the list needs a keeper.
///
/// This test reads the MCP result-type module and asserts that every type carrying
/// `#[serde(tag = …)]` — the exact construct schemars renders without a root
/// `type` — appears in the case list above. It is a source scan for the same
/// reason `theme_law.rs` and `checkout_completeness.rs` are: the property is
/// about *what exists in the tree*, and no amount of exercising the types that
/// were remembered can speak for the one that was not.
#[test]
fn every_internally_tagged_result_type_is_covered_by_the_root_type_guard() {
    let tools_src = include_str!("../../src/mcp/tools/results.rs");
    let this_test = include_str!("schemas.rs");

    // `#[serde(tag = "…")]` on a `pub enum` is precisely the shape that emits a
    // bare `oneOf`. Untagged or externally-tagged enums render differently and
    // are not at risk, so this deliberately does not flag them.
    let mut tagged: Vec<&str> = Vec::new();
    let lines: Vec<&str> = tools_src.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("#[serde(tag") {
            continue;
        }
        // Walk forward past any further attributes to the declaration itself.
        for decl in lines.iter().skip(i + 1).take(6) {
            if let Some(rest) = decl.trim_start().strip_prefix("pub enum ") {
                let name = rest
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or_default();
                if !name.is_empty() {
                    tagged.push(name);
                }
                break;
            }
        }
    }

    assert!(
        !tagged.is_empty(),
        "found no `#[serde(tag = …)]` enums in tools/results.rs — either they were \
         all removed (in which case delete this test) or the scan broke, which \
         would make it pass forever without checking anything"
    );

    let missing: Vec<&str> = tagged
        .iter()
        .copied()
        .filter(|name| !this_test.contains(&format!("schema_for!({name})")))
        .collect();

    assert!(
        missing.is_empty(),
        "internally-tagged result type(s) {missing:?} are not in \
         `every_tool_result_schema_declares_a_root_object_type`'s case list. \
         schemars emits a bare `oneOf` for these and rmcp rejects it when the \
         router is built, so the server panics before serving anything — and \
         the failure appears in endpoint, host-lifecycle and screenshot tests \
         that have nothing to do with the type. Add \
         `#[schemars(extend(\"type\" = \"object\"))]` to it and a \
         `schema_for!(…)` row above."
    );
}

/// §L2.5 / the capability-probe's "critical honesty requirement": a tool that
/// lets an agent switch which generation of a package it reads must not ship
/// a schema that implies every `SymbolKey` survives the switch. This asserts
/// the caveat is actually in the text an MCP client sees — the derived
/// schema's `description`, sourced from the doc comments above
/// `SelectVersionResult` and `ListVersionsResult` — not merely somewhere in
/// this crate's rustdoc that no client ever reads.
#[test]
fn version_switching_schemas_disclose_that_keys_can_stop_resolving() {
    let select = serde_json::to_value(schema_for!(SelectVersionResult))
        .expect("SelectVersionResult schema must serialise");
    let select_desc = select
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_else(|| panic!("SelectVersionResult schema has no top-level description"));
    assert!(
        select_desc.contains("not guaranteed") || select_desc.contains("does not hold for"),
        "select_version's result schema must plainly say key survival is not \
         guaranteed for every declaration; got: {select_desc:?}"
    );

    let list = serde_json::to_value(schema_for!(ListVersionsResult))
        .expect("ListVersionsResult schema must serialise");
    let list_desc = list
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_else(|| panic!("ListVersionsResult schema has no top-level description"));
    assert!(
        list_desc.contains("not guaranteed") || list_desc.contains("indistinguishable from"),
        "list_versions's result schema must plainly say a stale key looks the \
         same as a deleted symbol; got: {list_desc:?}"
    );
}

#[test]
fn argument_schemas_document_their_fields() {
    // A description on every field is what an LLM client reads to decide how to
    // call the tool; an undocumented argument is a silently unusable one.
    let schema =
        serde_json::to_value(schema_for!(SearchSymbolsArgs)).expect("schema must serialise");
    let properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("search_symbols args must expose properties");

    for field in ["query", "kinds", "packages", "limit"] {
        let property = properties
            .get(field)
            .unwrap_or_else(|| panic!("search_symbols args must have a `{field}` property"));
        assert!(
            property.get("description").is_some(),
            "`{field}` has no description — an agent cannot tell what it does"
        );
    }
}

#[test]
fn optional_arguments_are_actually_optional() {
    // `query` is the only required field; everything else must be omissible,
    // or a caller that just wants a name search is forced to invent values.
    let schema =
        serde_json::to_value(schema_for!(SearchSymbolsArgs)).expect("schema must serialise");
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, vec!["query"], "only `query` may be required");

    let parsed: SearchSymbolsArgs =
        serde_json::from_str(r#"{"query":"Deserializer"}"#).expect("a bare query must deserialise");
    assert_eq!(parsed.kinds, None);
    assert_eq!(parsed.packages, None);
    assert_eq!(parsed.limit, None);
}

/// The batch reader's default is deliberately source-first.  If this default
/// drifted to signature-only, an agent issuing the documented `get_symbols`
/// call would silently lose bodies; if `format` stopped being optional, a
/// caller would have to know an implementation detail merely to read symbols.
#[test]
fn get_symbols_schema_keeps_source_first_format_optional() {
    let schema = serde_json::to_value(schema_for!(GetSymbolsArgs))
        .expect("get_symbols schema must serialise");
    let properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("get_symbols args must expose properties");
    assert!(properties.contains_key("keys"), "keys must be documented");
    assert!(
        properties.contains_key("format"),
        "format must be documented"
    );
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, vec!["keys"], "only keys may be required");

    let parsed: GetSymbolsArgs = serde_json::from_str(r#"{"keys":["cargo:serde#0000000000000000000000000000000000000000000000000000000000000000"]}"#)
        .expect("a batch with no format must deserialise");
    assert_eq!(parsed.format, SymbolFormat::Source, "source is the default");
}

// ---------------------------------------------------------------------------
// Serialisation tests for wire result types
//
// `SymbolDoc` and `SearchResult` contain `SigToken` (which carries `&'static
// str` in `Kw` and `Punct` variants), so they are not `Deserialize`.  We
// verify they serialise correctly rather than round-tripping.
// ---------------------------------------------------------------------------

/// A key with a well-formed 64-character intro half.
fn sample_key_dto() -> SymbolKeyDto {
    SymbolKeyDto(format!("cargo:serde#{}", "ab".repeat(32)))
}

fn sample_wire_key() -> nudox_engine::wire::SymbolKey {
    sample_key_dto().to_wire().expect("sample key must parse")
}

fn sample_sig() -> Vec<SigToken> {
    vec![
        SigToken::Kw("fn"),
        SigToken::Ws,
        SigToken::Ident(nudox_engine::wire::SharedStr::from("parse")),
        SigToken::Punct("("),
        SigToken::Ty {
            text: nudox_engine::wire::SharedStr::from("&str"),
            target: Some(sample_wire_key()),
        },
        SigToken::Punct(")"),
    ]
}

fn sample_head() -> Box<SymbolHead> {
    Box::new(SymbolHead {
        key: sample_wire_key(),
        name: nudox_engine::wire::SharedStr::from("Deserializer"),
        breadcrumb: vec![nudox_engine::wire::CrumbRef {
            key: sample_wire_key(),
            label: nudox_engine::wire::SharedStr::from("serde"),
        }],
        signature: sample_sig(),
        kind: KindTag::Known(KindDiscriminant::from_u16(3).expect("Function discriminant")),
        visibility: Visibility::Public,
        provenance: Provenance::TrustedLocal,
        deprecation: Some(nudox_engine::wire::SharedStr::from(
            "use `from_str` instead",
        )),
        // Populated rather than `None` on purpose: this fixture exists to prove
        // the schema carries everything an MCP client needs, and a field that is
        // always `None` in the only sample is a field whose serialisation nobody
        // has ever actually checked.
        cfg: Some(nudox_engine::wire::SharedStr::from(
            "cfg(feature = \"std\")",
        )),
        section_plan: vec![SectionPlan {
            id: SectionId(1),
            kind: SectionKind::Prose,
            size_hint: SizeHint::Lines(12),
        }],
        // Source location: populated with the `Declared` variant on purpose.
        // It is the only one an MCP client can turn into a jump, so a fixture
        // that used `Unlocated` would leave the fields that actually matter —
        // the line/column pair — unexercised by the round-trip below.
        source: nudox_engine::wire::SourceLocation::Declared {
            file: nudox_engine::wire::SharedStr::from("src/de/mod.rs"),
            bytes: [120, 480],
            start: nudox_engine::wire::LineCol {
                line: std::num::NonZeroU32::new(7).expect("nonzero"),
                column: std::num::NonZeroU32::new(1).expect("nonzero"),
            },
            end: nudox_engine::wire::LineCol {
                line: std::num::NonZeroU32::new(19).expect("nonzero"),
                column: std::num::NonZeroU32::new(2).expect("nonzero"),
            },
        },
        source_excerpt: Some(nudox_engine::wire::SharedStr::from(
            "pub trait Deserializer<'de> {}",
        )),
    })
}

/// A one-row `Timeline` for the sample key: present in the one loaded
/// version, exactly what a single-version corpus produces. Populated rather
/// than empty on purpose (see `sample_head`'s own reasoning) — an always-empty
/// fixture would leave every field of `Timeline`/`TimelineRow` unexercised by
/// the assertions below.
fn sample_timeline() -> Timeline {
    Timeline {
        key: sample_wire_key(),
        rows: vec![TimelineRow {
            version: nudox_engine::wire::SharedStr::from("1.0.219"),
            change: TimelineChange::Present,
            name: nudox_engine::wire::SharedStr::from("Deserializer"),
            sig: sample_sig(),
            deprecated: false,
            is_current: true,
        }]
        .into(),
        versions_examined: 1,
    }
}

#[test]
fn search_result_serialises() {
    let value = SearchResult {
        hits: vec![SearchHitDoc {
            hit: HitRow {
                key: sample_wire_key(),
                display_name: nudox_engine::wire::SharedStr::from("serde::de::Deserializer"),
                sig_preview: sample_sig(),
                kind: KindTag::Known(KindDiscriminant::from_u16(5).expect("Trait discriminant")),
                provenance: Provenance::SyncedLocal {
                    generation: GenerationId(7),
                },
                score: 0.875,
            },
            address: Some("cargo:serde@1.0.219::serde::de::Deserializer[trait]".to_owned()),
            semantic: false,
            documentation: None,
        }],
        semantic: None,
        excluded_kinds: None,
        truncated: true,
        next_cursor: Some("50".to_owned()),
    };
    let json = serde_json::to_string(&value).expect("SearchResult must serialise");
    // Key must be the canonical string form.
    assert!(
        json.contains("cargo:serde#"),
        "hit key must be ecosystem:name#introhex string: {json}"
    );
    // Provenance must have a kind discriminant.
    assert!(
        json.contains("synced_local"),
        "provenance kind must appear: {json}"
    );
    // truncated flag must be present.
    assert!(
        json.contains("\"truncated\":true"),
        "truncated must serialise: {json}"
    );
    // next_cursor must reach the wire as an opaque string an agent can pass
    // straight back into `search_symbols`'s `cursor` argument.
    assert!(
        json.contains("\"next_cursor\":\"50\""),
        "next_cursor must serialise: {json}"
    );
}

#[test]
fn symbol_doc_serialises() {
    use std::sync::Arc;
    let blocks = vec![nudox_engine::wire::ProseBlock::Paragraph {
        runs: vec![nudox_engine::wire::InlineRun::Text {
            text: nudox_engine::wire::SharedStr::from("Hello world"),
        }],
    }];
    let value = SymbolDoc {
        head: sample_head(),
        timeline: sample_timeline(),
        sections: vec![
            RenderSection::Prose {
                id: SectionId(1),
                blocks: blocks.clone(),
            },
            RenderSection::CodeBlock {
                id: SectionId(2),
                lang: LangId(nudox_engine::wire::SharedStr::from("rust")),
                text: nudox_engine::wire::SharedStr::from("fn main() {}"),
                line_count: 1,
            },
            RenderSection::Members {
                id: SectionId(3),
                entries: Arc::from(vec![MemberRow {
                    key: sample_wire_key(),
                    name: nudox_engine::wire::SharedStr::from("deserialize"),
                    sig: sample_sig(),
                    kind: KindTag::Known(
                        KindDiscriminant::from_u16(3).expect("Function discriminant"),
                    ),
                    visibility: Visibility::Public,
                    source: SourceLocation::Unlocated {
                        reason: nudox_engine::wire::UnlocatedReason::Synthesized,
                    },
                }]),
            },
            RenderSection::Callout {
                id: SectionId(4),
                level: CalloutLevel::Warning,
                blocks,
            },
            RenderSection::Unknown {
                id: SectionId(5),
                kind_tag: nudox_engine::wire::SharedStr::from("future-kind"),
            },
        ],
    };
    assert_serialises(&value, "SymbolDoc");

    let json = serde_json::to_string(&value).expect("SymbolDoc must serialise");
    // Section kind discriminant must appear.
    assert!(
        json.contains("\"kind\":\"prose\""),
        "prose section must be tagged: {json}"
    );
    assert!(
        json.contains("\"kind\":\"code_block\""),
        "code_block section must be tagged: {json}"
    );
    assert!(
        json.contains("\"kind\":\"members\""),
        "members section must be tagged: {json}"
    );
    assert!(
        json.contains("\"kind\":\"callout\""),
        "callout section must be tagged: {json}"
    );
    assert!(
        json.contains("\"kind\":\"unknown\""),
        "unknown section must be tagged: {json}"
    );

    // The source location must reach the wire *as a jump target*, not merely as
    // present. An MCP client cannot open `bytes 120–480`; it can open
    // `src/de/mod.rs:7:1`. So this asserts the discriminant, the path, and the
    // line/column pair — the three things without which "source jumping" is
    // still just a rendered string.
    assert!(
        json.contains("\"source\":{\"kind\":\"declared\""),
        "head.source must serialise its variant so a client can tell a jump \
         target from a byte range: {json}"
    );
    assert!(
        json.contains("\"file\":\"src/de/mod.rs\""),
        "head.source must carry the package-relative path: {json}"
    );
    assert!(
        json.contains("\"start\":{\"line\":7,\"column\":1}"),
        "head.source must carry 1-based line/column, which is what an editor \
         opens: {json}"
    );
    assert!(
        json.contains("\"bytes\":[120,480]"),
        "head.source must still carry the byte range for in-file slicing: {json}"
    );

    // `timeline` must reach the wire — this is the fix for the defect where
    // `open_symbol` computed `DocEvent::Timeline` on every call and
    // `do_get_symbol` silently dropped it in a wildcard match arm. A test
    // that only checked `head`/`sections` would stay green if that
    // regressed.
    assert!(
        json.contains("\"timeline\":{"),
        "SymbolDoc must carry a timeline field: {json}"
    );
    assert!(
        json.contains("\"kind\":\"present\""),
        "the sample timeline's one row must serialise its TimelineChange::Present \
         tag: {json}"
    );
    assert!(
        json.contains("\"versions_examined\":1"),
        "timeline must carry versions_examined: {json}"
    );
}

/// Compact symbol records are the token-budget boundary.  This test pins the
/// shape directly: no GUI section plan, timeline, breadcrumbs, or typed token
/// array may leak back into the public MCP projection, while the exact source
/// text survives intact.
#[test]
fn compact_symbol_result_is_flat_and_source_faithful() {
    let value = CompactSymbolDoc {
        key: sample_key_dto(),
        address: Some("cargo:serde@1.0.219::serde::de::Deserializer[trait]".to_owned()),
        path: "serde::de::Deserializer".to_owned(),
        signature: None,
        kind: KindTag::Known(KindDiscriminant::from_u16(6).expect("Trait discriminant")),
        visibility: Visibility::Public,
        references: vec![CompactSymbolReference {
            text: "Deserializer".to_owned(),
            target: sample_key_dto(),
        }],
        location: Some("src/de/mod.rs:7:1".to_owned()),
        source: Some("pub trait Deserializer<'de> {}".to_owned()),
        deprecation: None,
    };
    let batch = SymbolsResult {
        symbols: vec![value.clone()],
    };

    assert_serialises(&value, "CompactSymbolDoc");
    assert_serialises(&batch, "SymbolsResult");
    let json = serde_json::to_value(&batch).expect("compact result must serialise");
    let symbol = &json["symbols"][0];
    assert_eq!(symbol["source"], "pub trait Deserializer<'de> {}");
    assert!(
        symbol.get("signature").is_none(),
        "exact source must not be accompanied by a duplicate signature"
    );
    assert_eq!(
        symbol["references"][0]["text"], "Deserializer",
        "a resolved signature link must stay first-class in compact output"
    );
    assert!(
        symbol["references"][0]["target"]
            .as_str()
            .is_some_and(|key| key.starts_with("cargo:serde#")),
        "compact references must carry navigable symbol keys: {}",
        symbol["references"]
    );
    for forbidden in ["head", "timeline", "sections", "section_plan", "breadcrumb"] {
        assert!(
            symbol.get(forbidden).is_none(),
            "compact MCP output must not leak the GUI `{forbidden}` structure: {symbol}"
        );
    }
}

#[test]
fn usages_result_round_trips() {
    let value = UsagesResult {
        usages: vec![UsageRow {
            key: sample_key_dto(),
            address: Some("cargo:serde_json::serde_json::from_str[function]".to_owned()),
            name: "from_str".to_owned(),
            kind: "Function".to_owned(),
            signature: "fn from_str(input: &str) -> Result<T>".to_owned(),
            path: "serde_json::from_str".to_owned(),
        }],
        truncated: false,
        next_cursor: None,
    };
    assert_round_trips(&value, "UsagesResult");
}

#[test]
fn packages_result_round_trips() {
    let value = PackagesResult {
        packages: vec![PackageSummary {
            lineage: "cargo:serde".to_owned(),
            name: "serde".to_owned(),
            ecosystem: "cargo".to_owned(),
        }],
    };
    assert_round_trips(&value, "PackagesResult");
}

#[test]
fn query_result_round_trips() {
    let value = QueryResult {
        columns: vec!["key".to_owned(), "name".to_owned()],
        rows: vec![QueryResultRow {
            cells: vec![sample_key_dto().0, "Deserializer".to_owned()],
        }],
        truncated: true,
        next_cursor: Some("10".to_owned()),
        edge_coverage: None,
    };
    assert_round_trips(&value, "QueryResult");
}

#[test]
fn schema_result_round_trips() {
    let full = SchemaResult {
        schema: nudox_engine::mcp::SCHEMA_SDL.to_owned(),
        full: true,
    };
    assert_round_trips(&full, "SchemaResult { full: true }");

    let card = SchemaResult {
        schema: nudox_engine::mcp::SCHEMA_CARD.to_owned(),
        full: false,
    };
    assert_round_trips(&card, "SchemaResult { full: false }");
}

#[test]
fn tool_arguments_round_trip() {
    assert_round_trips(
        &SearchSymbolsArgs {
            query: "Deserializer".into(),
            kinds: Some(vec!["Trait".into()]),
            packages: Some(vec!["cargo:serde".into()]),
            limit: Some(25),
            cursor: None,
        },
        "SearchSymbolsArgs",
    );
    assert_round_trips(
        &GetSymbolArgs {
            key: sample_key_dto(),
        },
        "GetSymbolArgs",
    );
    assert_round_trips(
        &FindUsagesArgs {
            key: sample_key_dto(),
            limit: None,
            cursor: None,
        },
        "FindUsagesArgs",
    );
    assert_round_trips(&ListPackagesArgs {}, "ListPackagesArgs");
    assert_round_trips(
        &GraphQueryArgs {
            query: "query { Packages { lineage @output } }".into(),
            args: Some(
                [("key".to_owned(), sample_key_dto().0)]
                    .into_iter()
                    .collect(),
            ),
            limit: Some(10),
            cursor: None,
        },
        "GraphQueryArgs",
    );
    assert_round_trips(&GraphSchemaArgs::default(), "GraphSchemaArgs");
    assert_round_trips(
        &GraphSchemaArgs { full: true },
        "GraphSchemaArgs { full: true }",
    );
}

#[test]
fn version_tool_types_round_trip() {
    use nudox_engine::mcp::key::PackageLineageDto;

    assert_round_trips(
        &ListVersionsArgs {
            package: PackageLineageDto("cargo:memchr".to_owned()),
        },
        "ListVersionsArgs",
    );
    assert_round_trips(
        &SelectVersionArgs {
            package: PackageLineageDto("cargo:memchr".to_owned()),
            version: "2.8.0".to_owned(),
        },
        "SelectVersionArgs",
    );
    assert_round_trips(
        &ListVersionsResult {
            package: PackageLineageDto("cargo:memchr".to_owned()),
            versions: vec![
                VersionSummary {
                    version: "2.8.0".to_owned(),
                    is_current: true,
                    symbol_count: 1835,
                },
                VersionSummary {
                    version: "2.7.5".to_owned(),
                    is_current: false,
                    symbol_count: 1794,
                },
            ],
        },
        "ListVersionsResult",
    );
    assert_round_trips(
        &SelectVersionResult::Switched {
            package: PackageLineageDto("cargo:memchr".to_owned()),
            version: "2.7.5".to_owned(),
            symbol_count: 1794,
        },
        "SelectVersionResult::Switched",
    );
    assert_round_trips(
        &SelectVersionResult::NotLoaded {
            package: PackageLineageDto("cargo:memchr".to_owned()),
            version: "9.9.9".to_owned(),
        },
        "SelectVersionResult::NotLoaded",
    );
}

#[test]
fn symbol_keys_are_plain_strings_on_the_wire() {
    // LR-1: the key is one value, spelled one way. If this ever serialised as
    // an object, keys would stop being copy-pasteable between tools.
    let json = serde_json::to_string(&sample_key_dto()).expect("key must serialise");
    assert!(
        json.starts_with('"'),
        "SymbolKey must be a JSON string, got {json}"
    );
    assert!(json.contains("cargo:serde#"));
}

#[test]
fn wire_key_serialises_as_canonical_string() {
    // The wire SymbolKey (in HitRow, SymbolHead, etc.) must also serialise as
    // the canonical string — not as a struct — so agent output is consistent
    // with what agents pass back in tool arguments.
    let key = sample_wire_key();
    // Serialise via the HitRow wrapper (which uses serialize_symbol_key).
    let hit = HitRow {
        key,
        display_name: nudox_engine::wire::SharedStr::from("test"),
        sig_preview: vec![],
        kind: KindTag::Unknown(0),
        provenance: Provenance::TrustedLocal,
        score: 1.0,
    };
    let json = serde_json::to_string(&hit).expect("HitRow must serialise");
    assert!(
        json.contains("\"key\":\"cargo:serde#"),
        "wire key in HitRow must be a string: {json}"
    );
}

#[test]
fn kind_tag_known_serialises_with_name() {
    let tag = KindTag::Known(KindDiscriminant::from_u16(3).expect("Function"));
    let json = serde_json::to_string(&tag).expect("KindTag must serialise");
    assert!(
        json.contains("\"kind\":\"known\""),
        "KindTag::Known must have kind discriminant: {json}"
    );
    assert!(
        json.contains("\"name\":"),
        "KindTag::Known must include the kind name: {json}"
    );
}

#[test]
fn kind_tag_unknown_serialises_with_raw() {
    let tag = KindTag::Unknown(999);
    let json = serde_json::to_string(&tag).expect("KindTag must serialise");
    assert!(
        json.contains("\"kind\":\"unknown\""),
        "KindTag::Unknown must have kind discriminant: {json}"
    );
    assert!(
        json.contains("\"raw\":999"),
        "KindTag::Unknown must carry raw value: {json}"
    );
}

#[test]
fn sig_token_kw_serialises_with_kind() {
    let token = SigToken::Kw("fn");
    let json = serde_json::to_string(&token).expect("SigToken must serialise");
    assert!(
        json.contains("\"kind\":\"kw\""),
        "SigToken::Kw must have kind: {json}"
    );
    assert!(
        json.contains("\"text\":\"fn\""),
        "SigToken::Kw must have text: {json}"
    );
}

#[test]
fn provenance_stale_serialises_unix_secs() {
    use std::time::{Duration, UNIX_EPOCH};
    let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let prov = Provenance::Stale { as_of: t };
    let json = serde_json::to_string(&prov).expect("Provenance must serialise");
    assert!(
        json.contains("\"kind\":\"stale\""),
        "Provenance::Stale must be tagged: {json}"
    );
    assert!(
        json.contains("1700000000"),
        "Provenance::Stale.as_of must be unix secs: {json}"
    );
}
