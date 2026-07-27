//! LR-2: every tool argument and result has a `schemars`-derived schema, and
//! every result survives a JSON round trip.
//!
//! These are the tests that would catch the failure mode LR-2 exists to
//! prevent: a tool whose payload is shaped by hand-written JSON rather than by
//! a Rust type, which then drifts from what the engine actually produces.

use nudox_mcp::dto::{
    CalloutLevelDto, FieldRowDto, HitRowDto, InlineRunDto, KindDto, LinkTargetDto, MemberRowDto,
    ProseBlockDto, ProvenanceDto, RenderSectionDto, SectionKindDto, SectionPlanDto, SigTokenDto,
    SignatureDto, SymbolHeadDto, SymbolKeyDto, VisibilityDto,
};
use nudox_mcp::tools::{
    FindUsagesArgs, GetSymbolArgs, GraphQueryArgs, GraphSchemaArgs, ListPackagesArgs,
    PackageSummary, PackagesResult, QueryResult, QueryResultRow, SchemaResult, SearchResult,
    SearchSymbolsArgs, SymbolDoc, UsageRow, UsagesResult,
};
use schemars::schema_for;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A schema must be a JSON object with at least a `type` or a composition
/// keyword — an empty schema documents nothing and would let any payload
/// through.
fn assert_schema_is_meaningful(schema: &schemars::Schema, what: &str) {
    let value = serde_json::to_value(schema).expect("a derived schema must serialise");
    let object = value.as_object().unwrap_or_else(|| panic!("{what}: schema must be an object"));
    assert!(
        object.contains_key("type")
            || object.contains_key("oneOf")
            || object.contains_key("anyOf")
            || object.contains_key("$ref"),
        "{what}: schema is empty — it would accept anything: {value}"
    );
}

/// Serialise, deserialise, and require the value to survive unchanged.
fn assert_round_trips<T>(value: &T, what: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
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
    assert_schema_is_meaningful(&schema_for!(FindUsagesArgs), "find_usages args");
    assert_schema_is_meaningful(&schema_for!(ListPackagesArgs), "list_packages args");
    assert_schema_is_meaningful(&schema_for!(GraphQueryArgs), "graph_query args");
    assert_schema_is_meaningful(&schema_for!(GraphSchemaArgs), "graph_schema args");
}

#[test]
fn every_tool_result_type_has_a_derived_schema() {
    assert_schema_is_meaningful(&schema_for!(SearchResult), "search_symbols result");
    assert_schema_is_meaningful(&schema_for!(SymbolDoc), "get_symbol result");
    assert_schema_is_meaningful(&schema_for!(UsagesResult), "find_usages result");
    assert_schema_is_meaningful(&schema_for!(PackagesResult), "list_packages result");
    assert_schema_is_meaningful(&schema_for!(QueryResult), "graph_query result");
    assert_schema_is_meaningful(&schema_for!(SchemaResult), "graph_schema result");
}

#[test]
fn argument_schemas_document_their_fields() {
    // A description on every field is what an LLM client reads to decide how to
    // call the tool; an undocumented argument is a silently unusable one.
    let schema = serde_json::to_value(schema_for!(SearchSymbolsArgs))
        .expect("schema must serialise");
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
    let schema = serde_json::to_value(schema_for!(SearchSymbolsArgs))
        .expect("schema must serialise");
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(required, vec!["query"], "only `query` may be required");

    let parsed: SearchSymbolsArgs = serde_json::from_str(r#"{"query":"Deserializer"}"#)
        .expect("a bare query must deserialise");
    assert_eq!(parsed.kinds, None);
    assert_eq!(parsed.packages, None);
    assert_eq!(parsed.limit, None);
}

// ---------------------------------------------------------------------------
// Round trips
// ---------------------------------------------------------------------------

/// A key with a well-formed 64-character intro half.
fn sample_key() -> SymbolKeyDto {
    SymbolKeyDto(format!("cargo:serde#{}", "ab".repeat(32)))
}

fn sample_signature() -> SignatureDto {
    SignatureDto {
        text: "fn parse(input: &str) -> Value".to_owned(),
        tokens: vec![
            SigTokenDto::Keyword { text: "fn".into() },
            SigTokenDto::Space,
            SigTokenDto::Ident { text: "parse".into() },
            SigTokenDto::Punct { text: "(".into() },
            SigTokenDto::Type { text: "&str".into(), target: Some(sample_key()) },
            SigTokenDto::Punct { text: ")".into() },
            SigTokenDto::Generic { text: "T".into() },
            SigTokenDto::Lifetime { text: "'a".into() },
            SigTokenDto::Unknown,
        ],
    }
}

fn sample_head() -> SymbolHeadDto {
    SymbolHeadDto {
        key: sample_key(),
        breadcrumb: vec![nudox_mcp::dto::CrumbDto {
            key: sample_key(),
            label: "serde".to_owned(),
        }],
        signature: sample_signature(),
        kind: KindDto("Function".to_owned()),
        visibility: VisibilityDto::Public,
        provenance: ProvenanceDto::TrustedLocal,
        deprecation: Some("use `from_str` instead".to_owned()),
        section_plan: vec![SectionPlanDto {
            id: 1,
            kind: SectionKindDto::Prose,
            estimated_lines: Some(12),
            estimated_rows: None,
        }],
    }
}

#[test]
fn search_result_round_trips() {
    let value = SearchResult {
        hits: vec![HitRowDto {
            key: sample_key(),
            display_name: "serde::de::Deserializer".to_owned(),
            signature: sample_signature(),
            kind: KindDto("Trait".to_owned()),
            provenance: ProvenanceDto::SyncedLocal { generation: 7 },
            score: 0.875,
        }],
        truncated: true,
    };
    assert_round_trips(&value, "SearchResult");
}

#[test]
fn symbol_doc_round_trips_across_every_section_variant() {
    let blocks = vec![
        ProseBlockDto::Paragraph {
            runs: vec![
                InlineRunDto::Text { text: "See ".into() },
                InlineRunDto::Code { text: "parse".into() },
                InlineRunDto::Strong { text: "now".into() },
                InlineRunDto::Emphasis { text: "really".into() },
                InlineRunDto::Link {
                    text: "docs".into(),
                    target: LinkTargetDto::Url { url: "https://example.invalid".into() },
                },
                InlineRunDto::Link {
                    text: "Deserializer".into(),
                    target: LinkTargetDto::Symbol { key: sample_key() },
                },
                InlineRunDto::Unknown,
            ],
        },
        ProseBlockDto::Heading { level: 3, runs: vec![] },
        ProseBlockDto::List { ordered: true, items: vec![vec![]] },
        ProseBlockDto::Rule,
        ProseBlockDto::Code { lang: "rust".into(), text: "let x = 1;".into(), line_count: 1 },
        ProseBlockDto::Unknown,
    ];

    let value = SymbolDoc {
        head: sample_head(),
        sections: vec![
            RenderSectionDto::Prose { id: 1, blocks: blocks.clone() },
            RenderSectionDto::CodeBlock {
                id: 2,
                lang: "rust".into(),
                text: "fn main() {}".into(),
                line_count: 1,
            },
            RenderSectionDto::Members {
                id: 3,
                entries: vec![MemberRowDto {
                    key: sample_key(),
                    name: "deserialize".into(),
                    signature: sample_signature(),
                    kind: KindDto("Function".into()),
                    visibility: VisibilityDto::Crate,
                }],
            },
            RenderSectionDto::Fields {
                id: 4,
                entries: vec![FieldRowDto {
                    key: sample_key(),
                    name: "inner".into(),
                    ty: sample_signature(),
                    kind: KindDto("Field".into()),
                }],
            },
            RenderSectionDto::Examples { id: 5, blocks },
            RenderSectionDto::Callout {
                id: 6,
                level: CalloutLevelDto::Warning,
                blocks: vec![ProseBlockDto::Rule],
            },
            RenderSectionDto::Unknown { id: 7, tag: "future-kind".into() },
        ],
    };
    assert_round_trips(&value, "SymbolDoc");
}

#[test]
fn usages_result_round_trips() {
    let value = UsagesResult {
        usages: vec![UsageRow {
            key: sample_key(),
            name: "from_str".to_owned(),
            kind: "Function".to_owned(),
        }],
        truncated: false,
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
            cells: vec![sample_key().0, "Deserializer".to_owned()],
        }],
        truncated: true,
    };
    assert_round_trips(&value, "QueryResult");
}

#[test]
fn schema_result_round_trips() {
    let value = SchemaResult { schema: nudox_mcp::SCHEMA_SDL.to_owned() };
    assert_round_trips(&value, "SchemaResult");
}

#[test]
fn tool_arguments_round_trip() {
    assert_round_trips(
        &SearchSymbolsArgs {
            query: "Deserializer".into(),
            kinds: Some(vec!["Trait".into()]),
            packages: Some(vec!["cargo:serde".into()]),
            limit: Some(25),
        },
        "SearchSymbolsArgs",
    );
    assert_round_trips(&GetSymbolArgs { key: sample_key() }, "GetSymbolArgs");
    assert_round_trips(
        &FindUsagesArgs { key: sample_key(), limit: None },
        "FindUsagesArgs",
    );
    assert_round_trips(&ListPackagesArgs {}, "ListPackagesArgs");
    assert_round_trips(
        &GraphQueryArgs {
            query: "query { Packages { lineage @output } }".into(),
            args: Some([("key".to_owned(), sample_key().0)].into_iter().collect()),
            limit: Some(10),
        },
        "GraphQueryArgs",
    );
    assert_round_trips(&GraphSchemaArgs {}, "GraphSchemaArgs");
}

#[test]
fn symbol_keys_are_plain_strings_on_the_wire() {
    // LR-1: the key is one value, spelled one way. If this ever serialised as
    // an object, keys would stop being copy-pasteable between tools.
    let json = serde_json::to_string(&sample_key()).expect("key must serialise");
    assert!(json.starts_with('"'), "SymbolKey must be a JSON string, got {json}");
    assert!(json.contains("cargo:serde#"));
}
