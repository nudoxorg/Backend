//! Lowering of Go value-level declarations: alias, func, const, var.
//!
//! Called from [`super::lower_decl`] in the dispatch layer. These are the
//! non-type top-level declarations.

use std::collections::HashSet;

use nudox_ir::build::*;

use super::{GoId, Result, lower_sig_params_into_lowering, sym_for};
use crate::{oracle, types};

// ── Alias (`type A = B`) ──────────────────────────────────────────────────────

pub(super) fn lower_alias(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let target = decl
        .target
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low, local));
    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    let alias_kind = Alias::builder()
        .maybe_target(target)
        .generics(generics)
        .build();
    low.declare(item_id, Some(parent), sym, alias_kind);
    Ok(())
}

// ── Package-level function ─────────────────────────────────────────────────────

pub(super) fn lower_func(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let (input_refs, output_refs) =
        lower_sig_params_into_lowering(pkg, "", &decl.name, decl.signature.as_ref(), low, local);

    let generics: Vec<GenericParam> = decl
        .type_params
        .iter()
        .map(|tp| types::lower_type_param_decl(tp, low, local))
        .collect();

    let fn_kind = Function::builder()
        .maybe_receiver(None)
        .input_params(input_refs)
        .output_params(output_refs)
        .generics(generics)
        .build();

    low.declare(item_id, Some(parent), sym, fn_kind);
    Ok(())
}

// ── Constant ──────────────────────────────────────────────────────────────────

pub(super) fn lower_const(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let ty = decl
        .r#type
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low, local))
        // `const Foo = 1` writes no type; Go infers one. The source-level fact
        // is that the annotation is absent, which is not the same claim as
        // "this constant accepts any value".
        .unwrap_or(Type::UNANNOTATED);
    let value = if decl.value.is_empty() {
        None
    } else {
        Some(decl.value.clone())
    };

    let const_kind = Const::builder().ty(ty).maybe_value(value).build();
    low.declare(item_id, Some(parent), sym, const_kind);
    Ok(())
}

// ── Variable ──────────────────────────────────────────────────────────────────

pub(super) fn lower_var(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) -> Result<()> {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(&decl.name, &decl.doc, decl.exported, decl.pos.as_ref(), decl.span.as_ref());

    let ty = decl
        .r#type
        .as_ref()
        .map(|t| types::lower_type_with_lowering(t, low, local))
        // `var x = f()` writes no type; Go infers one. See `lower_const`.
        .unwrap_or(Type::UNANNOTATED);

    let static_kind = Static::builder().ty(ty).mutable(true).build();
    low.declare(item_id, Some(parent), sym, static_kind);
    Ok(())
}
