//! Comparing two loaded generations of one package.
//!
//! # The question this answers
//!
//! "What changed between memchr 2.8.0 and 2.8.3." Before this module the only
//! route was `select_version` → query → `select_version` → query → diff by
//! hand, because the corpus has room for exactly one resident `PackageView`
//! per lineage (see [`crate::versions`]) and so no single query can see two
//! generations at once.
//!
//! # Why here, and not as a graph vertex or a second resident generation
//!
//! Three designs were on the table.
//!
//! * **A lineage-diff vertex in `nudox-graph`.** Rejected on the dependency
//!   law: the graph resolves against `crate::store::corpus::Corpus`, and the
//!   non-current generations live in this crate's [`VersionRegistry`], which
//!   sits *above* the graph. A vertex would need the edge inverted.
//! * **Multi-generation residency in the corpus.** Rejected on meaning before
//!   memory: `PackageLineageId` is version-free on purpose, and that is what
//!   makes `IntroId` continuity mean anything. Making the corpus hold several
//!   generations would make every *existing* query ambiguous about which world
//!   it is querying — a much larger change than the feature warrants. The
//!   memory cost is real too (corpus packages lower to 30k+ entries) but it is
//!   the second objection, not the first.
//! * **A computed answer in the engine, exposed as a tool.** Chosen. Both
//!   generations are *already* resident in the registry, so the diff costs no
//!   additional memory at all; it is a walk over two `IrView`s that the
//!   process is holding regardless.
//!
//! # The join key is not trustworthy, and that is the hard part
//!
//! The diff joins on `IntroId`. `IntroId` is minted through a disambiguator
//! ladder whose lower two rungs embed a byte span or an ordinal
//! (`nudox_ir::intro::Disambiguator`), so a declaration can be minted a
//! *different* key in the next release having changed nothing itself.
//!
//! A set-difference diff therefore reports such a declaration as **removed and
//! added**. That is not a cosmetic inaccuracy: "this API was deleted" is the
//! single most consequential thing a diff can say, and a diff that says it
//! wrongly is worse than no diff, because the reader cannot tell which rows to
//! discount.
//!
//! So this module does three things a set difference does not:
//!
//! 1. Re-pairs unmatched declarations on `(kind, fully-qualified path)`, which
//!    is exactly the tuple the sealer's own phase-A `path_id` is minted from
//!    and therefore the identity that survives a disambiguator change. A
//!    unique match on each side becomes one
//!    [`DiffVerdict::Rekeyed`] row, not two.
//! 2. Refuses to say `Removed` for a key that was not content-derived. If it
//!    cannot re-pair such a declaration it says
//!    [`DiffVerdict::Indeterminate`], carrying the tier as the evidence.
//! 3. Classifies the declarations present in both with
//!    [`super::timeline::classify`] — the same function `get_symbol`'s
//!    timeline uses — so the two surfaces cannot disagree.
//!
//! Step 2 depends entirely on `PackageView::key_tier`, which is why this
//! module could not have been written before the `SealReport` was carried
//! through the `nudox-store` boundary. On a package whose view carries no seal
//! report every disappearance is `Indeterminate`, and that is the correct
//! answer rather than a degraded one.
//!
//! [`VersionRegistry`]: crate::versions::VersionRegistry

use std::collections::HashMap;

use nudox_ir::change::{IntroId, PackageLineageId, StableRef};
use crate::store::package::PackageView;

use crate::{
    chunk::signature,
    versions::VersionSlice,
    wire::{DiffRow, DiffVerdict, KeyTierLabel, PackageDiff, SharedStr, TimelineChange},
};

use super::timeline::{self, Seen};

/// The identity a declaration keeps across a disambiguator change.
///
/// `(kind discriminant label, fully-qualified path)`. Not the `IntroId`, which
/// is the thing under suspicion; not the bare name, which collides across
/// modules; and not the signature, which is allowed to change.
///
/// The path comes from `PackageIndexes::path_of`, already precomputed for
/// every entry, so building the re-pairing map costs a hash per unmatched
/// declaration rather than a parent-chain walk.
type PathKey = (&'static str, SharedStr);

/// One side's view of a declaration, resolved once so the two passes below do
/// not each re-look-it-up.
struct Decl {
    intro: IntroId,
    path: SharedStr,
    name: SharedStr,
    kind: &'static str,
    tier: KeyTierLabel,
}

/// Diff `from` (older) against `to` (newer).
///
/// The two slices must belong to the same lineage; the caller
/// ([`crate::EngineHandle::diff_versions`]) pulls both from one
/// `VersionRegistry` entry, so that is structural rather than checked here.
pub(crate) fn build(
    package: &PackageLineageId,
    from: &VersionSlice,
    to: &VersionSlice,
) -> PackageDiff {
    let from_decls = declarations(&from.package);
    let to_decls = declarations(&to.package);

    let from_by_intro: HashMap<IntroId, &Decl> =
        from_decls.iter().map(|d| (d.intro, d)).collect();
    let to_by_intro: HashMap<IntroId, &Decl> = to_decls.iter().map(|d| (d.intro, d)).collect();

    let mut rows: Vec<DiffRow> = Vec::new();
    let mut unchanged: u32 = 0;

    // ── Pass 1: present in both under the same key ──────────────────────────
    //
    // Classified by `timeline::classify`, so a `SignatureChanged` here means
    // exactly what a `SignatureChanged` on the symbol page means.
    for decl in &to_decls {
        let Some(before) = from_by_intro.get(&decl.intro) else {
            continue;
        };
        let (Some(prev_entry), Some(cur_entry)) = (
            from.package.view().entry(before.intro),
            to.package.view().entry(decl.intro),
        ) else {
            // Both intros came out of `entries_sorted` on these same views a
            // few lines ago, so a miss is not reachable. Skipping rather than
            // panicking keeps one inconsistent entry from costing the whole
            // diff.
            continue;
        };

        let prev_sig = signature::tokens(prev_entry, &from.package);
        let cur_sig = signature::tokens(cur_entry, &to.package);
        let change = timeline::classify(
            &Seen {
                entry: prev_entry,
                sig: prev_sig,
            },
            cur_entry,
            &cur_sig,
        );

        if matches!(change, TimelineChange::Unchanged) {
            unchanged += 1;
            continue;
        }
        rows.push(DiffRow {
            key: StableRef::new(package.clone(), decl.intro),
            path: decl.path.clone(),
            name: decl.name.clone(),
            kind: SharedStr::from(decl.kind),
            verdict: DiffVerdict::Changed { change },
        });
    }

    // ── Pass 2: re-pair the unmatched on (kind, path) ───────────────────────
    //
    // Only *unique* matches count. Two overloads that both churned share a
    // `(kind, path)` and there is no way to say which became which, so they
    // fall through to pass 3 and are reported as unresolved rather than paired
    // arbitrarily — an arbitrary pairing would be a fabricated `Rekeyed` row,
    // which is the same lie as a fabricated `Removed` one wearing a nicer hat.
    let orphaned_from: Vec<&Decl> = from_decls
        .iter()
        .filter(|d| !to_by_intro.contains_key(&d.intro))
        .collect();
    let orphaned_to: Vec<&Decl> = to_decls
        .iter()
        .filter(|d| !from_by_intro.contains_key(&d.intro))
        .collect();

    let from_by_path = unique_by_path(&orphaned_from);
    let to_by_path = unique_by_path(&orphaned_to);

    let mut paired_from: Vec<IntroId> = Vec::new();
    let mut paired_to: Vec<IntroId> = Vec::new();

    for (path_key, before) in &from_by_path {
        let Some(after) = to_by_path.get(path_key) else {
            continue;
        };
        paired_from.push(before.intro);
        paired_to.push(after.intro);
        rows.push(DiffRow {
            key: StableRef::new(package.clone(), after.intro),
            path: after.path.clone(),
            name: after.name.clone(),
            kind: SharedStr::from(after.kind),
            verdict: DiffVerdict::Rekeyed {
                from_key: StableRef::new(package.clone(), before.intro),
                to_key: StableRef::new(package.clone(), after.intro),
                from_tier: before.tier,
                to_tier: after.tier,
            },
        });
    }

    // ── Pass 3: what is left ────────────────────────────────────────────────
    //
    // `still_present_paths` is the second gate on `Removed`, and it was added
    // after running this against real memchr 2.8.0 → 2.8.3. Six declarations
    // came out `Removed` at `(kind, path)`s that also carried an `Added` — the
    // ambiguous groups pass 2 refused to pair, where two or more orphans share
    // one path on each side. Their keys were `Structural`, so the tier gate
    // alone let them through, and the diff said "deleted" about six fields
    // that plainly were not.
    //
    // A same-path declaration on the other side is positive evidence *against*
    // deletion, independent of tier. So a removal candidate whose path is
    // still occupied is `Indeterminate`: we know it is not a deletion and we
    // cannot say which of the same-path arrivals it became.
    let still_present_paths: std::collections::HashSet<PathKey> = orphaned_to
        .iter()
        .filter(|d| !paired_to.contains(&d.intro))
        .map(|d| (d.kind, d.path.clone()))
        .collect();

    for decl in &orphaned_from {
        if paired_from.contains(&decl.intro) {
            continue;
        }
        // The only branch in this module that asserts a deletion. Two gates,
        // and both must hold: the key must be a function of the declaration's
        // own content, *and* nothing of the same kind must still occupy its
        // path. Everything else is honestly unresolved.
        let path_reoccupied = still_present_paths.contains(&(decl.kind, decl.path.clone()));
        let verdict = match (decl.tier.is_content_derived(), path_reoccupied) {
            (Some(true), false) => DiffVerdict::Removed,
            _ => DiffVerdict::Indeterminate { tier: decl.tier },
        };
        rows.push(DiffRow {
            key: StableRef::new(package.clone(), decl.intro),
            path: decl.path.clone(),
            name: decl.name.clone(),
            kind: SharedStr::from(decl.kind),
            verdict,
        });
    }

    for decl in &orphaned_to {
        if paired_to.contains(&decl.intro) {
            continue;
        }
        // No tier gate on the added side: an addition claims only that this
        // key is present now and was not before, which is true whatever tier
        // minted it. The asymmetry is deliberate — a false "added" costs a
        // reader a wasted look, a false "removed" costs them a migration.
        rows.push(DiffRow {
            key: StableRef::new(package.clone(), decl.intro),
            path: decl.path.clone(),
            name: decl.name.clone(),
            kind: SharedStr::from(decl.kind),
            verdict: DiffVerdict::Added,
        });
    }

    // Deterministic row order. `entries_sorted` fixes the walk, but pass 2 and
    // pass 3 append in their own orders, so the whole list is re-sorted on the
    // identity a reader sees. The key is the tiebreak so the order is total
    // even for two declarations sharing a path.
    rows.sort_by(|a, b| {
        (&*a.path, &*a.name, a.key.to_string()).cmp(&(&*b.path, &*b.name, b.key.to_string()))
    });

    PackageDiff {
        package: package.clone(),
        from_version: from.version.clone(),
        to_version: to.version.clone(),
        rows: rows.into(),
        unchanged,
        from_symbol_count: from_decls.len() as u32,
        to_symbol_count: to_decls.len() as u32,
    }
}

/// Resolve every live declaration in `package` to the facts both passes need.
///
/// Walks `entries_sorted` rather than `entries`: this order reaches a user
/// through the diff's row order in every case where two rows tie, and
/// `HashMap` order is not allowed to do that.
fn declarations(package: &PackageView) -> Vec<Decl> {
    package
        .view()
        .entries_sorted()
        .map(|(intro, entry)| Decl {
            intro,
            path: package
                .indexes()
                .path_of(intro)
                .map(|p| SharedStr::from(p.as_ref()))
                // An entry with no precomputed path is one whose parent chain
                // did not resolve. Its own name is still a usable join axis
                // within its kind, and dropping the declaration entirely would
                // silently shrink the diff.
                .unwrap_or_else(|| SharedStr::from(entry.sym().name.as_str())),
            name: SharedStr::from(entry.sym().name.as_str()),
            kind: kind_label(entry),
            tier: tier_label(package, intro),
        })
        .collect()
}

/// Index `decls` by `(kind, path)`, **dropping** any key held by more than one
/// declaration.
///
/// The drop is the point. A `(kind, path)` shared by two orphans carries no
/// information about which one became which, so pairing them would invent a
/// fact. They fall through to pass 3, where the older side is reported
/// `Indeterminate` (its key is by construction one of the churn-prone tiers if
/// it collided at all) and the newer side `Added`.
fn unique_by_path<'a>(decls: &[&'a Decl]) -> HashMap<PathKey, &'a Decl> {
    let mut counts: HashMap<PathKey, usize> = HashMap::new();
    for d in decls {
        *counts.entry((d.kind, d.path.clone())).or_default() += 1;
    }
    decls
        .iter()
        .filter(|d| counts.get(&(d.kind, d.path.clone())) == Some(&1))
        .map(|d| ((d.kind, d.path.clone()), *d))
        .collect()
}

/// The kind label a diff row publishes.
///
/// Written out via the discriminant's `Debug` rather than a hand-rolled table
/// because — unlike the graph's `visibility_name` — this string is not a
/// filter operand any caller matches against; it is display text beside a key
/// the caller actually uses. If that ever stops being true it needs the same
/// treatment `visibility_name` got.
fn kind_label(entry: &nudox_ir::entry::Entry) -> &'static str {
    use nudox_ir::kind::KindDiscriminant as K;
    match entry.kind().discriminant() {
        Some(K::Function) => "Function",
        Some(K::Record) => "Record",
        Some(K::Trait) => "Trait",
        Some(K::Impl) => "Impl",
        Some(K::Enum) => "Enum",
        Some(K::Field) => "Field",
        Some(K::Const) => "Const",
        Some(K::Alias) => "Alias",
        Some(K::Static) => "Static",
        Some(K::Variant) => "Variant",
        Some(K::Module) => "Module",
        Some(K::Reexport) => "Reexport",
        Some(K::Param) => "Param",
        _ => "Reference",
    }
}

/// Project `PackageView::key_tier` onto the wire vocabulary.
fn tier_label(package: &PackageView, intro: IntroId) -> KeyTierLabel {
    use nudox_ir::package::KeyTier;
    match package.key_tier(intro) {
        Some(KeyTier::Structural) => KeyTierLabel::Structural,
        Some(KeyTier::Span) => KeyTierLabel::Span,
        Some(KeyTier::Ordinal) => KeyTierLabel::Ordinal,
        None => KeyTierLabel::Unrecorded,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nudox_ir::{
        apply::PristineIntroTable,
        change::PackageLineageId,
        entry::{Node, Symbol, Visibility},
        kind::Kind,
        kinds::{Function, Module},
        package::{Escalation, SealReport},
        view::IrView,
    };
    use crate::store::package::{PackageView, Provenance};

    use super::*;
    use crate::test_support::{intro, lineage};

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    /// A package whose entries are `(intro, name)` under one root module, with
    /// `forced` naming the intros that were escalated.
    ///
    /// Built through `PackageView::build_sealed` with a hand-made
    /// [`SealReport`] so that the tier gate is exercised for real rather than
    /// stubbed: a fixture built with `build` reports `Unrecorded` for
    /// everything and every disappearance would be `Indeterminate`, which
    /// would make the `Removed` assertions below unreachable.
    fn package(
        lid: &PackageLineageId,
        members: &[(u8, &str)],
        forced: &[(u8, Escalation)],
    ) -> Arc<PackageView> {
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(200),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        for (n, name) in members {
            table.insert_live(
                intro(*n),
                nudox_ir::entry::Entry::new(
                    sym(name),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    Kind::Function(Function::builder().build()),
                ),
                Some(intro(200)),
            );
        }
        let report = SealReport {
            forced_keys: forced.iter().map(|(n, e)| (intro(*n), *e)).collect(),
            ..SealReport::default()
        };
        Arc::new(PackageView::build_sealed(
            IrView::with_package(lid.clone(), table),
            Provenance::TrustedLocal,
            &report,
        ))
    }

    fn slice(version: &str, package: Arc<PackageView>, is_current: bool) -> VersionSlice {
        VersionSlice {
            version: SharedStr::from(version),
            package,
            is_current,
        }
    }

    fn verdict_for<'a>(diff: &'a PackageDiff, name: &str) -> Option<&'a DiffVerdict> {
        diff.rows
            .iter()
            .find(|r| &*r.name == name)
            .map(|r| &r.verdict)
    }

    /// A declaration whose key was content-derived and is gone really is gone.
    ///
    /// This is the only verdict that asserts a deletion, so it must still be
    /// reachable — a diff that answered `Indeterminate` to everything would
    /// satisfy the honesty requirement and be useless.
    #[test]
    fn a_content_derived_key_that_disappears_is_reported_as_removed() {
        let lid = lineage("memchr");
        let before = package(&lid, &[(1, "old_fn"), (2, "kept")], &[]);
        let after = package(&lid, &[(2, "kept")], &[]);

        let diff = build(
            &lid,
            &slice("2.8.0", before, false),
            &slice("2.8.3", after, true),
        );

        assert_eq!(
            verdict_for(&diff, "old_fn"),
            Some(&DiffVerdict::Removed),
            "a Structural key that vanished is evidence about the declaration. \
             rows: {:?}",
            diff.rows
        );
        assert_eq!(diff.indeterminate(), 0);
    }

    /// The defect the whole module exists for: a declaration that kept its
    /// `(kind, path)` and changed only its `IntroId` must be **one** row.
    #[test]
    fn a_churned_key_is_one_rekeyed_row_not_a_removal_and_an_addition() {
        let lid = lineage("log");
        // `same_fn` is intro 1 before and intro 3 after — identical name and
        // path, different key, and the older key was Span-tier.
        let before = package(&lid, &[(1, "same_fn")], &[(1, Escalation::Span)]);
        let after = package(&lid, &[(3, "same_fn")], &[(3, Escalation::Span)]);

        let diff = build(
            &lid,
            &slice("0.4.17", before, false),
            &slice("0.4.33", after, true),
        );

        let rows: Vec<_> = diff.rows.iter().filter(|r| &*r.name == "same_fn").collect();
        assert_eq!(
            rows.len(),
            1,
            "one declaration must produce one row; two rows is the \
             removed-and-added lie. rows: {:?}",
            diff.rows
        );
        match &rows[0].verdict {
            DiffVerdict::Rekeyed {
                from_key,
                to_key,
                from_tier,
                to_tier,
            } => {
                assert_eq!(from_key.intro, intro(1));
                assert_eq!(to_key.intro, intro(3));
                assert_eq!(*from_tier, KeyTierLabel::Span);
                assert_eq!(*to_tier, KeyTierLabel::Span);
            }
            other => panic!("expected Rekeyed, got {other:?}"),
        }
        assert_eq!(
            diff.count(|v| matches!(v, DiffVerdict::Removed)),
            0,
            "nothing was removed"
        );
        assert_eq!(
            diff.count(|v| matches!(v, DiffVerdict::Added)),
            0,
            "nothing was added"
        );
    }

    /// A fragile key that disappears with no unique re-pairing must **not** be
    /// called removed.
    #[test]
    fn a_fragile_key_that_cannot_be_repaired_is_indeterminate_not_removed() {
        let lid = lineage("lodash");
        let before = package(
            &lid,
            &[(1, "gone_fn"), (2, "kept")],
            &[(1, Escalation::Ordinal)],
        );
        let after = package(&lid, &[(2, "kept")], &[]);

        let diff = build(
            &lid,
            &slice("4.17.20", before, false),
            &slice("4.17.21", after, true),
        );

        assert_eq!(
            verdict_for(&diff, "gone_fn"),
            Some(&DiffVerdict::Indeterminate {
                tier: KeyTierLabel::Ordinal
            }),
            "an Ordinal key's disappearance is equally consistent with the \
             producer having reordered its output. rows: {:?}",
            diff.rows
        );
        assert_eq!(
            diff.count(|v| matches!(v, DiffVerdict::Removed)),
            0,
            "claiming this was deleted is the failure mode"
        );
        assert_eq!(diff.indeterminate(), 1);
    }

    /// Two orphans sharing a `(kind, path)` must not be paired arbitrarily.
    #[test]
    fn an_ambiguous_repairing_is_refused_rather_than_guessed() {
        let lid = lineage("guava");
        // Two same-named overloads on each side, all four keys distinct.
        let before = package(
            &lid,
            &[(1, "overload"), (2, "overload")],
            &[(1, Escalation::Span), (2, Escalation::Span)],
        );
        let after = package(
            &lid,
            &[(3, "overload"), (4, "overload")],
            &[(3, Escalation::Span), (4, Escalation::Span)],
        );

        let diff = build(
            &lid,
            &slice("32.0", before, false),
            &slice("33.0", after, true),
        );

        assert_eq!(
            diff.count(|v| matches!(v, DiffVerdict::Rekeyed { .. })),
            0,
            "there is no evidence about which overload became which; pairing \
             them would fabricate one. rows: {:?}",
            diff.rows
        );
        assert_eq!(diff.indeterminate(), 2, "both old keys are unresolved");
        assert_eq!(diff.count(|v| matches!(v, DiffVerdict::Added)), 2);
    }

    /// A path that is still occupied on the newer side is not a deletion, even
    /// when the vanished key was content-derived.
    ///
    /// # Found by running the real thing
    ///
    /// The tier gate alone is not sufficient, and unit tests did not show it:
    /// against real memchr 2.8.0 → 2.8.3 six `Structural`-keyed tuple fields
    /// came out `Removed` at paths that simultaneously carried an `Added`,
    /// because they belonged to ambiguous same-path groups that pass 2
    /// correctly refused to pair. "Deleted" was plainly the wrong word for a
    /// field whose path is still occupied.
    #[test]
    fn a_structural_key_whose_path_is_still_occupied_is_not_a_deletion() {
        let lid = lineage("memchr");
        // Two same-path declarations on each side, all four Structural. Pass 2
        // refuses to pair them (ambiguous), so pass 3 decides — and must not
        // decide "deleted".
        let before = package(&lid, &[(1, "0"), (2, "0")], &[]);
        let after = package(&lid, &[(3, "0"), (4, "0")], &[]);

        let diff = build(
            &lid,
            &slice("2.8.0", before, false),
            &slice("2.8.3", after, true),
        );

        assert_eq!(
            diff.count(|v| matches!(v, DiffVerdict::Removed)),
            0,
            "a path still occupied by a same-kind declaration is evidence \
             against deletion, whatever tier minted the old key. rows: {:?}",
            diff.rows
        );
        assert_eq!(
            diff.indeterminate(),
            2,
            "both vanished keys are unresolved, not deleted"
        );
        assert_eq!(diff.count(|v| matches!(v, DiffVerdict::Added)), 2);
    }

    /// A package with no seal report cannot claim any deletion.
    #[test]
    fn a_view_without_a_seal_report_reports_every_disappearance_as_unrecorded() {
        let lid = lineage("fixture");
        let mut table = PristineIntroTable::new();
        table.insert_live(
            intro(200),
            nudox_ir::entry::Entry::new(
                sym("root"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Module(Module),
            ),
            None,
        );
        table.insert_live(
            intro(1),
            nudox_ir::entry::Entry::new(
                sym("gone"),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Function(Function::builder().build()),
            ),
            Some(intro(200)),
        );
        let before = Arc::new(PackageView::build(
            IrView::with_package(lid.clone(), table),
            Provenance::TrustedLocal,
        ));
        let after = package(&lid, &[], &[]);

        let diff = build(&lid, &slice("1", before, false), &slice("2", after, true));

        assert_eq!(
            verdict_for(&diff, "gone"),
            Some(&DiffVerdict::Indeterminate {
                tier: KeyTierLabel::Unrecorded
            }),
            "with no seal report there is no basis for asserting a deletion. \
             rows: {:?}",
            diff.rows
        );
    }

    /// An unchanged declaration is a count, not a row — and the count is real.
    #[test]
    fn unchanged_declarations_are_counted_and_not_listed() {
        let lid = lineage("memchr");
        let before = package(&lid, &[(1, "a"), (2, "b")], &[]);
        let after = package(&lid, &[(1, "a"), (2, "b")], &[]);

        let diff = build(&lid, &slice("1", before, false), &slice("2", after, true));

        assert!(
            diff.rows.is_empty(),
            "identical generations produce no rows; got {:?}",
            diff.rows
        );
        // Three: the root module plus the two functions.
        assert_eq!(diff.unchanged, 3);
        assert_eq!(diff.from_symbol_count, 3);
        assert_eq!(diff.to_symbol_count, 3);
    }

    /// A rename is one declaration under one key, and the diff must say so
    /// with the *same* vocabulary the symbol page uses.
    #[test]
    fn a_rename_under_a_stable_key_is_a_changed_row_not_a_pair() {
        let lid = lineage("memchr");
        let before = package(&lid, &[(1, "Router")], &[]);
        let after = package(&lid, &[(1, "App")], &[]);

        let diff = build(&lid, &slice("1", before, false), &slice("2", after, true));

        assert_eq!(diff.rows.len(), 1, "rows: {:?}", diff.rows);
        assert_eq!(
            diff.rows[0].verdict,
            DiffVerdict::Changed {
                change: TimelineChange::Renamed {
                    from: SharedStr::from("Router")
                }
            },
            "the classifier is `timeline::classify`, so this is the same \
             `Renamed` the symbol page shows"
        );
        assert_eq!(&*diff.rows[0].name, "App", "the row names the new name");
    }
}
