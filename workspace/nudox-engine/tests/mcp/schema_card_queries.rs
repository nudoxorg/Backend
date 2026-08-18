//! The worked example queries embedded in [`nudox_engine::mcp::SCHEMA_CARD`]
//! must actually parse against the real schema.
//!
//! `graph_schema`'s default response (the compact reference card,
//! `docs/MCP-SURFACE-PLAN.md` §1.2) trades the full SDL's copy-pasted
//! doc-comments for worked Trustfall queries an agent can adapt directly.
//! An example that does not parse is worse than no example — it teaches an
//! agent to write a query the adapter rejects. This test extracts every
//! ` ```graphql ` fence from the card and parses each one against
//! [`nudox_engine::graph::schema()`] the same way
//! `tests/graph/queries_parse.rs` validates the production `.trustfall`
//! query files, so a card edit that breaks an example fails loudly here
//! rather than shipping.

use std::sync::Arc;

use nudox_engine::graph::adapter::CorpusAdapter;
use nudox_engine::mcp::SCHEMA_CARD;
use nudox_engine::store::corpus::Corpus;
use trustfall::FieldValue;
use trustfall::execute_query_async;

fn empty_adapter() -> Arc<CorpusAdapter> {
    Arc::new(CorpusAdapter::new(Corpus::new()))
}

/// Every fenced ` ```graphql ... ``` ` block in `text`, in order.
fn extract_graphql_fences(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.by_ref().next() {
        if line.trim() != "```graphql" {
            continue;
        }
        let mut body = String::new();
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                break;
            }
            body.push_str(inner);
            body.push('\n');
        }
        blocks.push(body);
    }
    blocks
}

/// A plausible value for every variable name a card example references.
///
/// Mirrors `tests/graph/queries_parse.rs`'s `dummy_value`: every field the
/// card's examples filter on (`Trait.name`, `Symbol.name`, `Symbol.key`) is
/// declared `String!` in `schema.graphql`, so a string dummy is always
/// schema-correct here — there is no boolean/int filter in any card
/// example to special-case.
fn dummy_value(name: &str) -> FieldValue {
    match name {
        "key" => FieldValue::String(
            "cargo:demo#0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        _ => FieldValue::String("Display".into()),
    }
}

/// Scan `query` for `$ident` variable references and build a binding map.
fn vars_for(query: &str) -> std::collections::BTreeMap<String, FieldValue> {
    let mut names: Vec<String> = Vec::new();
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                names.push(query[start..end].to_owned());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|n| (n.clone(), dummy_value(&n)))
        .collect()
}

#[test]
fn every_worked_query_in_the_schema_card_parses() {
    let schema = nudox_engine::graph::schema();
    let adapter = empty_adapter();

    let queries = extract_graphql_fences(SCHEMA_CARD);
    assert!(
        queries.len() >= 2,
        "the schema card should carry at least two worked examples; found {}",
        queries.len()
    );

    for (i, query) in queries.iter().enumerate() {
        let _stream = execute_query_async(schema, Arc::clone(&adapter), query, vars_for(query))
            .unwrap_or_else(|e| {
                panic!("schema card example #{i} failed to parse:\n{e}\n\nQuery text:\n{query}")
            });
    }
}

/// Every type and field name the SDL declares must appear **verbatim** in the
/// card.
///
/// The card is the only schema most agents will ever read, so a name it
/// contracts is a name they cannot write. This caught a real defect on
/// 2026-08-15: `SourceLocation`'s six positional fields had been compressed to
/// `byteStart/End`, `startLine/Column`, `endLine/Column`, which names three
/// fields that exist and three that do not — an agent copying them writes a
/// query the schema rejects.
///
/// Comment lines are stripped from the SDL before extraction so this asserts
/// over declarations only, not prose.
#[test]
fn the_card_names_every_type_and_field_the_sdl_declares() {
    let sdl: String = nudox_engine::mcp::SCHEMA_SDL
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    let mut missing = Vec::new();

    for line in sdl.lines() {
        let trimmed = line.trim_start();
        // `type Foo {` / `interface Bar {`
        if let Some(rest) = trimmed
            .strip_prefix("type ")
            .or_else(|| trimmed.strip_prefix("interface "))
        {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if !name.is_empty() && !SCHEMA_CARD.contains(name) {
                missing.push(format!("type {name}"));
            }
            continue;
        }
        // `  fieldName: Type` / `  fieldName(args): Type`, indented inside a body
        if line.starts_with("  ") && trimmed.contains(':') {
            let name: String = trimmed
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.len() > 1 && !SCHEMA_CARD.contains(&name) {
                missing.push(format!("field {name}"));
            }
        }
    }

    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "the schema card omits {} name(s) the SDL declares, so an agent cannot \
         write them: {missing:?}",
        missing.len()
    );
}

#[test]
fn schema_card_is_non_empty_and_documents_the_key_format() {
    assert!(!SCHEMA_CARD.is_empty(), "the schema card must not be empty");
    assert!(
        SCHEMA_CARD.contains("ecosystem:name#introhex"),
        "the schema card must document the key format every tool consumes"
    );
    assert!(
        SCHEMA_CARD.contains("full: true"),
        "the schema card must tell the agent how to reach the full SDL"
    );
}
