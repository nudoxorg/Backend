//! Matcher-free structural diff between two [`PristineIntroTable`]s (§7.3).
//!
//! [`diff_tables`] iterates the sorted union of both tables' intro IDs, emits
//! lifecycle ops for entries that only appear in one table, and field-compares
//! entries that appear in both. Links are compared by their [`LinkDomainKey`]
//! sets (added/removed). The diff produces a [`PackageDelta`] ready for the
//! semver classifier.
//!
//! # Algorithm summary
//!
//! ```text
//! for id in sort(T0.ids ∪ T1.ids):
//!   (None, Some)  → Introduced
//!   (Some, None)  → Deleted
//!   (Some, Some) where hashes equal and links equal → skip
//!   else          → field-compare, emit ops
//! ops sorted: lifecycle < continuity < meta < kind-specific < links
//! ```
//!
//! # Sequence diff (params / recfield / variants)
//!
//! Parameter and child-id lists use LCS on element identity to detect
//! reorders vs. additions/removals:
//! - params: identity = `(name, TypeRefWire)`
//! - child lists (recfield / variant): identity = `IntroId`
//!
//! A reorder is detected when the two lists have equal *multisets* of elements
//! but different orderings. Only in that case is `*Reordered` emitted instead
//! of individual Add/Remove ops, matching the spec's canonical "multiset-equal
//! but different order" condition.

use std::collections::{BTreeMap, BTreeSet};

use ir::change::domain::LinkDomainKey;
use ir::change::{ChangeSetFingerprint, IntroId};
use smol_str::SmolStr;

use ir::apply::{LinkRecord, PristineIntroTable};
use ir::intro::sig_key;
use ir::wire::{
    AttrTok, AutoFact, AutoState, AutoTrait, GenericParamWire, KindWire, OwnedEntryPayload,
    TriState, TypeRefWire, WherePredWire,
};

use crate::diff::delta::{PackageDelta, PartialDelta};
use crate::diff::ir_op::{GenericsDelta, IrOp, SigKey, WherePred, op_sort_key};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Compute the structural delta between two [`PristineIntroTable`] generations.
///
/// - `from` / `to` are the channel-tip [`ChangeSetFingerprint`]s of T0 and T1.
/// - `resurrected_ids` is an optional set of IDs the caller knows are
///   resurrections (i.e. were deleted before T0); when provided, `Introduced`
///   ops for those IDs are replaced with `Resurrected`. Historical replay may
///   pass `None`.
pub fn diff_tables(
    t0: &PristineIntroTable,
    t1: &PristineIntroTable,
    from: ChangeSetFingerprint,
    to: ChangeSetFingerprint,
    resurrected_ids: Option<&BTreeSet<IntroId>>,
) -> PackageDelta {
    diff_tables_partial(t0, t1, from, to, resurrected_ids, None)
}

/// Like [`diff_tables`] but labels the result as a partial delta (§5.6).
///
/// `partial` should be `Some(PartialDelta { deletions_valid: false,
/// identity_final: false })` for checkpoint-to-checkpoint comparisons.
pub fn diff_tables_partial(
    t0: &PristineIntroTable,
    t1: &PristineIntroTable,
    from: ChangeSetFingerprint,
    to: ChangeSetFingerprint,
    resurrected_ids: Option<&BTreeSet<IntroId>>,
    partial: Option<PartialDelta>,
) -> PackageDelta {
    // Index each table's links by their canonical LinkDomainKey.
    let recs_t0: BTreeMap<LinkDomainKey, &LinkRecord> =
        t0.links().map(|l| (link_key(l), l)).collect();
    let recs_t1: BTreeMap<LinkDomainKey, &LinkRecord> =
        t1.links().map(|l| (link_key(l), l)).collect();

    // Sorted union of all intro IDs across both generations.
    let ids_t0: BTreeSet<IntroId> = t0.live_entries().map(|(id, _)| id).collect();
    let ids_t1: BTreeSet<IntroId> = t1.live_entries().map(|(id, _)| id).collect();
    let all_ids: BTreeSet<IntroId> = ids_t0.union(&ids_t1).copied().collect();

    let mut ops_map: BTreeMap<IntroId, Vec<IrOp>> = BTreeMap::new();

    // --- Link diff (§1.1): each changed link produces exactly ONE op, attached
    // to its canonical-owner intro (same-package: smaller IntroId; cross-package:
    // the local endpoint). Attributing here — not inside per-entry comparison —
    // is what stops a single link change from smearing across every edited entry.
    for (key, rec) in &recs_t1 {
        if !recs_t0.contains_key(key) {
            ops_map.entry(link_owner(rec, &all_ids)).or_default().push(IrOp::LinkAdded { key: *key });
        }
    }
    for (key, rec) in &recs_t0 {
        if !recs_t1.contains_key(key) {
            ops_map.entry(link_owner(rec, &all_ids)).or_default().push(IrOp::LinkRemoved { key: *key });
        }
    }

    // --- Per-entry lifecycle + field diff.
    for id in &all_ids {
        match (t0.get(*id), t1.get(*id)) {
            (None, Some(_)) => {
                let resurrected = resurrected_ids.is_some_and(|set| set.contains(id));
                ops_map.entry(*id).or_default().push(if resurrected {
                    IrOp::Resurrected
                } else {
                    IrOp::Introduced
                });
            }
            (Some(_), None) => ops_map.entry(*id).or_default().push(IrOp::Deleted),
            (Some(prev), Some(next)) => {
                let (parent0, parent1) = (t0.parent_of(*id), t1.parent_of(*id));
                // Fast path: identical payload AND parent ⇒ no field/continuity
                // ops. (payload_hash excludes the parent edge, so a pure move —
                // same payload, new parent — must still be compared for `Moved`.)
                if prev.payload_hash == next.payload_hash && parent0 == parent1 {
                    continue;
                }
                let field_ops = compare_payloads(prev, parent0, next, parent1);
                if !field_ops.is_empty() {
                    ops_map.entry(*id).or_default().extend(field_ops);
                }
            }
            (None, None) => unreachable!("id in union must exist in at least one table"),
        }
    }

    // Canonicalize each entry's op order (§7.3) and drop any left empty.
    let ops = ops_map
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(id, v)| (id, sort_ops(v)))
        .collect();

    PackageDelta { from, to, ops, partial }
}

// ---------------------------------------------------------------------------
// Link helpers
// ---------------------------------------------------------------------------

/// The canonical [`LinkDomainKey`] of a materialized link.
fn link_key(l: &LinkRecord) -> LinkDomainKey {
    LinkDomainKey::from_link(&l.a, &l.b, l.kind_a.as_u16(), l.kind_b.as_u16())
}

/// The canonical-owner intro of a link (§1.1): for a same-package link the
/// endpoint with the smaller `IntroId` (byte order); for a cross-package link
/// the local endpoint. `local_ids` is the union of both generations' intros.
fn link_owner(rec: &LinkRecord, local_ids: &BTreeSet<IntroId>) -> IntroId {
    let (a, b) = (rec.a.intro, rec.b.intro);
    let smaller = if a.as_bytes() <= b.as_bytes() { a } else { b };
    match (local_ids.contains(&a), local_ids.contains(&b)) {
        (true, true) | (false, false) => smaller,
        (true, false) => a,
        (false, true) => b,
    }
}

// ---------------------------------------------------------------------------
// Per-payload comparison
// ---------------------------------------------------------------------------

fn compare_payloads(
    p0: &OwnedEntryPayload,
    parent0: Option<IntroId>,
    p1: &OwnedEntryPayload,
    parent1: Option<IntroId>,
) -> Vec<IrOp> {
    let mut ops: Vec<IrOp> = Vec::new();
    let s0 = &p0.symbol;
    let s1 = &p1.symbol;

    // ---- Continuity ops ----
    if s0.name != s1.name {
        ops.push(IrOp::Renamed {
            old: SmolStr::new(&s0.name),
            new: SmolStr::new(&s1.name),
        });
    }
    if parent0 != parent1 {
        ops.push(IrOp::Moved { old_parent: parent0, new_parent: parent1 });
    }
    // SignatureEvolved: functions only
    if let (KindWire::Function(f0), KindWire::Function(f1)) = (&p0.kind, &p1.kind) {
        let sk0 = SigKey::from_content_blake3(sig_key(&f0.input_params, &f0.output_params, &f0.sig));
        let sk1 = SigKey::from_content_blake3(sig_key(&f1.input_params, &f1.output_params, &f1.sig));
        if sk0 != sk1 {
            ops.push(IrOp::SignatureEvolved { old: sk0, new: sk1 });
        }
    }

    // ---- Meta ops ----
    if s0.visibility != s1.visibility {
        ops.push(IrOp::VisChanged { old: s0.visibility, new: s1.visibility });
    }
    if s0.documentation != s1.documentation {
        ops.push(IrOp::DocChanged);
    }
    match (&s0.deprecation, &s1.deprecation) {
        (None, Some(_)) => ops.push(IrOp::DeprecationChanged { added: true }),
        (Some(_), None) => ops.push(IrOp::DeprecationChanged { added: false }),
        _ => {}
    }
    {
        let a0: BTreeSet<&str> = s0.aliases.iter().map(|s| s.as_str()).collect();
        let a1: BTreeSet<&str> = s1.aliases.iter().map(|s| s.as_str()).collect();
        if a0 != a1 {
            let added: Vec<SmolStr> = a1.difference(&a0).map(SmolStr::new).collect();
            let removed: Vec<SmolStr> = a0.difference(&a1).map(SmolStr::new).collect();
            ops.push(IrOp::AliasesChanged { added, removed });
        }
    }
    if s0.span_start != s1.span_start || s0.span_end != s1.span_end {
        ops.push(IrOp::SpanChanged);
    }
    if s0.source_path != s1.source_path {
        ops.push(IrOp::SourcePathChanged);
    }
    if s0.cfg != s1.cfg {
        ops.push(IrOp::CfgChanged { old: s0.cfg.clone(), new: s1.cfg.clone() });
    }
    {
        // Attrs: compare as BTreeSet of (token, arg) pairs.
        let to_set = |attrs: &Vec<AttrTok>| -> BTreeSet<(String, Option<String>)> {
            attrs.iter().map(|a| (a.token.clone(), a.arg.clone())).collect()
        };
        let set0 = to_set(&s0.attrs);
        let set1 = to_set(&s1.attrs);
        if set0 != set1 {
            let added: Vec<AttrTok> = s1.attrs.iter()
                .filter(|a| !set0.contains(&(a.token.clone(), a.arg.clone())))
                .cloned().collect();
            let removed: Vec<AttrTok> = s0.attrs.iter()
                .filter(|a| !set1.contains(&(a.token.clone(), a.arg.clone())))
                .cloned().collect();
            ops.push(IrOp::AttrsChanged { added, removed });
        }
    }

    // ---- Kind-specific ops ----
    // (Link ops are attributed to canonical owners in `diff_tables_partial`, not
    // here, so a link change lands on exactly one intro — never every edited one.)
    ops.extend(compare_kind_bodies(&p0.kind, &p1.kind));

    ops
}

// ---------------------------------------------------------------------------
// Kind-body comparison
// ---------------------------------------------------------------------------

fn compare_kind_bodies(k0: &KindWire, k1: &KindWire) -> Vec<IrOp> {
    let mut ops = Vec::new();

    match (k0, k1) {
        (KindWire::Function(f0), KindWire::Function(f1)) => {
            // Input params: LCS-based diff
            ops.extend(diff_params(&f0.input_params, &f1.input_params));
            // Output params: ReturnChanged when different
            if f0.output_params != f1.output_params {
                let old: Vec<TypeRefWire> = f0.output_params.iter().map(|p| p.ty.clone()).collect();
                let new: Vec<TypeRefWire> = f1.output_params.iter().map(|p| p.ty.clone()).collect();
                ops.push(IrOp::ReturnChanged { old, new });
            }
            // FnSigFlags
            if f0.sig != f1.sig {
                ops.push(IrOp::FnSigFlagsChanged { old: f0.sig.clone(), new: f1.sig.clone() });
            }
            // Generics
            let gd = diff_generics(&f0.generics, &f1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            // Where clauses
            ops.extend(diff_wheres(&f0.wheres, &f1.wheres));
        }

        (KindWire::Record(r0), KindWire::Record(r1)) => {
            if r0.form != r1.form {
                ops.push(IrOp::RecFormChanged);
            }
            ops.extend(diff_child_ids(&r0.fields, &r1.fields, |id| IrOp::ChildAdded { child: id }, |id| IrOp::ChildRemoved { child: id }, || IrOp::FieldsReordered));
            let gd = diff_generics(&r0.generics, &r1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            ops.extend(diff_wheres(&r0.wheres, &r1.wheres));
            ops.extend(diff_auto_facts(&r0.auto, &r1.auto));
        }

        (KindWire::Field(f0), KindWire::Field(f1)) => {
            if f0.ty != f1.ty {
                ops.push(IrOp::FieldTypeChanged { old: f0.ty.clone(), new: f1.ty.clone() });
            }
        }

        (KindWire::Enum(e0), KindWire::Enum(e1)) => {
            ops.extend(diff_child_ids(&e0.variants, &e1.variants, |id| IrOp::ChildAdded { child: id }, |id| IrOp::ChildRemoved { child: id }, || IrOp::FieldsReordered));
            let gd = diff_generics(&e0.generics, &e1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            ops.extend(diff_wheres(&e0.wheres, &e1.wheres));
            ops.extend(diff_auto_facts(&e0.auto, &e1.auto));
        }

        (KindWire::Variant(v0), KindWire::Variant(v1)) => {
            if v0.form != v1.form {
                ops.push(IrOp::VariantFormChanged);
            }
            if v0.discr != v1.discr {
                ops.push(IrOp::VariantDiscrChanged);
            }
            ops.extend(diff_child_ids(&v0.fields, &v1.fields, |id| IrOp::ChildAdded { child: id }, |id| IrOp::ChildRemoved { child: id }, || IrOp::FieldsReordered));
        }

        (KindWire::Trait(t0), KindWire::Trait(t1)) => {
            // Supertraits: set diff
            ops.extend(diff_typeref_set(&t0.supers, &t1.supers, |added, removed| {
                IrOp::SupertraitsChanged { added, removed }
            }));
            if t0.flags != t1.flags {
                ops.push(IrOp::TraitFlagsChanged { old: t0.flags.clone(), new: t1.flags.clone() });
            }
            let gd = diff_generics(&t0.generics, &t1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            ops.extend(diff_wheres(&t0.wheres, &t1.wheres));
        }

        (KindWire::Impl(i0), KindWire::Impl(i1)) => {
            if i0.of != i1.of || i0.self_ty != i1.self_ty || i0.flags != i1.flags {
                ops.push(IrOp::ImplHeaderChanged);
            }
            let gd = diff_generics(&i0.generics, &i1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            ops.extend(diff_wheres(&i0.wheres, &i1.wheres));
        }

        (KindWire::Const(c0), KindWire::Const(c1)) => {
            if c0.ty != c1.ty {
                ops.push(IrOp::ConstTypeChanged);
            }
            if c0.value != c1.value {
                ops.push(IrOp::ConstValueChanged);
            }
        }

        (KindWire::Static(s0), KindWire::Static(s1)) => {
            if s0.ty != s1.ty || s0.mutable != s1.mutable {
                ops.push(IrOp::ConstTypeChanged);
            }
        }

        (KindWire::Type(ta0), KindWire::Type(ta1)) => {
            if ta0.ty != ta1.ty {
                ops.push(IrOp::TypeExprChanged);
            }
            let gd = diff_generics(&ta0.generics, &ta1.generics);
            if !generics_delta_is_empty(&gd) {
                ops.push(IrOp::GenericsChanged { detail: gd });
            }
            ops.extend(diff_wheres(&ta0.wheres, &ta1.wheres));
            ops.extend(diff_auto_facts(&ta0.auto, &ta1.auto));
        }

        (KindWire::Reexport(r0), KindWire::Reexport(r1)) => {
            if r0.target != r1.target {
                ops.push(IrOp::ReexportRetargeted {
                    old: r0.target.clone(),
                    new: r1.target.clone(),
                });
            }
        }

        (KindWire::Module(_), KindWire::Module(_)) => {
            // ModuleWire has no fields; no kind-specific ops possible.
        }

        _ => {
            // Kind mismatch — should not occur at diff time (kind is in IntroId
            // preimage in v2, so a kind change yields a new id). Emit nothing;
            // the lifecycle Introduced/Deleted pair covers this at the id level.
        }
    }

    ops
}

// ---------------------------------------------------------------------------
// Param LCS diff
// ---------------------------------------------------------------------------

/// Element identity for parameter LCS: `(name.cloned(), TypeRefWire)`.
#[derive(PartialEq, Eq, Clone)]
struct ParamIdent {
    name: Option<String>,
    ty: TypeRefWire,
}

fn param_ident(p: &ir::wire::ParamWire) -> ParamIdent {
    ParamIdent { name: p.name.clone(), ty: p.ty.clone() }
}

/// LCS-based diff of input parameter lists (§7.3).
///
/// - Same multiset of `(name, type)` in a different order ⇒ `ParamsReordered`.
/// - Otherwise params are aligned by an identity that **survives a type change**
///   — named params match on name, unnamed params match on type+position — so a
///   param whose type changed pairs with its old self and yields `ParamTypeChanged`
///   (carrying the new type, hence round-trippable) rather than a remove+add pair
///   with a placeholder type. Unpaired old/new params yield `ParamRemoved`/`ParamAdded`.
fn diff_params(
    p0: &[ir::wire::ParamWire],
    p1: &[ir::wire::ParamWire],
) -> Vec<IrOp> {
    if p0 == p1 {
        return vec![];
    }

    // Reorder: identical multiset of (name, type), different order.
    let mut sorted0: Vec<ParamIdent> = p0.iter().map(param_ident).collect();
    let mut sorted1: Vec<ParamIdent> = p1.iter().map(param_ident).collect();
    sorted0.sort_by_key(param_sort_key);
    sorted1.sort_by_key(param_sort_key);
    if sorted0 == sorted1 {
        return vec![IrOp::ParamsReordered];
    }

    // Align by type-insensitive identity so a type change pairs with its origin.
    let lcs = lcs_pairs_by(p0, p1, params_align);

    let matched0: BTreeSet<usize> = lcs.iter().map(|&(i0, _)| i0).collect();
    let matched1: BTreeSet<usize> = lcs.iter().map(|&(_, i1)| i1).collect();

    let mut ops = Vec::new();
    for i0 in 0..p0.len() {
        if !matched0.contains(&i0) {
            ops.push(IrOp::ParamRemoved { index: i0 as u16 });
        }
    }
    for i1 in 0..p1.len() {
        if !matched1.contains(&i1) {
            ops.push(IrOp::ParamAdded { index: i1 as u16 });
        }
    }
    for &(i0, i1) in &lcs {
        let (a, b) = (&p0[i0], &p1[i1]);
        if a.ty != b.ty {
            ops.push(IrOp::ParamTypeChanged {
                index: i1 as u16,
                old: a.ty.clone(),
                new: b.ty.clone(),
            });
        } else if a.name != b.name {
            ops.push(IrOp::ParamRenamed { index: i1 as u16 });
        }
    }
    ops
}

/// Alignment predicate for the param LCS: named params match on name (so a type
/// change keeps the pairing); unnamed params match only when their types agree.
fn params_align(a: &ir::wire::ParamWire, b: &ir::wire::ParamWire) -> bool {
    match (&a.name, &b.name) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.ty == b.ty,
        _ => false,
    }
}

fn param_sort_key(p: &ParamIdent) -> (Option<String>, String) {
    (p.name.clone(), format!("{:?}", p.ty))
}

/// Longest-common-subsequence index pairing under a custom match predicate.
///
/// Standard O(n·m) DP; parameter/child lists are small in practice.
fn lcs_pairs_by<T>(a: &[T], b: &[T], eq: impl Fn(&T, &T) -> bool) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if eq(&a[i - 1], &b[j - 1]) {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if eq(&a[i - 1], &b[j - 1]) {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

// ---------------------------------------------------------------------------
// Child ID list diff (recfield / variants)
// ---------------------------------------------------------------------------

fn diff_child_ids<FA, FR, FO>(
    ids0: &[IntroId],
    ids1: &[IntroId],
    make_added: FA,
    make_removed: FR,
    make_reordered: FO,
) -> Vec<IrOp>
where
    FA: Fn(IntroId) -> IrOp,
    FR: Fn(IntroId) -> IrOp,
    FO: Fn() -> IrOp,
{
    if ids0 == ids1 {
        return vec![];
    }

    let set0: BTreeSet<IntroId> = ids0.iter().copied().collect();
    let set1: BTreeSet<IntroId> = ids1.iter().copied().collect();

    if set0 == set1 {
        // Same elements, different order.
        return vec![make_reordered()];
    }

    let mut ops = Vec::new();
    for &id in ids0 {
        if !set1.contains(&id) {
            ops.push(make_removed(id));
        }
    }
    for &id in ids1 {
        if !set0.contains(&id) {
            ops.push(make_added(id));
        }
    }
    ops
}

// ---------------------------------------------------------------------------
// Generic param diff
// ---------------------------------------------------------------------------

fn diff_generics(g0: &[GenericParamWire], g1: &[GenericParamWire]) -> GenericsDelta {
    // Use a simple name-keyed comparison; names are canonical identifiers.
    let names0: BTreeSet<&str> = g0.iter().map(generic_name).collect();
    let names1: BTreeSet<&str> = g1.iter().map(generic_name).collect();

    let added: Vec<SmolStr> = names1.difference(&names0).map(|n| SmolStr::new(*n)).collect();
    let removed: Vec<SmolStr> = names0.difference(&names1).map(|n| SmolStr::new(*n)).collect();

    let mut bounds_tightened = Vec::new();
    let mut bounds_loosened = Vec::new();
    let mut default_added = Vec::new();
    let mut default_removed = Vec::new();

    // For params present in both, check bound and default changes.
    for g_new in g1.iter() {
        let name = generic_name(g_new);
        if let Some(g_old) = g0.iter().find(|g| generic_name(g) == name) {
            // Bounds comparison (type params only).
            if let (
                GenericParamWire::Type { bounds: b0, default: d0, .. },
                GenericParamWire::Type { bounds: b1, default: d1, .. },
            ) = (g_old, g_new) {
                let set0: BTreeSet<String> = b0.iter().map(|t| format!("{:?}", t)).collect();
                let set1: BTreeSet<String> = b1.iter().map(|t| format!("{:?}", t)).collect();
                if set1.is_superset(&set0) && set1 != set0 {
                    bounds_tightened.push(SmolStr::new(name));
                } else if set0.is_superset(&set1) && set0 != set1 {
                    bounds_loosened.push(SmolStr::new(name));
                }
                match (d0, d1) {
                    (None, Some(_)) => default_added.push(SmolStr::new(name)),
                    (Some(_), None) => default_removed.push(SmolStr::new(name)),
                    _ => {}
                }
            }
        }
    }

    GenericsDelta { added, removed, bounds_tightened, bounds_loosened, default_added, default_removed }
}

fn generic_name(g: &GenericParamWire) -> &str {
    match g {
        GenericParamWire::Lifetime { name } => name,
        GenericParamWire::Type { name, .. } => name,
        GenericParamWire::Const { name, .. } => name,
    }
}

fn generics_delta_is_empty(gd: &GenericsDelta) -> bool {
    gd.added.is_empty()
        && gd.removed.is_empty()
        && gd.bounds_tightened.is_empty()
        && gd.bounds_loosened.is_empty()
        && gd.default_added.is_empty()
        && gd.default_removed.is_empty()
}

// ---------------------------------------------------------------------------
// Where clause diff
// ---------------------------------------------------------------------------

fn diff_wheres(w0: &[WherePredWire], w1: &[WherePredWire]) -> Vec<IrOp> {
    if w0 == w1 {
        return vec![];
    }
    // Represent each predicate by its debug string for set comparison.
    let key = |p: &WherePredWire| format!("{:?}", p);
    let set0: BTreeSet<String> = w0.iter().map(key).collect();
    let set1: BTreeSet<String> = w1.iter().map(key).collect();
    if set0 == set1 {
        return vec![];
    }
    let added: Vec<WherePred> = w1.iter().filter(|p| !set0.contains(&format!("{:?}", p))).cloned().collect();
    let removed: Vec<WherePred> = w0.iter().filter(|p| !set1.contains(&format!("{:?}", p))).cloned().collect();
    vec![IrOp::WhereChanged { added, removed }]
}

// ---------------------------------------------------------------------------
// Auto-trait fact diff
// ---------------------------------------------------------------------------

fn diff_auto_facts(a0: &[AutoFact], a1: &[AutoFact]) -> Vec<IrOp> {
    if a0 == a1 {
        return vec![];
    }
    // Build maps by AutoTrait.
    let to_map = |facts: &[AutoFact]| -> BTreeMap<String, AutoState> {
        facts.iter().map(|f| (format!("{:?}", f.trait_), f.state.clone())).collect()
    };
    let m0 = to_map(a0);
    let m1 = to_map(a1);

    // Collect all trait keys.
    let all_traits: BTreeSet<&String> = m0.keys().chain(m1.keys()).collect();
    let mut changed = Vec::new();
    for trait_key in all_traits {
        let old_state = m0.get(trait_key);
        let new_state = m1.get(trait_key);
        if old_state != new_state {
            let old_ts = autostate_to_tristate(old_state);
            let new_ts = autostate_to_tristate(new_state);
            if let Some(t) = parse_autotrait(trait_key) {
                changed.push((t, old_ts, new_ts));
            }
        }
    }
    if changed.is_empty() {
        vec![]
    } else {
        vec![IrOp::AutoTraitsChanged { changed }]
    }
}

fn autostate_to_tristate(s: Option<&AutoState>) -> TriState {
    match s {
        None => TriState::Unknown,
        Some(AutoState::Yes) => TriState::Yes,
        Some(AutoState::No) => TriState::No,
        Some(AutoState::Cond) => TriState::Unknown,
    }
}

fn parse_autotrait(s: &str) -> Option<AutoTrait> {
    match s {
        "Send" => Some(AutoTrait::Send),
        "Sync" => Some(AutoTrait::Sync),
        "Unpin" => Some(AutoTrait::Unpin),
        "UnwindSafe" => Some(AutoTrait::UnwindSafe),
        "RefUnwindSafe" => Some(AutoTrait::RefUnwindSafe),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Typeref set diff helper
// ---------------------------------------------------------------------------

fn diff_typeref_set<F>(
    s0: &[TypeRefWire],
    s1: &[TypeRefWire],
    make_op: F,
) -> Vec<IrOp>
where
    F: Fn(Vec<TypeRefWire>, Vec<TypeRefWire>) -> IrOp,
{
    if s0 == s1 {
        return vec![];
    }
    let key = |t: &TypeRefWire| format!("{:?}", t);
    let set0: BTreeSet<String> = s0.iter().map(key).collect();
    let set1: BTreeSet<String> = s1.iter().map(key).collect();
    if set0 == set1 {
        return vec![];
    }
    let added: Vec<TypeRefWire> = s1.iter().filter(|t| !set0.contains(&format!("{:?}", t))).cloned().collect();
    let removed: Vec<TypeRefWire> = s0.iter().filter(|t| !set1.contains(&format!("{:?}", t))).cloned().collect();
    vec![make_op(added, removed)]
}

// ---------------------------------------------------------------------------
// Op sorting
// ---------------------------------------------------------------------------

/// Sort ops within an id's list into canonical order (§7.3).
pub(crate) fn sort_ops(mut ops: Vec<IrOp>) -> Vec<IrOp> {
    ops.sort_by_key(op_sort_key);
    ops
}
