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

use nudox_engine::wire::{
    CalloutLevel, GenerationId, HitRow, KindDiscriminant, KindTag, LangId, MemberRow, Provenance,
    RenderSection, SectionId, SectionKind, SectionPlan, SigToken, SizeHint, SymbolHead, Visibility,
};
use nudox_mcp::SymbolKeyDto;
use nudox_mcp::tools::{
    FindUsagesArgs, GetSymbolArgs, GraphQueryArgs, GraphSchemaArgs, ListPackagesArgs,
    PackageSummary, PackagesResult, QueryResult, QueryResultRow, SchemaResult, SearchResult,
    SearchSymbolsArgs, SymbolDoc, UsageRow, UsagesResult,
};
use schemars::schema_for;
use serde::Serialize;

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

/// Serialise a value and assert the JSON is non-empty.
///
/// `Deserialize` is not available for wire types containing `SigToken`, so we
/// do a one-way serialise check here rather than a full round-trip.
fn assert_serialises<T: Serialize>(value: &T, what: &str) {
    let json =
        serde_json::to_string(value).unwrap_or_else(|e| panic!("{what}: serialise: {e}"));
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
    let back: T = serde_json::from_str(&json)
        .unwrap_or_else(|e| panic!("{what}: deserialise: {e} ({json})"));
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
        breadcrumb: vec![nudox_engine::wire::CrumbRef {
            key: sample_wire_key(),
            label: nudox_engine::wire::SharedStr::from("serde"),
        }],
        signature: sample_sig(),
        kind: KindTag::Known(KindDiscriminant::from_u16(3).expect("Function discriminant")),
        visibility: Visibility::Public,
        provenance: Provenance::TrustedLocal,
        deprecation: Some(nudox_engine::wire::SharedStr::from("use `from_str` instead")),
        section_plan: vec![SectionPlan {
            id: SectionId(1),
            kind: SectionKind::Prose,
            size_hint: SizeHint::Lines(12),
        }],
    })
}

#[test]
fn search_result_serialises() {
    use std::sync::Arc;
    let value = SearchResult {
        hits: vec![HitRow {
            key: sample_wire_key(),
            display_name: nudox_engine::wire::SharedStr::from("serde::de::Deserializer"),
            sig_preview: sample_sig(),
            kind: KindTag::Known(KindDiscriminant::from_u16(5).expect("Trait discriminant")),
            provenance: Provenance::SyncedLocal { generation: GenerationId(7) },
            score: 0.875,
        }],
        truncated: true,
    };
    let json = serde_json::to_string(&value).expect("SearchResult must serialise");
    // Key must be the canonical string form.
    assert!(
        json.contains("cargo:serde#"),
        "hit key must be ecosystem:name#introhex string: {json}"
    );
    // Provenance must have a kind discriminant.
    assert!(json.contains("synced_local"), "provenance kind must appear: {json}");
    // truncated flag must be present.
    assert!(json.contains("\"truncated\":true"), "truncated must serialise: {json}");
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
        sections: vec![
            RenderSection::Prose { id: SectionId(1), blocks: blocks.clone() },
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
    assert!(json.contains("\"kind\":\"prose\""), "prose section must be tagged: {json}");
    assert!(json.contains("\"kind\":\"code_block\""), "code_block section must be tagged: {json}");
    assert!(json.contains("\"kind\":\"members\""), "members section must be tagged: {json}");
    assert!(json.contains("\"kind\":\"callout\""), "callout section must be tagged: {json}");
    assert!(json.contains("\"kind\":\"unknown\""), "unknown section must be tagged: {json}");
}

#[test]
fn usages_result_round_trips() {
    let value = UsagesResult {
        usages: vec![UsageRow {
            key: sample_key_dto(),
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
            cells: vec![sample_key_dto().0, "Deserializer".to_owned()],
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
    assert_round_trips(&GetSymbolArgs { key: sample_key_dto() }, "GetSymbolArgs");
    assert_round_trips(
        &FindUsagesArgs { key: sample_key_dto(), limit: None },
        "FindUsagesArgs",
    );
    assert_round_trips(&ListPackagesArgs {}, "ListPackagesArgs");
    assert_round_trips(
        &GraphQueryArgs {
            query: "query { Packages { lineage @output } }".into(),
            args: Some([("key".to_owned(), sample_key_dto().0)].into_iter().collect()),
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
    let json = serde_json::to_string(&sample_key_dto()).expect("key must serialise");
    assert!(json.starts_with('"'), "SymbolKey must be a JSON string, got {json}");
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
        key: key.clone(),
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
    assert!(json.contains("\"raw\":999"), "KindTag::Unknown must carry raw value: {json}");
}

#[test]
fn sig_token_kw_serialises_with_kind() {
    let token = SigToken::Kw("fn");
    let json = serde_json::to_string(&token).expect("SigToken must serialise");
    assert!(json.contains("\"kind\":\"kw\""), "SigToken::Kw must have kind: {json}");
    assert!(json.contains("\"text\":\"fn\""), "SigToken::Kw must have text: {json}");
}

#[test]
fn provenance_stale_serialises_unix_secs() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
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
