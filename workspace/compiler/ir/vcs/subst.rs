//! σ substitution cascade (§5, K-Subst-Cascade).
//!
//! When the continuity matcher decides a staged wire entry `w` *is* a deleted
//! durable entry `d`, **every in-generation reference to `w`** must be rewritten
//! `w → d` and every touched payload **re-sealed** (its `payload_hash` covers
//! those refs). This module owns that rewrite. It is deliberately total over the
//! wire-v2 schema: parent edges and link endpoints are handled by the caller
//! (they live outside the payload), but every reference *inside* a payload —
//! `TypeRefWire::Same` in params / fields / type exprs / bounds / supertraits /
//! impl headers, `recfield` / `variant` child-id lists, and the `reexport`
//! target — is remapped here.
//!
//! `substitute_and_reseal` is idempotent on the identity map and, crucially,
//! yields bytes that are **byte-identical to a from-scratch record of the final
//! state** (acceptance C-10): the only thing σ changes is *which* durable id a
//! ref points at, never the surrounding encoding.

use std::collections::BTreeMap;

use crate::wire::{
    ConstWire, EnumWire, FieldWire, FunctionWire, GenericParamWire, ImplWire, KindWire,
    OwnedEntryPayload, ParamWire, PrimitiveWire, RecordWire, ReexportWire, StaticWire, TraitWire,
    TypeAliasWire, TypeRefWire, TypeWire, VariantWire, WherePredWire,
};
use ir::change::{IntroId, StableRef};

/// The σ map: wire-id → durable-id. Missing keys map to themselves.
pub type Sigma = BTreeMap<IntroId, IntroId>;

#[inline]
fn map_id(sigma: &Sigma, id: IntroId) -> IntroId {
    sigma.get(&id).copied().unwrap_or(id)
}

/// Remap a same-package `IntroId`; foreign `StableRef` intros are left untouched
/// unless the σ map happens to carry them (it never does — its keys are this
/// package's wire ids), so `StableRef` remapping is safe to apply blindly.
#[inline]
fn map_stable_ref(sigma: &Sigma, sr: &StableRef) -> StableRef {
    StableRef::new(sr.package.clone(), map_id(sigma, sr.intro))
}

fn map_type_ref(sigma: &Sigma, tr: &TypeRefWire) -> TypeRefWire {
    match tr {
        TypeRefWire::Same(id) => TypeRefWire::Same(map_id(sigma, *id)),
        // Foreign refs address another package; σ (this-package wire ids) never
        // touches them.
        TypeRefWire::Foreign(sr) => TypeRefWire::Foreign(sr.clone()),
        // Neither carries a same-package `IntroId` at all — an unlinked
        // foreign key and an unresolved-external name are both entirely
        // outside this package's wire-id space, exactly like `Foreign` above.
        TypeRefWire::ForeignUnlinked(key) => TypeRefWire::ForeignUnlinked(key.clone()),
        TypeRefWire::UnresolvedExternal(name) => TypeRefWire::UnresolvedExternal(name.clone()),
    }
}

fn map_type_wire(sigma: &Sigma, tw: &TypeWire) -> TypeWire {
    let refs = |xs: &[TypeRefWire]| xs.iter().map(|t| map_type_ref(sigma, t)).collect();
    match tw {
        TypeWire::SelfType | TypeWire::Never | TypeWire::Any | TypeWire::UnresolvedExternal(_) => {
            tw.clone()
        }
        TypeWire::Primitive(p) => TypeWire::Primitive(map_primitive(sigma, p)),
        TypeWire::Tuple(xs) => TypeWire::Tuple(refs(xs)),
        TypeWire::Slice(t) => TypeWire::Slice(Box::new(map_type_ref(sigma, t))),
        TypeWire::Array { ty, length } => TypeWire::Array {
            ty: Box::new(map_type_ref(sigma, ty)),
            length: *length,
        },
        TypeWire::Union(xs) => TypeWire::Union(refs(xs)),
        TypeWire::Intersection(xs) => TypeWire::Intersection(refs(xs)),
    }
}

fn map_primitive(sigma: &Sigma, p: &PrimitiveWire) -> PrimitiveWire {
    match p {
        PrimitiveWire::MutPointer(t) => PrimitiveWire::MutPointer(Box::new(map_type_ref(sigma, t))),
        PrimitiveWire::ConstPointer(t) => {
            PrimitiveWire::ConstPointer(Box::new(map_type_ref(sigma, t)))
        }
        PrimitiveWire::Reference {
            lifetime,
            mutable,
            ty,
        } => PrimitiveWire::Reference {
            lifetime: lifetime.clone(),
            mutable: *mutable,
            ty: Box::new(map_type_ref(sigma, ty)),
        },
        // Scalar primitives carry no refs.
        other => other.clone(),
    }
}

fn map_generics(sigma: &Sigma, gs: &[GenericParamWire]) -> Box<[GenericParamWire]> {
    gs.iter()
        .map(|g| match g {
            GenericParamWire::Lifetime { name } => {
                GenericParamWire::Lifetime { name: name.clone() }
            }
            GenericParamWire::Type {
                name,
                bounds,
                default,
            } => GenericParamWire::Type {
                name: name.clone(),
                bounds: bounds.iter().map(|b| map_type_ref(sigma, b)).collect(),
                default: default.as_ref().map(|d| map_type_wire(sigma, d)),
            },
            GenericParamWire::Const { name, ty, default } => GenericParamWire::Const {
                name: name.clone(),
                ty: map_type_ref(sigma, ty),
                default: default.clone(),
            },
        })
        .collect()
}

fn map_wheres(sigma: &Sigma, ws: &[WherePredWire]) -> Box<[WherePredWire]> {
    ws.iter()
        .map(|w| WherePredWire {
            target: map_type_wire(sigma, &w.target),
            bounds: w.bounds.iter().map(|b| map_type_ref(sigma, b)).collect(),
        })
        .collect()
}

#[inline]
fn map_ids(sigma: &Sigma, ids: &[IntroId]) -> Box<[IntroId]> {
    ids.iter().map(|id| map_id(sigma, *id)).collect()
}

fn map_kind(sigma: &Sigma, kind: &KindWire) -> KindWire {
    match kind {
        KindWire::Module(m) => KindWire::Module(m.clone()),
        KindWire::Record(r) => KindWire::Record(RecordWire {
            form: r.form,
            fields: map_ids(sigma, &r.fields),
            generics: map_generics(sigma, &r.generics),
            wheres: map_wheres(sigma, &r.wheres),
            auto: r.auto.clone(),
        }),
        KindWire::Field(f) => KindWire::Field(FieldWire {
            ty: f.ty.as_ref().map(|t| map_type_ref(sigma, t)),
        }),
        KindWire::Function(f) => KindWire::Function(FunctionWire {
            input_params: map_params(sigma, &f.input_params),
            output_params: map_params(sigma, &f.output_params),
            sig: f.sig.clone(),
            generics: map_generics(sigma, &f.generics),
            wheres: map_wheres(sigma, &f.wheres),
        }),
        KindWire::Type(t) => KindWire::Type(TypeAliasWire {
            ty: map_type_wire(sigma, &t.ty),
            generics: map_generics(sigma, &t.generics),
            wheres: map_wheres(sigma, &t.wheres),
            auto: t.auto.clone(),
        }),
        KindWire::Trait(t) => KindWire::Trait(TraitWire {
            supers: t.supers.iter().map(|s| map_type_ref(sigma, s)).collect(),
            flags: t.flags.clone(),
            generics: map_generics(sigma, &t.generics),
            wheres: map_wheres(sigma, &t.wheres),
        }),
        KindWire::Impl(i) => KindWire::Impl(ImplWire {
            of: i.of.as_ref().map(|t| map_type_ref(sigma, t)),
            self_ty: map_type_wire(sigma, &i.self_ty),
            flags: i.flags.clone(),
            generics: map_generics(sigma, &i.generics),
            wheres: map_wheres(sigma, &i.wheres),
        }),
        KindWire::Enum(e) => KindWire::Enum(EnumWire {
            variants: map_ids(sigma, &e.variants),
            generics: map_generics(sigma, &e.generics),
            wheres: map_wheres(sigma, &e.wheres),
            auto: e.auto.clone(),
        }),
        KindWire::Variant(v) => KindWire::Variant(VariantWire {
            form: v.form,
            discr: v.discr.clone(),
            fields: map_ids(sigma, &v.fields),
        }),
        KindWire::Const(c) => KindWire::Const(ConstWire {
            ty: map_type_ref(sigma, &c.ty),
            value: c.value.clone(),
        }),
        KindWire::Static(s) => KindWire::Static(StaticWire {
            ty: map_type_ref(sigma, &s.ty),
            mutable: s.mutable,
        }),
        KindWire::Reexport(r) => KindWire::Reexport(ReexportWire {
            target: map_stable_ref(sigma, &r.target),
        }),
        KindWire::Param(p) => KindWire::Param(ParamWire {
            name: p.name.clone(),
            ty: map_type_ref(sigma, &p.ty),
        }),
    }
}

fn map_params(sigma: &Sigma, ps: &[ParamWire]) -> Box<[ParamWire]> {
    ps.iter()
        .map(|p| ParamWire {
            name: p.name.clone(),
            ty: map_type_ref(sigma, &p.ty),
        })
        .collect()
}

/// Rewrite every in-payload reference `w → σ(w)` and **re-seal** the payload so
/// its `payload_hash` reflects the durable ids. The symbol metadata (name, vis,
/// docs, attrs, cfg, doc-links) is unchanged — σ only moves references. Doc-link
/// targets are cross-references, not identity-bearing payload refs, and are left
/// as-is (they resolve through the graph tier, not the record seam).
pub fn substitute_and_reseal(payload: &OwnedEntryPayload, sigma: &Sigma) -> OwnedEntryPayload {
    // Fast path: an identity-only σ (nothing reused) leaves the payload
    // byte-identical, so skip the rebuild+reseal entirely.
    if sigma.iter().all(|(w, d)| w == d) {
        return payload.clone();
    }
    let kind = map_kind(sigma, &payload.kind);
    OwnedEntryPayload::sealed(
        payload.symbol.clone(),
        payload.kind_disc,
        kind,
        payload.flags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{EntryPayloadFlags, FnSigFlags, SymbolWire};
    use ir::change::{EcosystemId, PackageLineageId, PackageName};
    use ir::entry::Visibility;
    use ir::kind::KindDiscriminant;

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: Visibility::Public,
            documentation: None,
            source_path: String::new(),
            span_start: 0,
            span_end: 0,
            aliases: vec![],
            deprecation: None,
            doc_links: vec![],
            attrs: vec![],
            cfg: None,
        }
    }

    fn fn_with_param(name: &str, param_ty: IntroId) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([ParamWire {
                    name: Some("x".into()),
                    ty: TypeRefWire::Same(param_ty),
                }]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    #[test]
    fn identity_sigma_is_noop() {
        let p = fn_with_param("f", intro(9));
        let sigma: Sigma = std::iter::once((intro(9), intro(9))).collect();
        assert_eq!(substitute_and_reseal(&p, &sigma), p);
    }

    #[test]
    fn param_type_ref_is_remapped_and_resealed() {
        // f(x: Same(7)) with σ: 7 → 3  ⇒  f(x: Same(3)), payload_hash matches a
        // from-scratch f(x: Same(3)).
        let staged = fn_with_param("f", intro(7));
        let sigma: Sigma = std::iter::once((intro(7), intro(3))).collect();
        let got = substitute_and_reseal(&staged, &sigma);
        let expected = fn_with_param("f", intro(3));
        assert_eq!(
            got, expected,
            "ref rewrite + reseal must equal from-scratch"
        );
        assert_eq!(got.payload_hash, expected.payload_hash);
    }

    #[test]
    fn reexport_target_is_remapped() {
        let pkg = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("p"));
        let mk = |t: IntroId| {
            OwnedEntryPayload::sealed(
                sym("re"),
                KindDiscriminant::Reexport,
                KindWire::Reexport(ReexportWire {
                    target: StableRef::new(pkg.clone(), t),
                }),
                EntryPayloadFlags::default(),
            )
        };
        let sigma: Sigma = std::iter::once((intro(5), intro(2))).collect();
        assert_eq!(substitute_and_reseal(&mk(intro(5)), &sigma), mk(intro(2)));
    }

    #[test]
    fn record_field_ids_remapped() {
        let mk = |fields: Vec<IntroId>| {
            OwnedEntryPayload::sealed(
                sym("R"),
                KindDiscriminant::Record,
                KindWire::Record(RecordWire {
                    form: crate::wire::RecordForm::Struct,
                    fields: fields.into_boxed_slice(),
                    generics: Box::new([]),
                    wheres: Box::new([]),
                    auto: Box::new([]),
                }),
                EntryPayloadFlags::default(),
            )
        };
        let sigma: Sigma = [(intro(10), intro(1)), (intro(11), intro(2))]
            .into_iter()
            .collect();
        assert_eq!(
            substitute_and_reseal(&mk(vec![intro(10), intro(11)]), &sigma),
            mk(vec![intro(1), intro(2)]),
        );
    }
}
