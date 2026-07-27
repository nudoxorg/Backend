//! LR-7: one schema.
//!
//! `graph_schema` must serve exactly the SDL that `nudox-graph` parses. These
//! tests pin that, because the two crates reach the file by separate
//! `include_str!`s (see the note on `nudox_mcp::SCHEMA_SDL`) and a divergence
//! would silently teach agents to write queries the adapter rejects.

use std::path::PathBuf;

/// The schema file `nudox-graph` compiles in.
fn graph_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../nudox-graph/schema.graphql")
}

#[test]
fn served_sdl_is_byte_identical_to_the_graph_crates_schema_file() {
    let on_disk = std::fs::read_to_string(graph_schema_path())
        .expect("crates/nudox-graph/schema.graphql must exist");
    assert_eq!(
        nudox_mcp::SCHEMA_SDL, on_disk,
        "SCHEMA_SDL has drifted from nudox-graph/schema.graphql — LR-7 says there is one schema"
    );
}

#[test]
fn served_sdl_parses_to_the_same_schema_nudox_graph_exposes() {
    // `trustfall::Schema` keeps no source text and exposes no way to render
    // itself back to SDL, so equality is established structurally: the text we
    // serve must parse, and the resulting schema must agree with
    // `nudox_graph::schema()` about the type hierarchy an agent will query
    // against. `subtypes` yields names in sorted order, so this comparison is
    // deterministic.
    let parsed = trustfall::Schema::parse(nudox_mcp::SCHEMA_SDL)
        .expect("the SDL served to agents must be a valid Trustfall schema");
    let theirs = nudox_graph::schema();

    for interface in ["Symbol", "Package", "Occurrence"] {
        let ours: Vec<&str> = parsed
            .subtypes(interface)
            .unwrap_or_else(|| panic!("served schema must define {interface}"))
            .collect();
        let reference: Vec<&str> = theirs
            .subtypes(interface)
            .unwrap_or_else(|| panic!("nudox-graph schema must define {interface}"))
            .collect();
        assert_eq!(
            ours, reference,
            "graph_schema and the adapter disagree about the {interface} hierarchy"
        );
    }

    // The implementors the tool descriptions name must actually be there.
    let symbol_subtypes: Vec<&str> = parsed
        .subtypes("Symbol")
        .expect("Symbol must be defined")
        .collect();
    for implementor in ["Function", "Record", "Trait", "Impl", "Enum", "Field", "Const", "Alias"] {
        assert!(
            symbol_subtypes.contains(&implementor),
            "schema must define the {implementor} implementor that graph_query's description \
             promises"
        );
    }
}

#[test]
fn served_sdl_documents_the_key_format_agents_must_use() {
    // Tool descriptions promise `ecosystem:name#introhex`; the schema an agent
    // reads must say the same thing, or the two documentations disagree.
    assert!(
        nudox_mcp::SCHEMA_SDL.contains("ecosystem:name#introhex"),
        "schema must document the key format the tools consume"
    );
}
