//! Pipeline part: **IR → linked data (JSON-LD)** (`compiler::linked_data`).
//!
//! TDD specs for emitting an `ir::Index` into the Entry + Kind document pair
//! TerminusDB ingests, with first-write-wins dedup and a deterministic upload
//! ordering. Mirrors the old `document::emit` contract.

/// Each entry emits an Entry document and a Kind document.
///
/// Act: emit a single `RecordType` entry.
/// Assert: two documents are produced — `@type = "Entry"` (referencing the Kind
///   `@id`) and the Kind document for the record.
#[test]
fn entry_emits_entry_and_kind_documents() {
    todo!("assert an entry emits its Entry + Kind docs");
}

/// Entry documents carry path, fq_name, members, and protocol references as URIs.
///
/// Assert: members / implemented_protocols are emitted as resolved `EntryUri`s,
///   not raw paths.
#[test]
fn entry_document_resolves_member_uris() {
    todo!("assert members/protocols are emitted as URIs");
}

/// Emitting the same URI twice is first-write-wins; conflicting bodies report.
///
/// Assert: re-emitting an identical doc is a no-op; a differing body for the
///   same `@id` surfaces a duplicate/conflict error rather than overwriting.
#[test]
fn duplicate_uri_is_first_write_wins() {
    todo!("assert dedup + conflict reporting on duplicate URIs");
}

/// The corpus is ordered Kinds-first, then entries deepest-path-first.
///
/// Assert: when serialized for upload, Kind docs precede Entry docs, and within
///   entries the deepest paths come first (dependency-before-dependent).
#[test]
fn corpus_upload_order_is_deterministic() {
    todo!("assert Kinds-first / deepest-entry-first ordering");
}

/// A whole `Index` emits without losing any entry.
///
/// Assert: emitting an index of N entries yields documents covering all N.
#[test]
fn whole_index_emits_every_entry() {
    todo!("assert every index entry is represented in the corpus");
}
