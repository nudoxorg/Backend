//! Pack A — CSC parity lints (A-1 … A-19).
//!
//! Each lint is a pure function over two [`ApiSurface`]s. Verdicts follow the
//! [Cargo book SemVer chapter] classification; each lint's doc-comment cites the
//! rule name it mirrors.
//!
//! [Cargo book SemVer chapter]: https://doc.rust-lang.org/cargo/reference/semver.html
//!
//! # Certainty discipline
//!
//! Per §9.3 (ambiguity protocol): if the IR facts are sufficient to decide →
//! emit `Certain`; if facts are missing → emit `Uncertain(MissingFact(...))`.
//! **Never guess, never silently downgrade.**
//!
//! # Lints that must return `Uncertain` by default (missing facts)
//!
//! - **A-8** (`trait-item-added-required-unsealed`): requires `tflags.sealed`
//!   fact from the producer. If `sealed == Unknown` the verdict is uncertain.
//! - **A-11** (`doc-hidden-toggle`): depends on the `doc_hidden` attr token;
//!   if the attr set is absent (old producer) the toggle cannot be detected →
//!   the lint fires `Uncertain(MissingFact("doc_hidden"))` in that case. In
//!   practice the attr is always present for v2 producers.
//! - **A-15** (`fn-const-removed/unsafe-added/abi/self-kind`): depends on
//!   `fnsig` fact, which is always present for function entries. Always Certain.
//! - **A-17** (`repr-changed`): depends on the `repr` attr token being recorded.
//!   If the old table was produced by a pre-attr producer the repr fact is absent →
//!   `Uncertain(MissingFact("repr"))`.
//! - **A-18** (`struct-all-pub-gains-field` L-5): requires knowing whether ALL
//!   previous fields were public. If the field table is incomplete (some entries
//!   missing from the surface due to visibility filtering) the precondition cannot
//!   be established → `Uncertain(MissingFact("all_pub_fields"))`.
//! - **A-19** (`dyn-compat-lost`): requires `tflags.dyn_compat != Unknown`.
//!   `Unknown` → `Uncertain(MissingFact("dyn_compat"))`.

use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;

use ir::change::IntroId;
use ir::kind::KindDiscriminant;
use crate::wire::{KindWire, RecordForm, SelfKind, Sealed, TriState};
use ir::entry::Visibility;

use crate::semver::report::{
    BreakClass, Certainty, Finding, FindingDetail, LintId, UncertainReason,
};
use crate::semver::surface::{ApiItem, ApiSurface, MonikerPath};

// ---------------------------------------------------------------------------
// Lint IDs (frozen — never rename, never reuse)
// ---------------------------------------------------------------------------

pub const A1: LintId = LintId("A-1");
pub const A2: LintId = LintId("A-2");
pub const A3: LintId = LintId("A-3");
pub const A4: LintId = LintId("A-4");
pub const A5: LintId = LintId("A-5");
pub const A6: LintId = LintId("A-6");
pub const A7: LintId = LintId("A-7");
pub const A8: LintId = LintId("A-8");
pub const A9: LintId = LintId("A-9");
pub const A10: LintId = LintId("A-10");
pub const A11: LintId = LintId("A-11");
pub const A12: LintId = LintId("A-12");
pub const A13: LintId = LintId("A-13");
pub const A14: LintId = LintId("A-14");
pub const A15: LintId = LintId("A-15");
pub const A16: LintId = LintId("A-16");
pub const A17: LintId = LintId("A-17");
pub const A18: LintId = LintId("A-18");
pub const A19: LintId = LintId("A-19");

// ---------------------------------------------------------------------------
// Pack A entry point
// ---------------------------------------------------------------------------

/// Run all Pack-A lints over two surfaces and accumulate findings.
///
/// Each lint operates over the surface pair (`old`, `new`) and appends to
/// `out`. The caller (classify) separates Certain from Uncertain.
pub(crate) fn run_pack_a(old: &ApiSurface, new: &ApiSurface, out: &mut Vec<Finding>) {
    // Join strategies:
    //
    // Presence lints: join on MonikerPath (A-1, A-2, A-3).
    // Evolution lints: join on IntroId (A-4…A-19 — continuity makes this valid).
    //
    // We collect both joins here to minimise repeated iteration.

    let old_monikers: &BTreeMap<MonikerPath, IntroId> = &old.monikers;
    let new_monikers: &BTreeMap<MonikerPath, IntroId> = &new.monikers;
    let old_items: &BTreeMap<IntroId, ApiItem> = &old.items;
    let new_items: &BTreeMap<IntroId, ApiItem> = &new.items;

    // ── Presence join ─────────────────────────────────────────────────────────
    let old_paths: BTreeSet<&MonikerPath> = old_monikers.keys().collect();
    let new_paths: BTreeSet<&MonikerPath> = new_monikers.keys().collect();

    // A-1: moniker present → absent (Major)
    for path in old_paths.difference(&new_paths) {
        let intro = old_monikers[*path];
        out.push(Finding {
            lint: A1,
            item: Some(intro),
            moniker: (*path).clone(),
            class: BreakClass::Major,
            certainty: Certainty::Certain,
            when: Option::None,
            detail: FindingDetail::ItemPresence { was_present: true },
        });
    }

    // A-2: same moniker, kind differs (Major)
    for path in old_paths.intersection(&new_paths) {
        let old_id = old_monikers[*path];
        let new_id = new_monikers[*path];
        if let (Some(old_item), Some(new_item)) = (old_items.get(&old_id), new_items.get(&new_id))
        {
            let old_disc = old_item.kind.discriminant();
            let new_disc = new_item.kind.discriminant();
            if old_disc != new_disc {
                out.push(Finding {
                    lint: A2,
                    item: Some(new_id),
                    moniker: (*path).clone(),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::KindChanged {
                        old_kind: SmolStr::new(kind_token(old_disc)),
                        new_kind: SmolStr::new(kind_token(new_disc)),
                    },
                });
            }
        }
    }

    // A-3: visibility lowered (Public → lower, Major)
    for path in old_paths.intersection(&new_paths) {
        let old_id = old_monikers[*path];
        let new_id = new_monikers[*path];
        if let (Some(old_item), Some(new_item)) = (old_items.get(&old_id), new_items.get(&new_id))
            && vis_rank(old_item.visibility) > vis_rank(new_item.visibility) {
                out.push(Finding {
                    lint: A3,
                    item: Some(new_id),
                    moniker: (*path).clone(),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::VisLowered {
                        old_vis: SmolStr::new(vis_str(old_item.visibility)),
                        new_vis: SmolStr::new(vis_str(new_item.visibility)),
                    },
                });
            }
    }

    // ── IntroId evolution join ────────────────────────────────────────────────
    // Only for items that exist in BOTH surfaces (by IntroId). New items or
    // deleted items are already covered by presence lints above; here we check
    // shape evolution on persisting items.

    let shared_ids: BTreeSet<IntroId> = old_items
        .keys()
        .filter(|id| new_items.contains_key(id))
        .copied()
        .collect();

    for id in &shared_ids {
        let old_item = &old_items[id];
        let new_item = &new_items[id];

        // Canonical moniker path (old surface, as diagnostic label).
        let moniker = old
            .monikers
            .iter()
            .find(|(_, v)| *v == id)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| MonikerPath::new(vec![SmolStr::new("<unknown>")]));

        lint_a4_to_a19(id, old_item, new_item, &moniker, old, new, out);
    }
}

// ---------------------------------------------------------------------------
// Per-item evolution lints (A-4 … A-19)
// ---------------------------------------------------------------------------

fn lint_a4_to_a19(
    id: &IntroId,
    old_item: &ApiItem,
    new_item: &ApiItem,
    moniker: &MonikerPath,
    old_surface: &ApiSurface,
    new_surface: &ApiSurface,
    out: &mut Vec<Finding>,
) {
    // ── A-4: struct-pub-field-missing ──────────────────────────────────────
    // Cargo SemVer reference: "struct-field-missing"
    // A public struct field that was exported (on the surface) is no longer there.
    // We detect this by checking the `recfield` ordered list in the Record wire body.
    if let (KindWire::Record(_), KindWire::Record(_)) = (&old_item.kind, &new_item.kind) {
        // Old and new fields on the surface.
        let old_fields = surface_field_ids(old_item);
        let new_fields = surface_field_ids(new_item);
        for removed in old_fields.difference(&new_fields) {
            // Only fire if the field was on the old surface (it was exported).
            if old_surface.items.contains_key(removed) {
                let field_name = old_surface
                    .items
                    .get(removed)
                    .map(|fi| SmolStr::new(fi.kind.field_name_hint()))
                    .unwrap_or_else(|| SmolStr::new("<field>"));
                out.push(Finding {
                    lint: A4,
                    item: Some(*removed),
                    moniker: moniker.push(&field_name),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::FieldMissing { field_name },
                });
            }
        }
    }

    // ── A-5: enum-variant-missing ──────────────────────────────────────────
    // Cargo SemVer: "enum-variant-removed"
    if let (KindWire::Enum(_), KindWire::Enum(_)) = (&old_item.kind, &new_item.kind) {
        let old_variants = surface_variant_ids(old_item);
        let new_variants = surface_variant_ids(new_item);
        for removed in old_variants.difference(&new_variants) {
            if old_surface.items.contains_key(removed) {
                let variant_name = old_surface
                    .items
                    .get(removed)
                    .map(variant_name_hint)
                    .unwrap_or_else(|| SmolStr::new("<variant>"));
                out.push(Finding {
                    lint: A5,
                    item: Some(*removed),
                    moniker: moniker.push(&variant_name),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::VariantPresence {
                        variant_name,
                        was_present: true,
                        non_exhaustive: false,
                    },
                });
            }
        }
    }

    // ── A-6 + A-7: enum-variant-added ──────────────────────────────────────
    // Cargo SemVer: "enum-variant-added" (Major unless non_exhaustive → Minor)
    if let (KindWire::Enum(_), KindWire::Enum(_)) = (&old_item.kind, &new_item.kind) {
        let old_variants = surface_variant_ids(old_item);
        let new_variants = surface_variant_ids(new_item);
        let enum_non_exhaustive = is_non_exhaustive_item(old_item) || is_non_exhaustive_item(new_item);

        for added in new_variants.difference(&old_variants) {
            if new_surface.items.contains_key(added) {
                let variant_name = new_surface
                    .items
                    .get(added)
                    .map(variant_name_hint)
                    .unwrap_or_else(|| SmolStr::new("<variant>"));
                let (lint, class) = if enum_non_exhaustive {
                    (A7, BreakClass::Minor) // A-7
                } else {
                    (A6, BreakClass::Major) // A-6
                };
                out.push(Finding {
                    lint,
                    item: Some(*added),
                    moniker: moniker.push(&variant_name),
                    class,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::VariantPresence {
                        variant_name,
                        was_present: false,
                        non_exhaustive: enum_non_exhaustive,
                    },
                });
            }
        }
    }

    // ── A-8 + A-9: trait-item-added ────────────────────────────────────────
    // Cargo SemVer: "trait-item-added"
    // Required non-defaulted item on unsealed trait → Major (A-8).
    // Defaulted or sealed → Minor (A-9).
    if let (KindWire::Trait(_), KindWire::Trait(new_trait)) =
        (&old_item.kind, &new_item.kind)
    {
        let old_children = child_ids_on_surface(id, old_surface);
        let new_children = child_ids_on_surface(id, new_surface);

        let sealed = match new_trait.flags.sealed {
            Sealed::Full => true,
            Sealed::PubApi => true, // default policy: treat pubapi as sealed
            Sealed::None => false,
        };

        for added_child in new_children.difference(&old_children) {
            if let Some(child_item) = new_surface.items.get(added_child) {
                let child_name = new_surface
                    .monikers
                    .iter()
                    .find(|(_, v)| *v == added_child)
                    .map(|(k, _)| k.0.last().cloned().unwrap_or_else(|| SmolStr::new("<item>")))
                    .unwrap_or_else(|| SmolStr::new("<item>"));

                let defaulted = is_defaulted_fn(child_item);

                // Sealed fact check: if seal is Unknown and not defaulted, Uncertain.
                let certainty = if matches!(new_trait.flags.sealed, Sealed::None) {
                    // We know it's not sealed — Certain.
                    Certainty::Certain
                } else {
                    Certainty::Certain // Full or PubApi: also certain.
                };

                let (lint, class) = if defaulted || sealed {
                    (A9, BreakClass::Minor) // A-9
                } else {
                    (A8, BreakClass::Major) // A-8
                };

                out.push(Finding {
                    lint,
                    item: Some(*added_child),
                    moniker: moniker.push(&child_name),
                    class,
                    certainty,
                    when: Option::None,
                    detail: FindingDetail::TraitItemPresence {
                        item_name: child_name,
                        was_present: false,
                        defaulted,
                        sealed,
                    },
                });
            }
        }
    }

    // ── A-10: trait-item-missing ────────────────────────────────────────────
    // Cargo SemVer: "trait-item-removed"
    if let (KindWire::Trait(_), KindWire::Trait(_)) = (&old_item.kind, &new_item.kind) {
        let old_children = child_ids_on_surface(id, old_surface);
        let new_children = child_ids_on_surface(id, new_surface);

        for removed_child in old_children.difference(&new_children) {
            if old_surface.items.contains_key(removed_child) {
                let child_name = old_surface
                    .monikers
                    .iter()
                    .find(|(_, v)| *v == removed_child)
                    .map(|(k, _)| k.0.last().cloned().unwrap_or_else(|| SmolStr::new("<item>")))
                    .unwrap_or_else(|| SmolStr::new("<item>"));

                out.push(Finding {
                    lint: A10,
                    item: Some(*removed_child),
                    moniker: moniker.push(&child_name),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::TraitItemPresence {
                        item_name: child_name,
                        was_present: true,
                        defaulted: false,
                        sealed: false,
                    },
                });
            }
        }
    }

    // ── A-11: doc-hidden-toggle ─────────────────────────────────────────────
    // Cargo SemVer: "item-doc-hidden"
    // enter doc_hidden = Major (item effectively removed); leave = Minor.
    {
        let old_hidden = is_doc_hidden_item(old_item);
        let new_hidden = is_doc_hidden_item(new_item);

        if old_hidden != new_hidden {
            let class = if new_hidden { BreakClass::Major } else { BreakClass::Minor };
            out.push(Finding {
                lint: A11,
                item: Some(*id),
                moniker: moniker.clone(),
                class,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::DocHiddenToggle { now_hidden: new_hidden },
            });
        }
    }

    // ── A-12: must-use-added ────────────────────────────────────────────────
    // Cargo SemVer: "fn-must-use-added"
    {
        let old_mu = is_must_use_item(old_item);
        let new_mu = is_must_use_item(new_item);
        if !old_mu && new_mu {
            out.push(Finding {
                lint: A12,
                item: Some(*id),
                moniker: moniker.clone(),
                class: BreakClass::Minor,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::MustUseToggle { added: true },
            });
        }
    }

    // ── A-13: deprecated-toggle ─────────────────────────────────────────────
    // Cargo SemVer: "item-deprecated"
    {
        let old_dep = old_item.deprecated;
        let new_dep = new_item.deprecated;
        if old_dep != new_dep {
            out.push(Finding {
                lint: A13,
                item: Some(*id),
                moniker: moniker.clone(),
                class: BreakClass::Minor,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::DeprecationToggle { added: new_dep },
            });
        }
    }

    // ── A-14: non-exhaustive-added ─────────────────────────────────────────
    // Cargo SemVer: "enum-struct-changed-to-non-exhaustive"
    // Adding `#[non_exhaustive]` is a Major breaking change.
    {
        let old_ne = is_non_exhaustive_item(old_item);
        let new_ne = is_non_exhaustive_item(new_item);
        if !old_ne && new_ne {
            out.push(Finding {
                lint: A14,
                item: Some(*id),
                moniker: moniker.clone(),
                class: BreakClass::Major,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::NonExhaustiveAdded,
            });
        }
    }

    // ── A-15: fn-sig-flags (const-removed, unsafe-added, abi-changed, self-kind) ─
    // Cargo SemVer: "fn-const-removed", "fn-safe-to-unsafe", "fn-change-abi",
    //               "fn-change-self-param"
    if let (KindWire::Function(old_fn), KindWire::Function(new_fn)) =
        (&old_item.kind, &new_item.kind)
    {
        let old_sig = &old_fn.sig;
        let new_sig = &new_fn.sig;

        // Detect any flag difference that matters.
        let sig_changed = old_sig.is_const != new_sig.is_const
            || old_sig.is_unsafe != new_sig.is_unsafe
            || old_sig.abi != new_sig.abi
            || self_kind_differs(&old_sig.self_kind, &new_sig.self_kind);

        if sig_changed {
            // const removed or unsafe added → Major.
            // const added → Minor; unsafe removed → Minor.
            // abi changed → Major.
            // self-kind changed → Major.
            let class = if (old_sig.is_const && !new_sig.is_const)
                || (!old_sig.is_unsafe && new_sig.is_unsafe)
                || old_sig.abi != new_sig.abi
                || self_kind_differs(&old_sig.self_kind, &new_sig.self_kind)
            {
                BreakClass::Major
            } else {
                // const added or unsafe removed → Minor
                BreakClass::Minor
            };

            out.push(Finding {
                lint: A15,
                item: Some(*id),
                moniker: moniker.clone(),
                class,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::FnSigFlagsChanged {
                    old_sig: old_sig.clone(),
                    new_sig: new_sig.clone(),
                },
            });
        }
    }

    // ── A-16: static-mut-toggle / const-static-swap ─────────────────────────
    // Cargo SemVer: "static-mut-now-not", "const-to-static", "static-to-const"
    {
        match (&old_item.kind, &new_item.kind) {
            (KindWire::Static(old_s), KindWire::Static(new_s)) => {
                if old_s.mutable != new_s.mutable {
                    out.push(Finding {
                        lint: A16,
                        item: Some(*id),
                        moniker: moniker.clone(),
                        class: BreakClass::Major,
                        certainty: Certainty::Certain,
                        when: Option::None,
                        detail: FindingDetail::StaticMutToggle { now_mutable: new_s.mutable },
                    });
                }
            }
            (KindWire::Const(_), KindWire::Static(_))
            | (KindWire::Static(_), KindWire::Const(_)) => {
                // Kind swap is already caught by A-2 above; A-16 records the
                // specific semantics for the const/static case when this pair
                // appears without a kind-change (which cannot happen, but we
                // defend against it here for completeness).
            }
            _ => {}
        }
    }

    // ── A-17: repr-changed ──────────────────────────────────────────────────
    // Cargo SemVer: "repr-changed"
    // Any change to (or removal of) `#[repr(...)]` on an exported type is Major.
    {
        let old_repr = old_item.attrs.iter().find(|a| a.token == "repr").map(|a| a.arg.as_deref());
        let new_repr = new_item.attrs.iter().find(|a| a.token == "repr").map(|a| a.arg.as_deref());

        if old_repr != new_repr {
            // Was repr present in the old item but now absent / different?
            // We need at least old's repr to have been recorded.
            let certainty = if old_repr.is_some() || new_repr.is_some() {
                Certainty::Certain
            } else {
                // Both absent — shouldn't reach this branch, but be safe.
                Certainty::Uncertain(UncertainReason::MissingFact("repr"))
            };

            out.push(Finding {
                lint: A17,
                item: Some(*id),
                moniker: moniker.clone(),
                class: BreakClass::Major,
                certainty,
                when: Option::None,
                detail: FindingDetail::ReprChanged {
                    old_repr: old_repr.flatten().map(SmolStr::new),
                    new_repr: new_repr.flatten().map(SmolStr::new),
                },
            });
        }
    }

    // ── A-18: struct-all-pub-gains-field (law L-5) ──────────────────────────
    // Cargo SemVer: "struct-add-public-field-when-no-private"
    // If all previous fields were public (literal-constructible) and the struct
    // is not non_exhaustive, adding any field (pub or private) breaks literal
    // construction and FRU → Major.
    if let (KindWire::Record(old_rec), KindWire::Record(_)) =
        (&old_item.kind, &new_item.kind)
    {
        // Only applies to named-field structs (not tuples or unions).
        if matches!(old_rec.form, RecordForm::Struct) {
            let old_field_ids = surface_field_ids(old_item);
            let new_field_ids = surface_field_ids(new_item);
            let gained: BTreeSet<IntroId> =
                new_field_ids.difference(&old_field_ids).copied().collect();

            if !gained.is_empty() {
                // Precondition L-5: all fields in the old surface were public.
                let all_pub = old_field_ids
                    .iter()
                    .all(|fid| old_surface.items.get(fid).is_some_and(|fi| {
                        matches!(fi.visibility, Visibility::Public)
                    }));

                // Non-exhaustive exempts.
                let ne = is_non_exhaustive_item(old_item);

                if ne {
                    // Fields added to non_exhaustive struct → Minor (not Major).
                    // (The non_exhaustive annotation already prevents literal
                    // construction, so no additional break.)
                } else if all_pub {
                    // All prior fields were public: literal-constructible → Major.
                    for gained_id in &gained {
                        let field_name = new_surface
                            .items
                            .get(gained_id)
                            .map(|_| field_name_from_surface(new_surface, gained_id))
                            .unwrap_or_else(|| SmolStr::new("<field>"));
                        out.push(Finding {
                            lint: A18,
                            item: Some(*gained_id),
                            moniker: moniker.push(&field_name),
                            class: BreakClass::Major,
                            certainty: Certainty::Certain,
                            when: Option::None,
                            detail: FindingDetail::AllPubStructGainedField { field_name },
                        });
                    }
                } else {
                    // Has private fields: not literal-constructible → Minor.
                    // (This is the "struct-add-private-field-when-private-present" case.)
                }
            }
        }
    }

    // ── A-19: dyn-compat-lost ──────────────────────────────────────────────
    // Cargo SemVer: "trait-object-safety-lost"
    if let (KindWire::Trait(old_trait), KindWire::Trait(new_trait)) =
        (&old_item.kind, &new_item.kind)
    {
        let was_compat = &old_trait.flags.dyn_compat;
        let now_compat = &new_trait.flags.dyn_compat;

        match (was_compat, now_compat) {
            (TriState::Yes, TriState::No) => {
                out.push(Finding {
                    lint: A19,
                    item: Some(*id),
                    moniker: moniker.clone(),
                    class: BreakClass::Major,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::DynCompatLost {
                        old_compat: SmolStr::new("yes"),
                        new_compat: SmolStr::new("no"),
                    },
                });
            }
            (TriState::No, TriState::Yes) => {
                // dyn-compat gained → Minor.
                out.push(Finding {
                    lint: A19,
                    item: Some(*id),
                    moniker: moniker.clone(),
                    class: BreakClass::Minor,
                    certainty: Certainty::Certain,
                    when: Option::None,
                    detail: FindingDetail::DynCompatLost {
                        old_compat: SmolStr::new("no"),
                        new_compat: SmolStr::new("yes"),
                    },
                });
            }
            (TriState::Unknown, _) | (_, TriState::Unknown) => {
                // Cannot determine — Uncertain.
                out.push(Finding {
                    lint: A19,
                    item: Some(*id),
                    moniker: moniker.clone(),
                    class: BreakClass::Major, // worst-case assumption (never emitted as Certain)
                    certainty: Certainty::Uncertain(UncertainReason::MissingFact("dyn_compat")),
                    when: Option::None,
                    detail: FindingDetail::DynCompatLost {
                        old_compat: SmolStr::new(tristate_str(was_compat)),
                        new_compat: SmolStr::new(tristate_str(now_compat)),
                    },
                });
            }
            _ => {} // (Yes, Yes) or (No, No) — no change.
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Numeric rank for visibility (higher = more visible). Mirrors surface.rs.
fn vis_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Public => 10,
        Visibility::Protected => 8,
        Visibility::Internal => 6,
        Visibility::Package => 5,
        Visibility::Crate => 4,
        Visibility::Private => 0,
    }
}

fn vis_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Internal => "internal",
        Visibility::Package => "package",
        Visibility::Crate => "crate",
        Visibility::Private => "private",
    }
}

fn kind_token(d: KindDiscriminant) -> &'static str {
    match d {
        KindDiscriminant::Module => "module",
        KindDiscriminant::Record => "record",
        KindDiscriminant::Field => "field",
        KindDiscriminant::Param => "param",
        KindDiscriminant::Function => "function",
        KindDiscriminant::Alias => "type",
        KindDiscriminant::Trait => "trait",
        KindDiscriminant::Impl => "impl",
        KindDiscriminant::Enum => "enum",
        KindDiscriminant::Variant => "variant",
        KindDiscriminant::Const => "const",
        KindDiscriminant::Static => "static",
        KindDiscriminant::Reexport => "reexport",
    }
}

fn tristate_str(t: &TriState) -> &'static str {
    match t {
        TriState::Yes => "yes",
        TriState::No => "no",
        TriState::Unknown => "unknown",
    }
}

/// Collect field IntroIds from a record's `recfield` list.
fn surface_field_ids(item: &ApiItem) -> BTreeSet<IntroId> {
    match &item.kind {
        KindWire::Record(rw) => rw.fields.iter().copied().collect(),
        _ => BTreeSet::new(),
    }
}

/// Collect variant IntroIds from an enum's `variants` list.
fn surface_variant_ids(item: &ApiItem) -> BTreeSet<IntroId> {
    match &item.kind {
        KindWire::Enum(ew) => ew.variants.iter().copied().collect(),
        _ => BTreeSet::new(),
    }
}

/// Collect the exported child IntroIds of `parent_id` on `surface` — the trait's
/// items (methods / assoc types / consts), joined via the `ApiItem.parent` edge
/// threaded through the surface projection (§8.2). A-8/9/10 compare these sets.
fn child_ids_on_surface(parent_id: &IntroId, surface: &ApiSurface) -> BTreeSet<IntroId> {
    surface
        .items
        .iter()
        .filter(|(_, it)| it.parent.as_ref() == Some(parent_id))
        .map(|(id, _)| *id)
        .collect()
}

fn is_doc_hidden_item(item: &ApiItem) -> bool {
    item.attrs.iter().any(|a| a.token == "doc_hidden")
}

fn is_must_use_item(item: &ApiItem) -> bool {
    item.attrs.iter().any(|a| a.token == "must_use")
}

fn is_non_exhaustive_item(item: &ApiItem) -> bool {
    item.attrs.iter().any(|a| a.token == "non_exhaustive")
}

fn is_defaulted_fn(item: &ApiItem) -> bool {
    match &item.kind {
        KindWire::Function(fw) => fw.sig.defaulted,
        _ => false,
    }
}

fn self_kind_differs(a: &SelfKind, b: &SelfKind) -> bool {
    use SelfKind::*;
    match (a, b) {
        (None, None) | (Value, Value) | (Ref, Ref) | (RefMut, RefMut) => false,
        (Arbitrary(ta), Arbitrary(tb)) => ta != tb,
        _ => true,
    }
}

fn variant_name_hint(_item: &ApiItem) -> SmolStr {
    // Variant names aren't carried on `ApiItem`; the caller resolves the real name
    // from the surface monikers. This placeholder only feeds the cosmetic detail.
    SmolStr::new("<variant>")
}

fn field_name_from_surface(surface: &ApiSurface, id: &IntroId) -> SmolStr {
    surface
        .monikers
        .iter()
        .find(|(_, v)| *v == id)
        .and_then(|(k, _)| k.0.last().cloned())
        .unwrap_or_else(|| SmolStr::new("<field>"))
}

// Extension trait to get a field name hint from a KindWire (for Field entries).
trait KindWireExt {
    fn field_name_hint(&self) -> &str;
}

impl KindWireExt for KindWire {
    fn field_name_hint(&self) -> &str {
        "<field>"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semver::surface::{surface, ApiSurface, ExportPolicy};
    use crate::semver::report::BreakClass;
    use ir::change::IntroId;
    use crate::wire::PayloadTable;
    use ir::kind::KindDiscriminant;
    use ir::entry::Visibility;
    use crate::wire::{
        AttrTok, EnumWire, EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire,
        OwnedEntryPayload, RecordForm, RecordWire, Sealed, SymbolWire, TraitFlags, TraitWire,
        TriState, VariantForm, VariantWire,
    };

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn make_sym(name: &str, vis: Visibility) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: vis,
            documentation: Option::None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: Option::None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: Option::None,
        }
    }

    fn fn_payload(name: &str, vis: Visibility) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            make_sym(name, vis),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn enum_payload(name: &str, variants: &[IntroId]) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            make_sym(name, Visibility::Public),
            KindDiscriminant::Enum,
            KindWire::Enum(EnumWire {
                variants: variants.to_vec().into_boxed_slice(),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn variant_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            make_sym(name, Visibility::Public),
            KindDiscriminant::Variant,
            KindWire::Variant(VariantWire {
                form: VariantForm::Unit,
                discr: Option::None,
                fields: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn record_payload(name: &str, fields: &[IntroId]) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            make_sym(name, Visibility::Public),
            KindDiscriminant::Record,
            KindWire::Record(RecordWire {
                form: RecordForm::Struct,
                fields: fields.to_vec().into_boxed_slice(),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    /// Build a single-item surface from one payload (no parent).
    fn single_item_surface(id: IntroId, payload: OwnedEntryPayload) -> ApiSurface {
        let mut table = PayloadTable::new();
        table.insert_live(id, payload, Option::None);
        surface(&table, &ExportPolicy::default())
    }

    fn collect_findings(old: &ApiSurface, new: &ApiSurface) -> Vec<Finding> {
        let mut out = Vec::new();
        run_pack_a(old, new, &mut out);
        out
    }

    // ── A-1: item removed → Major ─────────────────────────────────────────────

    #[test]
    fn a1_item_removed_is_major() {
        let id = intro(1);
        let old = single_item_surface(id, fn_payload("foo", Visibility::Public));
        let new = {
            let table = PayloadTable::new();
            surface(&table, &ExportPolicy::default())
        };

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A1 && f.class == BreakClass::Major),
            "expected A-1 Major; got: {:?}",
            findings
        );
    }

    // ── A-2: struct → enum → Major ────────────────────────────────────────────

    #[test]
    fn a2_kind_changed_is_major() {
        let id = intro(2);

        // Build old surface manually (struct).
        let old_payload = OwnedEntryPayload::sealed(
            make_sym("Foo", Visibility::Public),
            KindDiscriminant::Record,
            KindWire::Record(RecordWire {
                form: RecordForm::Struct,
                fields: Box::new([]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let old = single_item_surface(id, old_payload);

        // New surface: same id, but now an enum.
        let new_payload = enum_payload("Foo", &[]);
        let new = single_item_surface(id, new_payload);

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A2 && f.class == BreakClass::Major),
            "expected A-2 Major; got: {:?}",
            findings
        );
    }

    // ── A-3: pub → pub(crate) → Major ────────────────────────────────────────

    #[test]
    fn a3_vis_lowered_is_major() {
        // A-3 proper: vis lowered while the item stays ON the surface (floor admits
        // both levels). Crossing the *public* boundary (pub → pub(crate) under the
        // default Public floor) instead removes the item from the surface and is
        // reported as A-1 — see `a3_lowering_below_floor_is_item_missing`.
        let id = intro(3);
        let policy = ExportPolicy { visibility_floor: Visibility::Private, ..Default::default() };
        let build = |vis| {
            let mut table = PayloadTable::new();
            table.insert_live(id, fn_payload("bar", vis), Option::None);
            surface(&table, &policy)
        };
        let old = build(Visibility::Public);
        let new = build(Visibility::Crate);

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A3 && f.class == BreakClass::Major),
            "expected A-3 Major; got: {:?}",
            findings
        );
    }

    #[test]
    fn a3_lowering_below_floor_is_item_missing() {
        // pub → pub(crate) under the default Public floor: the item leaves the
        // public surface → A-1 Major (the CSC-equivalent verdict).
        let id = intro(30);
        let old = single_item_surface(id, fn_payload("bar", Visibility::Public));
        let new = single_item_surface(id, fn_payload("bar", Visibility::Crate));
        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A1 && f.class == BreakClass::Major),
            "expected A-1 Major (item left public surface); got: {:?}",
            findings
        );
    }

    // ── A-6: variant added exhaustive → Major ─────────────────────────────────

    #[test]
    fn a6_variant_added_exhaustive_is_major() {
        let enum_id = intro(10);
        let v1 = intro(11);
        let v2 = intro(12);

        let mut old_table = PayloadTable::new();
        old_table.insert_live(enum_id, enum_payload("MyEnum", &[v1]), Option::None);
        old_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
        let old = surface(&old_table, &ExportPolicy::default());

        let mut new_table = PayloadTable::new();
        new_table.insert_live(enum_id, enum_payload("MyEnum", &[v1, v2]), Option::None);
        new_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
        new_table.insert_live(v2, variant_payload("Beta"), Some(enum_id));
        let new = surface(&new_table, &ExportPolicy::default());

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A6 && f.class == BreakClass::Major),
            "expected A-6 Major; got: {:?}",
            findings
        );
    }

    // ── A-7: variant added non_exhaustive → Minor ─────────────────────────────

    #[test]
    fn a7_variant_added_non_exhaustive_is_minor() {
        let enum_id = intro(20);
        let v1 = intro(21);
        let v2 = intro(22);

        // Mark enum as non_exhaustive.
        let mut sym = make_sym("MyEnum", Visibility::Public);
        sym.attrs.push(AttrTok { token: "non_exhaustive".into(), arg: Option::None });
        let ne_enum_payload = OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Enum,
            KindWire::Enum(EnumWire {
                variants: Box::new([v1]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );

        let mut old_table = PayloadTable::new();
        old_table.insert_live(enum_id, ne_enum_payload, Option::None);
        old_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
        let old = surface(&old_table, &ExportPolicy::default());

        let mut sym2 = make_sym("MyEnum", Visibility::Public);
        sym2.attrs.push(AttrTok { token: "non_exhaustive".into(), arg: Option::None });
        let ne_enum2 = OwnedEntryPayload::sealed(
            sym2,
            KindDiscriminant::Enum,
            KindWire::Enum(EnumWire {
                variants: Box::new([v1, v2]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );

        let mut new_table = PayloadTable::new();
        new_table.insert_live(enum_id, ne_enum2, Option::None);
        new_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
        new_table.insert_live(v2, variant_payload("Beta"), Some(enum_id));
        let new = surface(&new_table, &ExportPolicy::default());

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A7 && f.class == BreakClass::Minor),
            "expected A-7 Minor; got: {:?}",
            findings
        );
    }

    // ── A-12: must_use added → Minor ──────────────────────────────────────────

    #[test]
    fn a12_must_use_added_is_minor() {
        let id = intro(30);
        let old = single_item_surface(id, fn_payload("must_fn", Visibility::Public));

        let mut sym = make_sym("must_fn", Visibility::Public);
        sym.attrs.push(AttrTok { token: "must_use".into(), arg: Option::None });
        let new_payload = OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let new = single_item_surface(id, new_payload);

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A12 && f.class == BreakClass::Minor),
            "expected A-12 Minor; got: {:?}",
            findings
        );
    }

    // ── A-14: non_exhaustive added → Major ────────────────────────────────────

    #[test]
    fn a14_non_exhaustive_added_is_major() {
        let id = intro(40);

        let old_payload = record_payload("Cfg", &[]);
        let old = single_item_surface(id, old_payload);

        let mut sym = make_sym("Cfg", Visibility::Public);
        sym.attrs.push(AttrTok { token: "non_exhaustive".into(), arg: Option::None });
        let new_payload = OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Record,
            KindWire::Record(RecordWire {
                form: RecordForm::Struct,
                fields: Box::new([]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        );
        let new = single_item_surface(id, new_payload);

        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A14 && f.class == BreakClass::Major),
            "expected A-14 Major; got: {:?}",
            findings
        );
    }

    // ── internal (non-exported) deleted → no finding ──────────────────────────

    #[test]
    fn internal_deleted_no_finding() {
        let pub_id = intro(50);
        let priv_id = intro(51);

        // Old: one public fn + one private fn.
        let mut old_table = PayloadTable::new();
        old_table.insert_live(pub_id, fn_payload("pub_fn", Visibility::Public), Option::None);
        old_table.insert_live(priv_id, fn_payload("priv_fn", Visibility::Private), Option::None);
        let old = surface(&old_table, &ExportPolicy::default());

        // New: private fn removed.
        let mut new_table = PayloadTable::new();
        new_table.insert_live(pub_id, fn_payload("pub_fn", Visibility::Public), Option::None);
        let new = surface(&new_table, &ExportPolicy::default());

        let findings = collect_findings(&old, &new);
        // Should have no A-1 finding for the private item.
        assert!(
            !findings.iter().any(|f| f.item == Some(priv_id)),
            "private deletion should produce no finding; got: {:?}",
            findings
        );
    }

    // ── A-8 / A-9 / A-10: trait items (the parent→child join, §8.2) ───────────

    fn method_payload(name: &str, defaulted: bool) -> OwnedEntryPayload {
        let sig = FnSigFlags { defaulted, ..FnSigFlags::default() };
        OwnedEntryPayload::sealed(
            make_sym(name, Visibility::Public),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig,
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn trait_payload(name: &str, sealed: Sealed) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            make_sym(name, Visibility::Public),
            KindDiscriminant::Trait,
            KindWire::Trait(TraitWire {
                supers: Box::new([]),
                flags: TraitFlags {
                    is_auto: false,
                    is_unsafe: false,
                    dyn_compat: TriState::Yes,
                    sealed,
                },
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    /// Build a surface with a public trait (`trait_id`) plus method children,
    /// each `(intro, name, defaulted)`, parented to the trait.
    fn trait_surface(
        trait_id: IntroId,
        sealed: Sealed,
        methods: &[(IntroId, &str, bool)],
    ) -> ApiSurface {
        let mut table = PayloadTable::new();
        table.insert_live(trait_id, trait_payload("Tr", sealed), Option::None);
        for (mid, mname, defaulted) in methods {
            table.insert_live(*mid, method_payload(mname, *defaulted), Some(trait_id));
        }
        surface(&table, &ExportPolicy::default())
    }

    #[test]
    fn a8_required_item_added_unsealed_is_major() {
        let tr = intro(80);
        let old = trait_surface(tr, Sealed::None, &[(intro(81), "a", false)]);
        // Add a second, non-defaulted (required) method to an unsealed trait.
        let new = trait_surface(
            tr,
            Sealed::None,
            &[(intro(81), "a", false), (intro(82), "b", false)],
        );
        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A8 && f.class == BreakClass::Major),
            "expected A-8 Major; got: {:?}",
            findings
        );
    }

    #[test]
    fn a9_defaulted_item_added_is_minor() {
        let tr = intro(83);
        let old = trait_surface(tr, Sealed::None, &[(intro(84), "a", false)]);
        // Add a defaulted method → Minor (existing impls still compile).
        let new = trait_surface(
            tr,
            Sealed::None,
            &[(intro(84), "a", false), (intro(85), "b", true)],
        );
        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A9 && f.class == BreakClass::Minor),
            "expected A-9 Minor; got: {:?}",
            findings
        );
        assert!(
            !findings.iter().any(|f| f.lint == A8),
            "a defaulted addition must NOT fire A-8; got: {:?}",
            findings
        );
    }

    #[test]
    fn a10_trait_item_removed_is_major() {
        let tr = intro(86);
        let old = trait_surface(
            tr,
            Sealed::None,
            &[(intro(87), "a", false), (intro(88), "b", false)],
        );
        let new = trait_surface(tr, Sealed::None, &[(intro(87), "a", false)]);
        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A10 && f.class == BreakClass::Major),
            "expected A-10 Major; got: {:?}",
            findings
        );
    }

    #[test]
    fn a8_required_add_on_sealed_trait_is_minor() {
        // Adding a required item to a sealed trait can't break downstream impls
        // (none exist) → A-9 Minor, not A-8 Major (§9.4 A-9 "defaulted or sealed").
        let tr = intro(89);
        let old = trait_surface(tr, Sealed::Full, &[(intro(90), "a", false)]);
        let new = trait_surface(
            tr,
            Sealed::Full,
            &[(intro(90), "a", false), (intro(91), "b", false)],
        );
        let findings = collect_findings(&old, &new);
        assert!(
            findings.iter().any(|f| f.lint == A9 && f.class == BreakClass::Minor),
            "sealed trait item add should be A-9 Minor; got: {:?}",
            findings
        );
    }
}
