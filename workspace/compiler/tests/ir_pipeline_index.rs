//! Pipeline part: **IR collection → index** (`ir::pipeline`).
//!
//! Specs for the typestate builder that turns a flat `Vec<Entry>` into an
//! addressable `Index` (the `Ir<Collected>` → `Ir<Indexed>` transition).

use std::path::PathBuf;

use ir::entry::NudoxPath;
use ir::kind::{Entry, Symbol, Visibility};
use ir::pipeline::Ir;

fn constant_at(path: NudoxPath) -> Entry {
    Entry::Constant(Symbol {
        name: path_leaf(&path),
        path,
        aliases: None,
        visibility: Visibility::Public,
        documentation: None,
        inner: (),
    })
}

fn path_leaf(path: &NudoxPath) -> String {
    let buf = match path {
        NudoxPath::Local(p) => p,
        NudoxPath::External { path, .. } => path,
    };
    buf.iter().next_back().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn local(segments: &str) -> NudoxPath {
    NudoxPath::Local(PathBuf::from(segments))
}

/// Collecting entries then indexing yields lookup by path.
///
/// Act: `Ir::from_entries(entries).index()`.
/// Assert: every entry is retrievable via `Index::get(path)`.
#[test]
fn indexing_makes_entries_retrievable_by_path() {
    let paths = [local("alpha"), local("alpha/beta"), local("alpha/beta/gamma")];
    let entries: Vec<Entry> = paths.iter().cloned().map(constant_at).collect();

    let indexed = Ir::from_entries(entries).index();

    assert_eq!(indexed.len(), paths.len());
    for path in &paths {
        let entry = indexed.get(path).unwrap_or_else(|| panic!("{path:?} retrievable"));
        assert_eq!(entry.path(), path);
    }
}

/// Top-level local items become `root_ids`.
///
/// Assert: a `NudoxPath::Local` with a single path component is pushed into
///   `root_ids`; deeper paths are not.
#[test]
fn single_segment_local_paths_become_roots() {
    let entries = vec![
        constant_at(local("alpha")),
        constant_at(local("alpha/beta")),
        constant_at(local("alpha/beta/gamma")),
    ];

    let indexed = Ir::from_entries(entries).index();

    assert_eq!(indexed.root_ids(), &[local("alpha")]);
}

/// External entries are indexed but never treated as roots.
///
/// Assert: a `NudoxPath::External` entry is in `entries_by_path` but absent from
///   `root_ids`.
#[test]
fn external_entries_are_not_roots() {
    let external = NudoxPath::External {
        dependency: "serde".to_string(),
        path: PathBuf::from("Serialize"),
    };
    let entries = vec![constant_at(local("alpha")), constant_at(external.clone())];

    let indexed = Ir::from_entries(entries).index();

    assert!(indexed.get(&external).is_some(), "external entry is indexed");
    assert!(
        !indexed.root_ids().contains(&external),
        "external entry must not be a root"
    );
    assert_eq!(indexed.root_ids(), &[local("alpha")]);
}

/// An empty collection produces an empty, well-formed index.
///
/// Assert: `is_empty()` is true and iterating yields nothing.
#[test]
fn empty_collection_indexes_to_empty() {
    let collected = Ir::from_entries(Vec::new());
    assert!(collected.is_empty());

    let indexed = collected.index();

    assert!(indexed.is_empty());
    assert_eq!(indexed.iter().count(), 0);
    assert!(indexed.root_ids().is_empty());
}
