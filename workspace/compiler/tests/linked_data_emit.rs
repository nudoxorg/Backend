//! Pipeline part: **IR → linked data (JSON-LD)** (`compiler::generate::linked_data`).
//!
//! Specs for emitting an `ir::Index` into the graph-model document stream
//! (GRAPH-ARCHITECTURE.md §5): Package → bare Symbols → full Symbols →
//! reified relations → version membership, with first-write-wins dedup and a
//! deterministic upload ordering. Supersedes the old `document::emit`
//! Entry/Kind contract.

use std::path::PathBuf;

use compiler::error::GenerateError;
use compiler::generate::linked_data::{DocumentSink, emit};
use compiler::graph::link::PackageCtx;
use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Symbol, Visibility};
use ir::protocols::TraitDef;
use ir::record::Record;
use serde_json::Value;

// ─── Test double ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Collector {
    docs: Vec<Value>,
    flushed: bool,
}

impl DocumentSink for Collector {
    fn write(&mut self, document: Value) -> Result<(), GenerateError> {
        self.docs.push(document);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), GenerateError> {
        self.flushed = true;
        Ok(())
    }
}

// ─── Fixture builders ────────────────────────────────────────────────────────

fn local(segments: &str) -> NudoxPath {
    NudoxPath::Local(PathBuf::from(segments))
}

fn symbol<T>(name: &str, path: NudoxPath, inner: T) -> Symbol<T> {
    Symbol {
        name: name.to_string(),
        path,
        aliases: None,
        visibility: Visibility::Public,
        documentation: None,
        inner,
    }
}

fn record(name: &str, path: NudoxPath, implemented_protocols: Option<Vec<NudoxPath>>) -> Entry {
    Entry::RecordType(symbol(
        name,
        path,
        Record {
            name: Some(name.to_string()),
            generics: None,
            fields: vec![],
            call_signatures: None,
            constructors: None,
            methods: None,
            index_signatures: None,
            super_types: None,
            implemented_protocols,
            members: None,
        },
    ))
}

fn trait_def(name: &str, path: NudoxPath) -> Entry {
    Entry::TraitDef(symbol(
        name,
        path,
        TraitDef {
            generics: None,
            super_traits: None,
            associated_types: None,
            properties: None,
            required_methods: None,
            provided_methods: None,
            required_constants: None,
            attributes: None,
            members: None,
        },
    ))
}

fn index_of(entries: Vec<Entry>) -> Index {
    Index {
        root_ids: vec![],
        entries_by_path: entries.into_iter().map(|e| (e.path().clone(), e)).collect(),
    }
}

fn ctx() -> PackageCtx {
    PackageCtx {
        language: "rust".into(),
        package: "pkg".into(),
        version: Some("1.0.0".into()),
    }
}

fn emit_all(index: &Index) -> Collector {
    let mut sink = Collector::default();
    emit(index, ctx(), &mut sink).expect("emission succeeds");
    assert!(sink.flushed, "emit must flush the sink once at the end");
    sink
}

fn symbol_docs<'a>(docs: &'a [Value], fq_name: &str) -> Vec<&'a Value> {
    docs.iter()
        .filter(|d| d["@type"] == "Symbol" && d["fq_name"] == fq_name)
        .collect()
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// Each entry emits its Symbol document, alongside the owning Package node.
///
/// Act: emit a single `RecordType` entry.
/// Assert: the stream contains a `@type = "Symbol"` document for the entry
///   (typed, `@id`-addressed) and the `@type = "Package"` document it hangs off.
#[test]
fn entry_emits_entry_and_kind_documents() {
    let sink = emit_all(&index_of(vec![record("Widget", local("pkg/Widget"), None)]));

    let widgets = symbol_docs(&sink.docs, "pkg::Widget");
    assert!(!widgets.is_empty(), "a Symbol document for Widget is emitted");
    for doc in &widgets {
        assert!(doc.get("@id").is_some(), "symbol documents are @id-addressed: {doc}");
    }

    let packages: Vec<&Value> =
        sink.docs.iter().filter(|d| d["@type"] == "Package").collect();
    assert_eq!(packages.len(), 1, "exactly one Package node: {:?}", sink.docs);
    assert_eq!(packages[0]["name"], "pkg");
}

/// Entry documents carry protocol references as resolved IRIs.
///
/// Assert: implemented protocols are emitted as resolved symbol IRIs
///   (`Symbol/{lang}%2F{pkg}%2F{fq}` — EntityIDFor requires a single id
///   segment, so the logical `Symbol/{lang}/{pkg}/{fq}` hierarchy is
///   `%2F`-encoded; see `graph::link::symbol_iri`), not raw `NudoxPath`s.
#[test]
fn entry_document_resolves_member_uris() {
    let index = index_of(vec![
        record("Widget", local("pkg/Widget"), Some(vec![local("pkg/Greeter")])),
        trait_def("Greeter", local("pkg/Greeter")),
    ]);
    let sink = emit_all(&index);

    // The full (wave-3) Widget document carries the implements edge.
    let widget = symbol_docs(&sink.docs, "pkg::Widget")
        .into_iter()
        .find(|d| {
            d.get("implements").map(|i| i.to_string().contains("Symbol/")).unwrap_or(false)
        })
        .expect("a Widget document with a populated implements edge");

    let implements = widget["implements"].to_string();
    // path `pkg/Greeter` → fq `pkg::Greeter` → Symbol/rust%2Fpkg%2Fpkg::Greeter
    assert!(
        implements.contains("Symbol/rust%2Fpkg%2Fpkg::Greeter"),
        "implements must hold the resolved (encoded) IRI, got {implements}"
    );
}

/// A conflicting body for an already-emitted `@id` reports instead of
/// overwriting.
///
/// Arrange: two distinct paths whose sanitized IRIs collide (`?` and `!` both
///   collapse to `_`), giving the same `@id` two different bodies.
/// Assert: `emit` surfaces the conflict as an error. (The identical-body
///   re-emission no-op is unreachable through `project()` — two entries can
///   only share an `@id` via sanitize collision, and then their `fq_name`s
///   differ — so only the conflict half is observable here.)
#[test]
fn duplicate_uri_is_first_write_wins() {
    let index = index_of(vec![
        record("a?b", local("pkg/a?b"), None),
        record("a!b", local("pkg/a!b"), None),
    ]);

    let mut sink = Collector::default();
    let result = emit(&index, ctx(), &mut sink);

    assert!(
        matches!(result, Err(GenerateError::EmitLinkedData(_))),
        "colliding @ids with different bodies must error, got {result:?}"
    );
}

/// The corpus streams in wave order, deterministically.
///
/// Assert: Package documents precede Symbol documents, the PackageVersion
///   membership document comes last, every symbol appears once bare and once
///   full, and two emissions of the same index produce byte-identical streams.
#[test]
fn corpus_upload_order_is_deterministic() {
    let index = index_of(vec![
        record("Widget", local("pkg/Widget"), Some(vec![local("pkg/Greeter")])),
        trait_def("Greeter", local("pkg/Greeter")),
    ]);

    let sink = emit_all(&index);
    let kinds: Vec<&str> =
        sink.docs.iter().map(|d| d["@type"].as_str().unwrap_or_default()).collect();

    assert_eq!(kinds.first(), Some(&"Package"), "packages lead: {kinds:?}");
    assert_eq!(kinds.last(), Some(&"PackageVersion"), "version membership trails: {kinds:?}");

    let first_symbol = kinds.iter().position(|k| *k == "Symbol").expect("symbols present");
    let last_package = kinds.iter().rposition(|k| *k == "Package").expect("package present");
    assert!(last_package < first_symbol, "every Package precedes every Symbol: {kinds:?}");

    // Bare wave + full wave: each projected symbol document appears twice.
    let widgets = symbol_docs(&sink.docs, "pkg::Widget");
    assert_eq!(widgets.len(), 2, "one bare + one full document per symbol");

    let again = emit_all(&index);
    assert_eq!(sink.docs, again.docs, "emission is deterministic");
}

/// A whole `Index` emits without losing any entry.
///
/// Assert: emitting an index of N entries yields Symbol documents covering all N.
#[test]
fn whole_index_emits_every_entry() {
    let names = ["Alpha", "Beta", "Gamma", "Delta", "Epsilon"];
    let entries: Vec<Entry> = names
        .iter()
        .map(|name| record(name, local(&format!("pkg/{name}")), None))
        .collect();
    let sink = emit_all(&index_of(entries));

    for name in names {
        let fq = format!("pkg::{name}");
        assert!(
            !symbol_docs(&sink.docs, &fq).is_empty(),
            "entry {fq} must be represented in the corpus"
        );
    }
}
