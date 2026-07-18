//! Round-trip application of a [`PackageDelta`] to a [`PristineIntroTable`] (§7.5).
//!
//! [`apply_delta`] produces the T1 table implied by the delta, sufficient to
//! verify `apply_delta(T0, delta) == T1` on constructed fixture pairs.
//!
//! # Round-trip coverage
//!
//! The following [`IrOp`] variants are **losslessly applied** (round-trip law holds):
//!
//! | Category | Ops |
//! |---|---|
//! | Lifecycle | `Introduced`¹, `Deleted`, `Resurrected`¹ |
//! | Continuity | `Renamed`, `Moved`, `SignatureEvolved`² |
//! | Meta | `VisChanged`, `AliasesChanged`, `CfgChanged`, `AttrsChanged`, `DeprecationChanged`³ |
//! | Function | `ParamRemoved`, `ParamTypeChanged`, `ReturnChanged`, `FnSigFlagsChanged` |
//! | Record/Enum | `FieldTypeChanged`, `ChildAdded`, `ChildRemoved`, `SupertraitsChanged` |
//! | Traits | `TraitFlagsChanged`, `WhereChanged` |
//! | Reexports | `ReexportRetargeted` |
//!
//! ¹ `Introduced`/`Resurrected` require the caller to supply T1's full payload
//!   (the ops carry no payload). Use `apply_delta_with_t1` for those entries.
//!
//! ² `SignatureEvolved` is a derived fact (computed from the kind body); applying
//!   body mutations implicitly updates the SigKey.
//!
//! ³ `DeprecationChanged { added: true }` cannot reconstruct the new
//!   `DeprecationWire` content without T1.
//!
//! # Ops **not** round-trippable (and why)
//!
//! | Op | Reason |
//! |---|---|
//! | `Introduced` / `Resurrected` | No payload in the op; caller must supply T1 entry |
//! | `DocChanged` | Signal only — no old/new text |
//! | `SpanChanged` | Signal only — no old/new span |
//! | `SourcePathChanged` | Signal only — no old/new path |
//! | `ParamAdded` | Inserts a placeholder (zero-id type); full param must come from T1 |
//! | `ParamRenamed` | No new name in the op |
//! | `ParamsReordered` | No new order in the op |
//! | `RecFormChanged` | No new form in the op |
//! | `FieldsReordered` | No new order in the op |
//! | `VariantFormChanged` | No new form in the op |
//! | `VariantDiscrChanged` | No new discriminant in the op |
//! | `ImplHeaderChanged` | Signal only — no old/new header |
//! | `ConstTypeChanged` | No new type payload |
//! | `ConstValueChanged` | Signal only — no old/new value |
//! | `TypeExprChanged` | No new `TypeWire` in the op |
//! | `GenericsChanged` | Summary delta only; added params become placeholder lifetimes |
//! | `LinkAdded` / `LinkRemoved` | `LinkDomainKey` is not invertible to a `LinkRecord` |
//! | `AutoTraitsChanged` | `TriState::Unknown` cannot distinguish `Cond` from absent |
//! | `DeprecationChanged { added: true }` | New `DeprecationWire` not in op |

use std::collections::BTreeSet;

use nudox_change::IntroId;

use nudox_ir::apply::{LinkRecord, PristineIntroTable};
use nudox_ir::wire::{
    GenericParamWire, KindWire, OwnedEntryPayload, TypeRefWire, WherePredWire,
};

use crate::delta::PackageDelta;
use crate::ir_op::IrOp;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error returned when a delta references an intro absent from the base table.
#[derive(Debug)]
pub struct ApplyError(pub String);

impl core::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for ApplyError {}

// ---------------------------------------------------------------------------
// apply_delta
// ---------------------------------------------------------------------------

/// Apply a [`PackageDelta`] to a base [`PristineIntroTable`], producing T1.
///
/// Entries with `Introduced`/`Resurrected` ops are skipped (the op carries no
/// payload). Callers that need those entries in T1 should supply them via
/// `apply_delta_with_t1`. The round-trip test fixture inserts T1 entries
/// separately for introduced symbols.
pub fn apply_delta(
    base: &PristineIntroTable,
    delta: &PackageDelta,
) -> Result<PristineIntroTable, ApplyError> {
    apply_delta_with_t1(base, delta, None)
}

/// Apply a [`PackageDelta`] to `base`, using `t1` to fill `Introduced` and
/// `Resurrected` entries when available.
///
/// When `t1` is `Some`, any entry with `Introduced`/`Resurrected` is looked up
/// in `t1` and inserted verbatim (preserving the round-trip law). When `t1` is
/// `None`, those entries are skipped.
pub fn apply_delta_with_t1(
    base: &PristineIntroTable,
    delta: &PackageDelta,
    t1: Option<&PristineIntroTable>,
) -> Result<PristineIntroTable, ApplyError> {
    // Snapshot all base entries into a BTreeMap so we can mutate them.
    let mut entries: std::collections::BTreeMap<IntroId, (OwnedEntryPayload, Option<IntroId>)> = base
        .live_entries()
        .map(|(id, payload)| (id, (payload.clone(), base.parent_of(id))))
        .collect();

    let base_links: Vec<LinkRecord> = base.links().cloned().collect();

    for (id, ops) in &delta.ops {
        let id = *id;
        let has_deleted = ops.iter().any(|op| matches!(op, IrOp::Deleted));
        let has_introduced = ops.iter().any(|op| matches!(op, IrOp::Introduced | IrOp::Resurrected));

        if has_deleted {
            entries.remove(&id);
            continue;
        }

        if has_introduced {
            // The op carries no payload, so T1 must supply it; skip otherwise.
            if let Some(t1) = t1
                && let Some(payload) = t1.get(id)
            {
                entries.insert(id, (payload.clone(), t1.parent_of(id)));
            }
            continue;
        }

        let (payload, parent) = entries
            .get_mut(&id)
            .ok_or_else(|| ApplyError(format!("apply_delta: id {} not found in base", id.to_hex())))?;

        let mut new_parent = *parent;

        for op in ops {
            if let IrOp::Moved { new_parent: np, .. } = op {
                new_parent = *np;
            } else {
                apply_op_to_payload(op, payload)?;
            }
        }

        // Re-seal after mutations.
        payload.payload_hash = OwnedEntryPayload::compute_payload_hash(
            &payload.symbol,
            &payload.kind_disc,
            &payload.kind,
            &payload.flags,
        );
        *parent = new_parent;
    }

    // Reconstruct PristineIntroTable.
    let mut result = PristineIntroTable::new();
    for (id, (payload, parent)) in entries {
        result.insert_live(id, payload, parent);
    }
    for link in base_links {
        result.insert_link(link);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Per-op mutation logic
// ---------------------------------------------------------------------------

fn apply_op_to_payload(op: &IrOp, payload: &mut OwnedEntryPayload) -> Result<(), ApplyError> {
    match op {
        // Already handled at caller.
        IrOp::Introduced | IrOp::Resurrected | IrOp::Deleted | IrOp::Moved { .. } => {}

        IrOp::Renamed { new, .. } => {
            payload.symbol.name = new.to_string();
        }
        IrOp::SignatureEvolved { .. } => {
            // Derived; body mutations update the SigKey implicitly.
        }

        // Meta
        IrOp::VisChanged { new, .. } => {
            payload.symbol.visibility = *new;
        }
        IrOp::DocChanged => {
            // Signal only — no old/new text in op.
        }
        IrOp::DeprecationChanged { added } => {
            if !added {
                payload.symbol.deprecation = None;
            }
            // added=true: new DeprecationWire not in op; skipped.
        }
        IrOp::AliasesChanged { added, removed } => {
            let removed_set: BTreeSet<String> = removed.iter().map(|s| s.to_string()).collect();
            payload.symbol.aliases.retain(|a| !removed_set.contains(a));
            for a in added {
                if !payload.symbol.aliases.iter().any(|x| x == a.as_str()) {
                    payload.symbol.aliases.push(a.to_string());
                }
            }
        }
        IrOp::SpanChanged => {}
        IrOp::SourcePathChanged => {}
        IrOp::CfgChanged { new, .. } => {
            payload.symbol.cfg = new.clone();
        }
        IrOp::AttrsChanged { added, removed } => {
            let removed_set: BTreeSet<(String, Option<String>)> =
                removed.iter().map(|a| (a.token.clone(), a.arg.clone())).collect();
            payload.symbol.attrs.retain(|a| !removed_set.contains(&(a.token.clone(), a.arg.clone())));
            for a in added {
                let key = (a.token.clone(), a.arg.clone());
                if !payload.symbol.attrs.iter().any(|x| (x.token.clone(), x.arg.clone()) == key) {
                    payload.symbol.attrs.push(a.clone());
                }
            }
        }

        // Functions
        IrOp::ParamAdded { index } => {
            if let KindWire::Function(ref mut f) = payload.kind {
                let mut params = f.input_params.to_vec();
                let idx = (*index as usize).min(params.len());
                // Placeholder; caller should supply T1 for true round-trip.
                params.insert(idx, nudox_ir::wire::ParamWire {
                    name: None,
                    ty: TypeRefWire::Same(IntroId::from_raw([0u8; 32])),
                });
                f.input_params = params.into_boxed_slice();
            }
        }
        IrOp::ParamRemoved { index } => {
            if let KindWire::Function(ref mut f) = payload.kind {
                let mut params = f.input_params.to_vec();
                let idx = *index as usize;
                if idx < params.len() {
                    params.remove(idx);
                }
                f.input_params = params.into_boxed_slice();
            }
        }
        IrOp::ParamRenamed { .. } => {
            // No new name in op.
        }
        IrOp::ParamTypeChanged { index, new, .. } => {
            if let KindWire::Function(ref mut f) = payload.kind {
                let mut params = f.input_params.to_vec();
                if let Some(p) = params.get_mut(*index as usize) {
                    p.ty = new.clone();
                }
                f.input_params = params.into_boxed_slice();
            }
        }
        IrOp::ParamsReordered => {}
        IrOp::ReturnChanged { new, .. } => {
            if let KindWire::Function(ref mut f) = payload.kind {
                let new_outputs: Vec<nudox_ir::wire::ParamWire> = new.iter()
                    .map(|ty| nudox_ir::wire::ParamWire { name: None, ty: ty.clone() })
                    .collect();
                f.output_params = new_outputs.into_boxed_slice();
            }
        }
        IrOp::FnSigFlagsChanged { new, .. } => {
            if let KindWire::Function(ref mut f) = payload.kind {
                f.sig = new.clone();
            }
        }
        IrOp::GenericsChanged { detail } => {
            let removed_set: BTreeSet<String> = detail.removed.iter().map(|s| s.to_string()).collect();
            let apply_to = |generics: &mut Box<[GenericParamWire]>| {
                let mut gs: Vec<GenericParamWire> = generics.iter()
                    .filter(|g| {
                        let name: &str = match g {
                            GenericParamWire::Lifetime { name } => name,
                            GenericParamWire::Type { name, .. } => name,
                            GenericParamWire::Const { name, .. } => name,
                        };
                        !removed_set.contains(name)
                    })
                    .cloned()
                    .collect();
                for name in &detail.added {
                    gs.push(GenericParamWire::Lifetime { name: name.to_string() });
                }
                *generics = gs.into_boxed_slice();
            };
            match &mut payload.kind {
                KindWire::Function(f) => apply_to(&mut f.generics),
                KindWire::Record(r) => apply_to(&mut r.generics),
                KindWire::Enum(e) => apply_to(&mut e.generics),
                KindWire::Trait(t) => apply_to(&mut t.generics),
                KindWire::Impl(i) => apply_to(&mut i.generics),
                KindWire::Type(ta) => apply_to(&mut ta.generics),
                _ => {}
            }
        }
        IrOp::WhereChanged { added, removed } => {
            let key = |p: &WherePredWire| format!("{:?}", p);
            let removed_keys: BTreeSet<String> = removed.iter().map(key).collect();
            let apply_to = |wheres: &mut Box<[WherePredWire]>| {
                let mut ws: Vec<WherePredWire> = wheres.iter()
                    .filter(|p| !removed_keys.contains(&format!("{:?}", p)))
                    .cloned()
                    .collect();
                ws.extend(added.iter().cloned());
                *wheres = ws.into_boxed_slice();
            };
            match &mut payload.kind {
                KindWire::Function(f) => apply_to(&mut f.wheres),
                KindWire::Record(r) => apply_to(&mut r.wheres),
                KindWire::Enum(e) => apply_to(&mut e.wheres),
                KindWire::Trait(t) => apply_to(&mut t.wheres),
                KindWire::Impl(i) => apply_to(&mut i.wheres),
                KindWire::Type(ta) => apply_to(&mut ta.wheres),
                _ => {}
            }
        }

        // Records / Enums / Variants
        IrOp::FieldTypeChanged { new, .. } => {
            if let KindWire::Field(ref mut f) = payload.kind {
                f.ty = new.clone();
            }
        }
        IrOp::RecFormChanged => {}
        IrOp::FieldsReordered => {}
        IrOp::ChildAdded { child } => {
            match &mut payload.kind {
                KindWire::Record(r) => { let mut v = r.fields.to_vec(); v.push(*child); r.fields = v.into_boxed_slice(); }
                KindWire::Enum(e) => { let mut v = e.variants.to_vec(); v.push(*child); e.variants = v.into_boxed_slice(); }
                KindWire::Variant(v) => { let mut f = v.fields.to_vec(); f.push(*child); v.fields = f.into_boxed_slice(); }
                _ => {}
            }
        }
        IrOp::ChildRemoved { child } => {
            match &mut payload.kind {
                KindWire::Record(r) => { let mut v = r.fields.to_vec(); v.retain(|id| id != child); r.fields = v.into_boxed_slice(); }
                KindWire::Enum(e) => { let mut v = e.variants.to_vec(); v.retain(|id| id != child); e.variants = v.into_boxed_slice(); }
                KindWire::Variant(v) => { let mut f = v.fields.to_vec(); f.retain(|id| id != child); v.fields = f.into_boxed_slice(); }
                _ => {}
            }
        }
        IrOp::VariantFormChanged => {}
        IrOp::VariantDiscrChanged => {}

        // Traits / Impls
        IrOp::SupertraitsChanged { added, removed } => {
            if let KindWire::Trait(ref mut t) = payload.kind {
                let key = |r: &TypeRefWire| format!("{:?}", r);
                let removed_keys: BTreeSet<String> = removed.iter().map(key).collect();
                let mut supers: Vec<TypeRefWire> = t.supers.iter()
                    .filter(|s| !removed_keys.contains(&format!("{:?}", s)))
                    .cloned()
                    .collect();
                supers.extend(added.iter().cloned());
                t.supers = supers.into_boxed_slice();
            }
        }
        IrOp::TraitFlagsChanged { new, .. } => {
            if let KindWire::Trait(ref mut t) = payload.kind {
                t.flags = new.clone();
            }
        }
        IrOp::ImplHeaderChanged => {}

        // Consts / aliases
        IrOp::ConstTypeChanged => {}
        IrOp::ConstValueChanged => {}
        IrOp::TypeExprChanged => {}

        // Reexports
        IrOp::ReexportRetargeted { new, .. } => {
            if let KindWire::Reexport(ref mut r) = payload.kind {
                r.target = new.clone();
            }
        }

        // Not round-trippable
        IrOp::LinkAdded { .. } | IrOp::LinkRemoved { .. } => {}
        IrOp::AutoTraitsChanged { .. } => {}
    }
    Ok(())
}
