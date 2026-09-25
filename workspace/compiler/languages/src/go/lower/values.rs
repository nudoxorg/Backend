//! Lowering of Go value-level declarations: alias, func, const, var.
//!
//! Called from [`super::lower_decl`] in the dispatch layer. These are the
//! non-type top-level declarations.

use std::collections::HashSet;

use nudox_ir::build::{Alias, Const, Function, GenericParam, Lowering, Static, Type};

use super::{GoId, lower_sig_params_into_lowering, sym_for};
use crate::go::{oracle, types};

// ── Alias (`type A = B`) ──────────────────────────────────────────────────────

pub(super) fn lower_alias(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(
        &decl.name,
        &decl.doc,
        decl.exported,
        decl.pos.as_ref(),
        decl.span.as_ref(),
    );

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
}

// ── Package-level function ─────────────────────────────────────────────────────

pub(super) fn lower_func(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: decl.name.clone(),
    };
    let sym = sym_for(
        &decl.name,
        &decl.doc,
        decl.exported,
        decl.pos.as_ref(),
        decl.span.as_ref(),
    );

    let (input_refs, output_refs) = lower_sig_params_into_lowering(
        pkg,
        "",
        &decl.name,
        None,
        decl.signature.as_ref(),
        low,
        local,
    );

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
}

// ── Constant ──────────────────────────────────────────────────────────────────

pub(super) fn lower_const(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    index: usize,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: super::unbound_name(&decl.name, index),
    };
    let sym = sym_for(
        &decl.name,
        &decl.doc,
        decl.exported,
        decl.pos.as_ref(),
        decl.span.as_ref(),
    );

    let ty = decl.r#type.as_ref().map_or(Type::UNANNOTATED, |t| {
        types::lower_type_with_lowering(t, low, local)
    });
    let value = if decl.value.is_empty() {
        None
    } else {
        Some(decl.value.clone())
    };

    let const_kind = Const::builder()
        .ty(ty.clone())
        .maybe_value(value.map(|source| {
            nudox_ir::build::ConstExpr::builder()
                .ty(ty)
                .source(source)
                .build()
        }))
        .build();
    low.declare(item_id, Some(parent), sym, const_kind);
}

// ── Variable ──────────────────────────────────────────────────────────────────

pub(super) fn lower_var(
    pkg: &oracle::Package,
    decl: &oracle::Decl,
    index: usize,
    parent: GoId,
    low: &mut Lowering<GoId>,
    local: &HashSet<String>,
) {
    let item_id = GoId::Item {
        import_path: pkg.import_path.clone(),
        name: super::unbound_name(&decl.name, index),
    };
    let sym = sym_for(
        &decl.name,
        &decl.doc,
        decl.exported,
        decl.pos.as_ref(),
        decl.span.as_ref(),
    );

    let ty = decl.r#type.as_ref().map_or(Type::UNANNOTATED, |t| {
        types::lower_type_with_lowering(t, low, local)
    });

    let static_kind = Static::builder().ty(ty).mutable(true).build();
    low.declare(item_id, Some(parent), sym, static_kind);
}
