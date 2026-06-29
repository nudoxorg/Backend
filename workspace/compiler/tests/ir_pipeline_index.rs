//! Pipeline part: **IR collection → index** (`ir::pipeline`).
//!
//! TDD specs for the typestate builder that turns a flat `Vec<Entry>` into an
//! addressable `Index` (the `Ir<Collected>` → `Ir<Indexed>` transition).

/// Collecting entries then indexing yields lookup by path.
///
/// Act: `Ir::from_entries(entries).index()`.
/// Assert: every entry is retrievable via `Index::get(path)`.
#[test]
fn indexing_makes_entries_retrievable_by_path() {
    todo!("assert get(path) returns each collected entry");
}

/// Top-level local items become `root_ids`.
///
/// Assert: a `NudoxPath::Local` with a single path component is pushed into
///   `root_ids`; deeper paths are not.
#[test]
fn single_segment_local_paths_become_roots() {
    todo!("assert root_ids contains only top-level local paths");
}

/// External entries are indexed but never treated as roots.
///
/// Assert: a `NudoxPath::External` entry is in `entries_by_path` but absent from
///   `root_ids`.
#[test]
fn external_entries_are_not_roots() {
    todo!("assert external entries are indexed but not roots");
}

/// An empty collection produces an empty, well-formed index.
///
/// Assert: `is_empty()` is true and iterating yields nothing.
#[test]
fn empty_collection_indexes_to_empty() {
    todo!("assert an empty Ir indexes to an empty Index");
}
