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

    SymbolHead {
        key,
        breadcrumb,
        signature,
        kind,
        visibility,
        provenance,
        deprecation,
        section_plan,
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
