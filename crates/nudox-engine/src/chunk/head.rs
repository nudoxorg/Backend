//! Builds the `SymbolHead` — the first event on every `DocEvent` stream.
//!
//! # Breadcrumb construction
//!
//! The breadcrumb is the ancestor chain of `intro`, root-first.  We walk
//! `view.parent_of(intro)` upward until we hit `None`, then reverse.
//! Each ancestor becomes a `CrumbRef { key, label }`.
//!
//! # One-walk guarantee for section_plan
//!
//! `section_plan` is obtained by calling `plan::section_plan`, which calls
//! `walk::walk_doc` — the same function that `sections::sections` calls.
//! The two are guaranteed structurally identical (see `walk.rs` docs).

use nudox_ir::{change::IntroId, entry::Entry, view::IrView};
use nudox_store::package::PackageView;

use crate::wire::{CrumbRef, KindTag, Provenance, SharedStr, SymbolHead, SymbolKey};

// ---------------------------------------------------------------------------
// Source-location helpers
// ---------------------------------------------------------------------------

/// Convert `Symbol::source` and `Symbol::span` into the wire representation.
///
/// Returns `(None, None)` when the producer did not supply a source path
/// (i.e. `Symbol::source` is the empty path `""` or `.`).
///
/// The path is carried as-is from the IR — it may be absolute, relative to the
/// package root, or partially qualified depending on the producer.  No attempt
/// is made to make it relative here because `PackageView` holds no package root
/// path; that is a future producer concern.
///
/// The span is a **byte** range, not a line range.  Converting bytes to line
/// numbers would require reading the file, which is outside the engine's doc
/// path.  The GUI must not present these as line numbers.
fn source_location(entry: &Entry) -> (Option<SharedStr>, Option<[u32; 2]>) {
    let path = &entry.sym().source;

    // An empty or trivially-relative path means "not recorded".
    let path_str = path.to_string_lossy();
    if path_str.is_empty() || path_str == "." {
        return (None, None);
    }

    let span = &entry.sym().span;
    // `Range<usize>` → `[u32; 2]`.  Saturate rather than panic on producers
    // that emit unrealistic offsets.
    let wire_span = [
        u32::try_from(span.start).unwrap_or(u32::MAX),
        u32::try_from(span.end).unwrap_or(u32::MAX),
    ];

    (
        Some(SharedStr::from(path_str.as_ref())),
        Some(wire_span),
    )
}

/// Build the `SymbolHead` for `entry` at `intro`.
///
/// # Arguments
///
/// * `intro`   — stable identity of the entry (also used as the `key`).
/// * `entry`   — the declaration entry we are building a head for.
/// * `view`    — the `IrView` for this package (used for breadcrumb walk).
/// * `package` — the `PackageView` (used for provenance, signature, and plan).
pub fn head(intro: IntroId, entry: &Entry, view: &IrView, package: &PackageView) -> SymbolHead {
    // ── Stable key ────────────────────────────────────────────────────────────

    let key = SymbolKey::new(package.lineage().clone(), intro);

    // ── Breadcrumb ────────────────────────────────────────────────────────────
    //
    // Walk from `intro` upward through parent links, collecting each ancestor's
    // (IntroId, name).  The walk stops at `None`.  We then reverse to produce
    // the root-first breadcrumb.  `intro` itself is NOT included — the
    // breadcrumb shows ancestors only, not the symbol itself.

    let mut ancestors: Vec<(IntroId, String)> = Vec::new();
    let mut cursor = view.parent_of(intro);
    while let Some(parent_id) = cursor {
        let label = view
            .entry(parent_id)
            .map(|e| e.sym().name.clone())
            .unwrap_or_else(|| "?".to_owned());
        ancestors.push((parent_id, label));
        cursor = view.parent_of(parent_id);
    }
    ancestors.reverse(); // root-first

    let breadcrumb: Vec<CrumbRef> = ancestors
        .into_iter()
        .map(|(id, label)| CrumbRef {
            key: SymbolKey::new(package.lineage().clone(), id),
            label: SharedStr::from(label.as_str()),
        })
        .collect();

    // ── Signature ─────────────────────────────────────────────────────────────

    let signature = super::signature::tokens(entry, package);

    // ── Kind ──────────────────────────────────────────────────────────────────

    let kind = entry
        .kind()
        .discriminant()
        .map(KindTag::Known)
        .unwrap_or(KindTag::Unknown(0));

    // ── Visibility ────────────────────────────────────────────────────────────

    let visibility = entry.sym().visibility;

    // ── Provenance ────────────────────────────────────────────────────────────

    let provenance = store_provenance_to_wire(package.provenance());

    // ── Deprecation ───────────────────────────────────────────────────────────
    //
    // `Symbol::deprecation` is `Option<Deprecation>` where `Deprecation` has a
    // `note: Option<String>` field.  We render the note text when present, or
    // fall back to an empty marker when the struct exists but the note is absent.

    let deprecation = entry.sym().deprecation.as_ref().map(|d| {
        if let Some(note) = d.note.as_deref() {
            SharedStr::from(note)
        } else if let Some(since) = d.since.as_deref() {
            SharedStr::from(format!("Deprecated since {}", since).as_str())
        } else {
            SharedStr::from("Deprecated")
        }
    });

    // ── Section plan ──────────────────────────────────────────────────────────
    //
    // `plan::section_plan` calls `walk::walk_doc` — the same function that
    // `sections::sections` calls — so the plan and sections are structurally
    // identical (one-walk guarantee).

    let section_plan = super::plan::section_plan(intro, entry, view, package);

    // ── Source location ───────────────────────────────────────────────────────

    let (source_path, source_span) = source_location(entry);

    SymbolHead {
        key,
        breadcrumb,
        signature,
        kind,
        visibility,
        provenance,
        deprecation,
        section_plan,
        source_path,
        source_span,
    }
}

/// Convert `nudox_store::package::Provenance` to the wire `Provenance`.
///
/// The store has a smaller provenance vocabulary than the wire (which includes
/// `SyncedLocal`, `Remote`, and `Stale` for distributed corpus scenarios).
/// Both store variants map to `TrustedLocal` — local IR produced in this
/// session is always trusted.  Future store variants are handled by `_ =>
/// TrustedLocal` so that adding a new store tier never silently panics.
fn store_provenance_to_wire(p: nudox_store::package::Provenance) -> Provenance {
    use nudox_store::package::Provenance as StoreP;
    match p {
        StoreP::TrustedLocal | StoreP::SnapshotLocal => Provenance::TrustedLocal,
        _ => Provenance::TrustedLocal,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_store::{
        package::{PackageView, Provenance as StoreProvenance},
        source::fixtures::build_rich_view,
    };

    #[test]
    fn head_key_matches_intro() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert_eq!(h.key.intro, intro, "head key.intro must equal the intro for '{}'", entry.sym().name);
            assert_eq!(&h.key.package, pkg.lineage(), "head key.package must equal the lineage");
        }
    }

    #[test]
    fn head_signature_non_empty() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(!h.signature.is_empty(), "signature must not be empty for '{}'", entry.sym().name);
        }
    }

    #[test]
    fn breadcrumb_root_has_no_ancestors() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        // The root entry (the one with no parent) should have an empty breadcrumb.
        let root = pkg.view().entries().find(|(id, _)| pkg.view().parent_of(*id).is_none());
        if let Some((intro, entry)) = root {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(h.breadcrumb.is_empty(), "root must have empty breadcrumb");
        }
    }

    #[test]
    fn breadcrumb_child_is_non_empty_when_parent_exists() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        // A child entry (one that has a parent) must have a non-empty breadcrumb.
        let child = pkg.view().entries().find(|(id, _)| pkg.view().parent_of(*id).is_some());
        if let Some((intro, entry)) = child {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(!h.breadcrumb.is_empty(), "child must have non-empty breadcrumb for '{}'", entry.sym().name);
        }
    }

    #[test]
    fn section_plan_matches_sections() {
        use super::super::sections::sections;

        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            let sects = sections(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.section_plan.len(),
                sects.len(),
                "head.section_plan must match sections for '{}'",
                entry.sym().name
            );
            for (p, s) in h.section_plan.iter().zip(sects.iter()) {
                assert_eq!(p.id, s.section_id(), "plan/section id mismatch for '{}'", entry.sym().name);
            }
        }
    }

    /// Helper that builds a minimal `Entry` — just enough to exercise
    /// `source_location`.  We use a module kind because `Module` is a unit
    /// struct and requires no extra data.
    fn minimal_entry(source: &str, span: std::ops::Range<usize>) -> nudox_ir::entry::Entry {
        use nudox_ir::{
            entry::{Node, Symbol, Visibility},
            index::RawRef,
            kind::Kind,
            kinds::Module,
        };
        let sym = Symbol {
            name: "test_sym".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from(source),
            span,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        nudox_ir::entry::Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
    }

    #[test]
    fn source_location_roundtrips() {
        let entry = minimal_entry("src/lib.rs", 10..42);
        let (path, span) = source_location(&entry);
        assert!(path.is_some(), "non-empty source should produce a path");
        assert_eq!(&**path.as_ref().unwrap(), "src/lib.rs");
        assert_eq!(span, Some([10u32, 42u32]));
    }

    #[test]
    fn source_location_empty_source_returns_none() {
        let entry = minimal_entry("", 0..0);
        let (path, span) = source_location(&entry);
        assert!(path.is_none(), "empty source path must yield None");
        assert!(span.is_none());
    }

    /// The rich fixture uses `PathBuf::new()` (empty) for all symbols, so every
    /// head should carry `None` for the source fields — this confirms we are not
    /// fabricating paths.
    #[test]
    fn head_source_is_none_when_fixture_has_empty_paths() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(
                h.source_path.is_none(),
                "fixture uses empty paths — source_path must be None for '{}'",
                entry.sym().name
            );
            assert!(
                h.source_span.is_none(),
                "fixture uses empty paths — source_span must be None for '{}'",
                entry.sym().name
            );
        }
    }

    #[test]
    fn provenance_is_trusted_local_for_fixture() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert_eq!(
                h.provenance,
                crate::wire::Provenance::TrustedLocal,
                "fixture provenance must be TrustedLocal"
            );
        }
    }

    #[test]
    fn kind_tag_known_for_all_fixture_entries() {
        let view = build_rich_view();
        let pkg = PackageView::build(view, StoreProvenance::TrustedLocal);

        for (intro, entry) in pkg.view().entries() {
            let h = head(intro, entry, pkg.view(), &pkg);
            assert!(
                matches!(h.kind, KindTag::Known(_)),
                "all fixture entries should have Known kind, got {:?} for '{}'",
                h.kind,
                entry.sym().name
            );
        }
    }
}
