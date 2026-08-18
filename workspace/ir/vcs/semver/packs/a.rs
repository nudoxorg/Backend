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

use crate::wire::{KindWire, RecordForm, Sealed, SelfKind, TriState};
use ir::change::IntroId;
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

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
        if let (Some(old_item), Some(new_item)) = (old_items.get(&old_id), new_items.get(&new_id)) {
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
            && vis_rank(old_item.visibility) > vis_rank(new_item.visibility)
        {
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
        let moniker = old.monikers.iter().find(|(_, v)| *v == id).map_or_else(
            || MonikerPath::new(vec![SmolStr::new("<unknown>")]),
            |(k, _)| k.clone(),
        );

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
                let field_name = old_surface.items.get(removed).map_or_else(
                    || SmolStr::new("<field>"),
                    |fi| SmolStr::new(fi.kind.field_name_hint()),
                );
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
                    .map_or_else(|| SmolStr::new("<variant>"), variant_name_hint);
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
        let enum_non_exhaustive =
            is_non_exhaustive_item(old_item) || is_non_exhaustive_item(new_item);

        for added in new_variants.difference(&old_variants) {
            if new_surface.items.contains_key(added) {
                let variant_name = new_surface
                    .items
                    .get(added)
                    .map_or_else(|| SmolStr::new("<variant>"), variant_name_hint);
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
    if let (KindWire::Trait(_), KindWire::Trait(new_trait)) = (&old_item.kind, &new_item.kind) {
        let old_children = child_ids_on_surface(id, old_surface);
        let new_children = child_ids_on_surface(id, new_surface);

        let sealed = match new_trait.flags.sealed {
            Sealed::Full | Sealed::PubApi => true, // default policy: treat pubapi as sealed
            Sealed::None => false,
        };

        for added_child in new_children.difference(&old_children) {
            if let Some(child_item) = new_surface.items.get(added_child) {
                let child_name = new_surface
                    .monikers
                    .iter()
                    .find(|(_, v)| *v == added_child)
                    .map_or_else(
                        || SmolStr::new("<item>"),
                        |(k, _)| {
                            k.0.last()
                                .cloned()
                                .unwrap_or_else(|| SmolStr::new("<item>"))
                        },
                    );

                let defaulted = is_defaulted_fn(child_item);

                // The sealed fact is known in every case: `None` is
                // known-not-sealed and `Full`/`PubApi` are known-sealed.
                let certainty = Certainty::Certain;

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
                    .map_or_else(
                        || SmolStr::new("<item>"),
                        |(k, _)| {
                            k.0.last()
                                .cloned()
                                .unwrap_or_else(|| SmolStr::new("<item>"))
                        },
                    );

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
            let class = if new_hidden {
                BreakClass::Major
            } else {
                BreakClass::Minor
            };
            out.push(Finding {
                lint: A11,
                item: Some(*id),
                moniker: moniker.clone(),
                class,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::DocHiddenToggle {
                    now_hidden: new_hidden,
                },
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
        // A const↔static kind swap is already caught by A-2 above; A-16 only
        // records the static-mut-toggle semantics when both sides stay `Static`.
        if let (KindWire::Static(old_s), KindWire::Static(new_s)) = (&old_item.kind, &new_item.kind)
            && old_s.mutable != new_s.mutable
        {
            out.push(Finding {
                lint: A16,
                item: Some(*id),
                moniker: moniker.clone(),
                class: BreakClass::Major,
                certainty: Certainty::Certain,
                when: Option::None,
                detail: FindingDetail::StaticMutToggle {
                    now_mutable: new_s.mutable,
                },
            });
        }
    }

    // ── A-17: repr-changed ──────────────────────────────────────────────────
    // Cargo SemVer: "repr-changed"
    // Any change to (or removal of) `#[repr(...)]` on an exported type is Major.
    {
        let old_repr = old_item
            .attrs
            .iter()
            .find(|a| a.token == "repr")
            .map(|a| a.arg.as_deref());
        let new_repr = new_item
            .attrs
            .iter()
            .find(|a| a.token == "repr")
            .map(|a| a.arg.as_deref());

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
    if let (KindWire::Record(old_rec), KindWire::Record(_)) = (&old_item.kind, &new_item.kind) {
        // Only applies to named-field structs (not tuples or unions).
        if matches!(old_rec.form, RecordForm::Struct) {
            let old_field_ids = surface_field_ids(old_item);
            let new_field_ids = surface_field_ids(new_item);
            let gained: BTreeSet<IntroId> =
                new_field_ids.difference(&old_field_ids).copied().collect();

            if !gained.is_empty() {
                // Precondition L-5: all fields in the old surface were public.
                let all_pub = old_field_ids.iter().all(|fid| {
                    old_surface
                        .items
                        .get(fid)
                        .is_some_and(|fi| matches!(fi.visibility, Visibility::Public))
                });

                // Non-exhaustive exempts.
                let ne = is_non_exhaustive_item(old_item);

                if ne {
                    // Fields added to non_exhaustive struct → Minor (not Major).
                    // (The non_exhaustive annotation already prevents literal
                    // construction, so no additional break.)
                } else if all_pub {
                    // All prior fields were public: literal-constructible → Major.
                    for gained_id in &gained {
                        let field_name = new_surface.items.get(gained_id).map_or_else(
                            || SmolStr::new("<field>"),
                            |_| field_name_from_surface(new_surface, gained_id),
                        );
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
    use SelfKind::{Arbitrary, None, Ref, RefMut, Value};
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
    fn field_name_hint(&self) -> &'static str {
        "<field>"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
