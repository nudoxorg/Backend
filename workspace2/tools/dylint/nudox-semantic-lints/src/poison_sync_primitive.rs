use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{
    AmbigArg, Expr, ExprKind, Node, Ty, TyKind as HirTyKind,
    def::{DefKind, Res},
};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{Ty as RustType, TyKind as RustTyKind};
use rustc_session::declare_lint;
use rustc_span::def_id::DefId;

declare_lint! {
    /// ### What it does
    ///
    /// Rejects poisoning synchronization primitives in shipping targets. Poisoning couples an
    /// ordinary coordination path to unwind history and encourages erased `PoisonError` handling.
    pub NUDOX_POISON_SYNC_PRIMITIVE,
    Warn,
    "a poisoning synchronization primitive is used in a shipping target"
}

fn forbidden_type(context: &LateContext<'_>, rust_type: RustType<'_>) -> bool {
    let rust_type = rust_type.peel_refs();
    let RustTyKind::Adt(definition, _) = rust_type.kind() else {
        return false;
    };
    forbidden_definition(context, definition.did())
}

/// Match the compiler-resolved definition, rather than the spelling used at a call site. This
/// keeps imports and type aliases covered while namespaced lookalikes (including local `loom`,
/// `tokio`, and `parking_lot` modules) remain outside the law.
fn forbidden_definition(context: &LateContext<'_>, definition: DefId) -> bool {
    matches!(
        context.tcx.def_path_str(definition).as_str(),
        "std::sync::Mutex"
            | "std::sync::RwLock"
            | "std::sync::Condvar"
            | "std::sync::poison::mutex::Mutex"
            | "std::sync::poison::rwlock::RwLock"
            | "std::sync::poison::condvar::Condvar"
            | "loom::sync::mutex::Mutex"
            | "loom::sync::rwlock::RwLock"
            | "loom::sync::condvar::Condvar"
    )
}

fn report(context: &LateContext<'_>, span: rustc_span::Span) {
    if span.in_external_macro(context.sess().source_map()) {
        return;
    }
    span_lint_and_help(
        context,
        NUDOX_POISON_SYNC_PRIMITIVE,
        span,
        "poisoning synchronization is forbidden in shipping targets",
        None,
        "prefer ownership transfer, bounded channels, atomics, or a proved lock-free structure; if a cold lock is genuinely required, use a non-poisoning primitive and document its contention boundary",
    );
}

pub(crate) fn check_ty<'tcx>(context: &LateContext<'tcx>, rust_ty: &'tcx Ty<'tcx, AmbigArg>) {
    let HirTyKind::Path(path) = rust_ty.kind else {
        return;
    };
    // A type-relative associated call (for example `Mutex::new(value)`) contains a HIR type path
    // for its receiver. The call expression below owns the diagnostic so the constructor is
    // reported once rather than once for the receiver and once for the call. Keep this tied to
    // the immediate callee path: a generic argument such as `consume::<Mutex<u8>>()` still needs
    // its own type-use diagnostic.
    let mut parents = context.tcx.hir_parent_iter(rust_ty.hir_id);
    if let Some((_, Node::Expr(path_expression))) = parents.next()
        && matches!(path_expression.kind, ExprKind::Path(_))
        && let Some((_, Node::Expr(call_expression))) = parents.next()
        && let ExprKind::Call(callee, _) = call_expression.kind
        && callee.hir_id == path_expression.hir_id
        && let ExprKind::Path(callee_path) = callee.kind
        && let Res::Def(DefKind::AssocFn, definition) =
            context.qpath_res(&callee_path, callee.hir_id)
        && let Some(owner) = context.tcx.impl_of_assoc(definition)
        && forbidden_type(
            context,
            context
                .tcx
                .type_of(owner)
                .instantiate_identity()
                .skip_norm_wip(),
        )
    {
        return;
    }
    let resolved = context.qpath_res(&path, rust_ty.hir_id);
    let forbidden = match resolved {
        Res::Def(DefKind::Struct, definition) => forbidden_definition(context, definition),
        Res::Def(DefKind::TyAlias, alias) => forbidden_type(
            context,
            context
                .tcx
                .type_of(alias)
                .instantiate_identity()
                .skip_norm_wip(),
        ),
        _ => false,
    };
    if forbidden {
        report(context, rust_ty.span);
    }
}

pub(crate) fn check_expr<'tcx>(context: &LateContext<'tcx>, expression: &'tcx Expr<'_>) {
    let definition = match expression.kind {
        ExprKind::MethodCall(..) => context
            .typeck_results()
            .type_dependent_def_id(expression.hir_id),
        ExprKind::Call(callee, _) => {
            let ExprKind::Path(path) = callee.kind else {
                return;
            };
            match context.qpath_res(&path, callee.hir_id) {
                Res::Def(DefKind::AssocFn, definition) => Some(definition),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(definition) = definition else {
        return;
    };
    let Some(owner) = context.tcx.impl_of_assoc(definition) else {
        return;
    };
    if forbidden_type(
        context,
        context
            .tcx
            .type_of(owner)
            .instantiate_identity()
            .skip_norm_wip(),
    ) {
        report(context, expression.span);
    }
}
