//! Lowering of Go type declarations: struct, interface, newtype, iota-enum.
//!
//! Called from [`super::lower_type_decl`] in the dispatch layer. Each function
//! creates the type entry and delegates to [`super::lower_methods`] for
//! associated methods.

use std::collections::HashSet;

use nudox_ir::build::*;

use super::{GoId, Result, lower_methods, lower_sig_params_into_lowering, sym_for};
use crate::go::{oracle, types};

// ── Struct ────────────────────────────────────────────────────────────────────

pub(super) fn lower_struct(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    // Collect field Refs (forward-refer them — declare will happen below).
    let underlying = decl.underlying.as_ref();
    let fields_slice = underlying.map(|u| u.fields.as_ref()).unwrap_or(&[]);

    let mut field_refs: Vec<Ref<Field>> = Vec::with_capacity(fields_slice.len());
    for f in fields_slice {
        let fid = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            member_name: f.name.clone(),
        };
        field_refs.push(low.refer(fid));
    }

    // Generics.
    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    // Item 3: Populate super_types from the oracle's `implements` list.
    let super_types: Vec<Type> = decl
        .implements
        .iter()
        .map(|iface_ty| types::lower_type_with_lowering(iface_ty, low, local))
        .collect();

    let record = Record::builder()
        .form(RecordForm::Struct)
        .fields(field_refs)
        .generics(generics)
        .super_types(super_types)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, record);

    // Declare fields as children of the struct.
    for f in fields_slice {
        let fid = GoId::Member {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            member_name: f.name.clone(),
        };
        let fdoc = decl
            .field_docs
            .get(&f.name)
            .map(|s| s.as_str())
            .unwrap_or("");
        let mut fsym = sym_for(&f.name, fdoc, f.exported, None, None);

        // Item 5: Struct field tags and embedded markers.
        let mut attrs: Vec<AttrTok> = Vec::new();
        if f.embedded {
            attrs.push(AttrTok {
                token: "embedded".to_string(),
                arg: None,
            });
        }
        if !f.tag.is_empty() {
            attrs.push(AttrTok {
                token: "tag".to_string(),
                arg: Some(f.tag.clone()),
            });
        }
        fsym.attrs = attrs.into_boxed_slice();

        let mut field_attrs = vec![FieldAttribute::Mutable];
        if f.embedded {
            let _ = &mut field_attrs;
        }

        let fty = f
            .r#type
            .as_ref()
            .map(|t| types::lower_type_with_lowering(t, low, local));

        let field_kind = Field::builder()
            .key(FieldKey::Named)
            .maybe_ty(fty)
            .attributes(field_attrs)
            .build();

        low.declare(fid, Some(item_id.clone()), fsym, field_kind);
    }

    lower_methods(pkg, decl, &item_id, low, local)
}

// ── Interface ─────────────────────────────────────────────────────────────────

pub(super) fn lower_interface(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let mut sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    // Item 7a: surface IsComparable as an AttrTok.
    let is_comparable = decl
        .underlying
        .as_ref()
        .map(|u| u.is_comparable)
        .unwrap_or(false);
    if is_comparable {
        sym.attrs = Box::new([AttrTok {
            token: "comparable".to_string(),
            arg: None,
        }]);
    }

    // Item 7b: surface constraint type-set terms from `embeddeds`.
    let supers: Vec<Type> = decl
        .underlying
        .as_ref()
        .map(|u| {
            u.embeddeds
                .iter()
                .map(|emb| types::lower_type_with_lowering(emb, low, local))
                .collect()
        })
        .unwrap_or_default();

    let trait_kind = Trait::builder()
        .flags(TraitFlags::default())
        .generics(generics)
        .supers(supers)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, trait_kind);

    // Interface methods become Function children of the Trait entry.
    let underlying = decl.underlying.as_ref();
    if let Some(iface) = underlying {
        for sig in iface.all_methods.iter() {
            let mid = GoId::Member {
                import_path: pkg.import_path.clone(),
                type_name: decl.name.clone(),
                member_name: sig.name.clone(),
            };
            let mdoc = decl
                .method_docs
                .get(&sig.name)
                .map(|s| s.as_str())
                .unwrap_or(&sig.pkg);
            let actual_doc = if !sig.pkg.is_empty() && sig.pkg != pkg.import_path {
                format!(
                    "{mdoc}\n\nInherited via embedded interface (declared in `{}`).",
                    sig.pkg
                )
            } else {
                mdoc.to_owned()
            };

            let msym = sym_for(&sig.name, &actual_doc, sig.exported, sig.pos.as_ref(), None);

            let (input_refs, output_refs) = lower_sig_params_into_lowering(
                pkg,
                &decl.name,
                &sig.name,
                sig.signature.as_ref(),
                low,
                local,
            );

            let fn_kind = Function::builder()
                .maybe_receiver(None)
                .input_params(input_refs)
                .output_params(output_refs)
                .is_defaulted(false)
                .build();

            low.declare(mid, Some(item_id.clone()), msym, fn_kind);
        }
    }

    Ok(())
}

// ── Newtype ───────────────────────────────────────────────────────────────────

pub(super) fn lower_newtype(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let inner_id = GoId::Member {
        import_path: pkg.import_path.clone(),
        type_name: decl.name.clone(),
        member_name: "(inner)".to_string(),
    };
    let inner_ref: Ref<Field> = low.refer(inner_id.clone());

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    let record = Record::builder()
        .form(RecordForm::Tuple)
        .fields([inner_ref])
        .generics(generics)
        .build();

    low.declare(item_id.clone(), Some(parent), sym, record);

    let underlying_ty = decl
        .underlying
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low, local));
    let inner_sym = sym_for("(inner)", "(underlying type field)", decl.exported, None, None);
    let inner_field = Field::builder()
        .key(FieldKey::Positional(0))
        .maybe_ty(underlying_ty)
        .build();
    low.declare(inner_id, Some(item_id.clone()), inner_sym, inner_field);

    lower_methods(pkg, decl, &item_id, low, local)
}

// ── Iota enum ─────────────────────────────────────────────────────────────────

pub(super) fn lower_iota_enum(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    variants: &[&oracle::Decl],
    item_id: GoId,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let mut variant_refs: Vec<Ref<Variant>> = Vec::with_capacity(variants.len());
    for v in variants.iter() {
        let vid = GoId::Variant {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            variant_name: v.name.clone(),
        };
        variant_refs.push(low.refer(vid));
    }

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    let enum_kind = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .build();
    low.declare(item_id.clone(), Some(parent), sym, enum_kind);

    for v in variants.iter() {
        let vid = GoId::Variant {
            import_path: pkg.import_path.clone(),
            type_name: decl.name.clone(),
            variant_name: v.name.clone(),
        };
        let vsym = sym_for(&v.name, &v.doc, v.exported, v.pos.as_ref(), v.span.as_ref());
        let discr = if v.value.is_empty() {
            None
        } else {
            Some(v.value.clone())
        };
        let variant_kind = Variant::builder()
            .form(VariantForm::Unit)
            .maybe_discr(discr)
            .build();
        low.declare(vid, Some(item_id.clone()), vsym, variant_kind);
    }

    lower_methods(pkg, decl, &item_id, low, local)
}
