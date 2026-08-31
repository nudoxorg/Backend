use clippy_utils::{diagnostics::span_lint_and_help, peel_blocks, peel_ref_operators};
use rustc_hir::{
    Expr, ExprKind, ImplItem, ImplItemImplKind, ImplItemKind, PatKind, StructTailExpr, def::Res,
};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{self, TyKind};
use rustc_session::declare_lint;
use rustc_span::sym;

declare_lint! {
    /// ### What it does
    ///
    /// Finds crate-visible inherent methods that only return one field from `self`, or public
    /// constructors that merely assemble an independently public record.
    ///
    /// Direct public facts should be fields. When construction must remain sealed, expose a
    /// read-only public view through `Deref` rather than multiplying one-line getters.
    pub NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    Warn,
    "a redundant public accessor or constructor"
}

fn peel_empty_blocks<'a>(expression: &'a Expr<'a>) -> Option<&'a Expr<'a>> {
    let mut expression = expression;
    loop {
        let ExprKind::Block(block, None) = expression.kind else {
            return Some(expression);
        };
        if !block.stmts.is_empty()
            || !matches!(block.rules, rustc_hir::BlockCheckMode::DefaultBlock)
        {
            return None;
        }
        let Some(tail) = block.expr else {
            return None;
        };
        expression = tail;
    }
}

fn transparent_enum_wrapper_source<'tcx>(
    context: &LateContext<'tcx>,
    expression: &Expr<'_>,
) -> Option<rustc_hir::HirId> {
    let ExprKind::Call(callee, [argument]) = expression.kind else {
        return None;
    };
    let ExprKind::Path(path) = callee.kind else {
        return None;
    };
    let variant = match context.qpath_res(&path, callee.hir_id) {
        Res::Def(rustc_hir::def::DefKind::Ctor(..), constructor) => context.tcx.parent(constructor),
        Res::Def(rustc_hir::def::DefKind::Variant, variant) => variant,
        _ => return None,
    };
    if context.tcx.def_kind(variant) != rustc_hir::def::DefKind::Variant {
        return None;
    }
    let ExprKind::Path(argument_path) = argument.kind else {
        return None;
    };
    let Res::Local(parameter) = context.qpath_res(&argument_path, argument.hir_id) else {
        return None;
    };
    Some(parameter)
}

fn check_public_constructor<'tcx>(
    context: &LateContext<'tcx>,
    item: &'tcx ImplItem<'_>,
    body: &'tcx rustc_hir::Body<'_>,
    typeck: &'tcx rustc_middle::ty::TypeckResults<'tcx>,
) -> bool {
    if item.ident.name != sym::new
        || !context.tcx.visibility(item.owner_id.def_id).is_public()
        || body.params.is_empty()
    {
        return false;
    }
    let Some(returned) = peel_empty_blocks(body.value) else {
        return false;
    };
    let ExprKind::Struct(_, fields, StructTailExpr::None) = returned.kind else {
        return false;
    };
    let TyKind::Adt(returned_definition, _) = typeck.expr_ty(returned).kind() else {
        return false;
    };
    let Some(implementation) = context.tcx.impl_of_assoc(item.owner_id.def_id.into()) else {
        return false;
    };
    let implementation_type = context
        .tcx
        .type_of(implementation)
        .instantiate_identity()
        .skip_norm_wip();
    let TyKind::Adt(implementation_definition, _) = implementation_type.kind() else {
        return false;
    };
    if returned_definition.did() != implementation_definition.did() {
        return false;
    }

    let mut parameters = Vec::with_capacity(body.params.len());
    for parameter in body.params {
        let PatKind::Binding(_, parameter_id, parameter_name, _) = parameter.pat.kind else {
            return false;
        };
        parameters.push((parameter_id, parameter_name.name, false));
    }

    for field in fields {
        let field_index = typeck.field_index(field.hir_id);
        let definition = &returned_definition.non_enum_variant().fields[field_index];
        if !context.tcx.visibility(definition.did).is_public() {
            return false;
        }
        let (source, direct) = match field.expr.kind {
            ExprKind::Path(path) => {
                let Res::Local(parameter) = context.qpath_res(&path, field.expr.hir_id) else {
                    return false;
                };
                (Some(parameter), true)
            }
            _ => (transparent_enum_wrapper_source(context, field.expr), false),
        };
        let Some(source) = source else {
            return false;
        };
        let Some((_, parameter_name, used)) =
            parameters.iter_mut().find(|(id, _, _)| *id == source)
        else {
            return false;
        };
        if direct && field.ident.name != *parameter_name {
            return false;
        }
        if *used {
            return false;
        }
        *used = true;
    }

    if parameters.iter().any(|(_, _, used)| !used) {
        return false;
    }
    span_lint_and_help(
        context,
        NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
        item.span,
        "public constructor only assembles independently public fields",
        None,
        "make the public fields the construction API, or reserve the constructor for validation or normalization",
    );
    true
}

pub(crate) fn check<'tcx>(context: &LateContext<'tcx>, item: &'tcx ImplItem<'_>) {
    let associated_item = context.tcx.associated_item(item.owner_id.def_id);
    if item.span.in_external_macro(context.sess().source_map())
        || !matches!(item.impl_kind, ImplItemImplKind::Inherent { .. })
        || (!associated_item.is_method() && item.ident.name != sym::new)
        || !context
            .tcx
            .visibility(item.owner_id.def_id)
            .is_accessible_from(rustc_hir::def_id::CRATE_DEF_ID.to_def_id(), context.tcx)
    {
        return;
    }
    let ImplItemKind::Fn(_, body_id) = item.kind else {
        return;
    };
    let body = context.tcx.hir_body(body_id);
    let typeck = context.tcx.typeck(item.owner_id.def_id);
    if check_public_constructor(context, item, body, typeck) {
        return;
    }
    let [receiver_parameter, parameters @ ..] = body.params else {
        return;
    };
    let PatKind::Binding(_, receiver_id, _, _) = receiver_parameter.pat.kind else {
        return;
    };
    let returned = peel_ref_operators(context, peel_blocks(body.value));

    if let ExprKind::Field(receiver, field_name) = returned.kind {
        let ExprKind::Path(receiver_path) = receiver.kind else {
            return;
        };
        if context.qpath_res(&receiver_path, receiver.hir_id) != Res::Local(receiver_id) {
            return;
        }
        let receiver_type = typeck.expr_ty(receiver).peel_refs();
        let ty::Adt(definition, _) = receiver_type.kind() else {
            return;
        };
        let field_index = typeck.field_index(returned.hir_id);
        let field = &definition.non_enum_variant().fields[field_index];
        span_lint_and_help(
            context,
            NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
            item.span,
            format!(
                "crate-visible accessor only returns the field `{}`",
                field_name.name
            ),
            None,
            if context.tcx.visibility(field.did).is_public() {
                "delete the accessor and use direct field access"
            } else {
                "use a public field when it is independently mutable, or seal construction behind an immutable Deref view when fields form one validated invariant"
            },
        );
        return;
    }

    let ExprKind::MethodCall(_, delegated_receiver, arguments, _) = returned.kind else {
        return;
    };
    let ExprKind::Field(inner_receiver, _) = delegated_receiver.kind else {
        return;
    };
    let ExprKind::Path(inner_receiver_path) = inner_receiver.kind else {
        return;
    };
    if context.qpath_res(&inner_receiver_path, inner_receiver.hir_id) != Res::Local(receiver_id)
        || arguments.len() != parameters.len()
    {
        return;
    }
    for (argument, parameter) in arguments.iter().zip(parameters) {
        let PatKind::Binding(_, parameter_id, _, _) = parameter.pat.kind else {
            return;
        };
        let ExprKind::Path(argument_path) = argument.kind else {
            return;
        };
        if context.qpath_res(&argument_path, argument.hir_id) != Res::Local(parameter_id) {
            return;
        }
    }
    span_lint_and_help(
        context,
        NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
        item.span,
        "crate-visible accessor only delegates to an inner method",
        None,
        "expose the inner field directly, or use a public Deref or trait view instead of forwarding each method",
    );
}
