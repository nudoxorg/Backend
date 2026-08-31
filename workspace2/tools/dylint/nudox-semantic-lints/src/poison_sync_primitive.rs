use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{AmbigArg, Expr, ExprKind, Ty, TyKind as HirTyKind, def::DefKind, def::Res};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty::{Ty as RustType, TyKind as RustTyKind};
use rustc_session::declare_lint;

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
    matches!(
        context.tcx.def_path_str(definition.did()).as_str(),
        "std::sync::Mutex"
            | "std::sync::RwLock"
            | "std::sync::Condvar"
            | "std::sync::poison::mutex::Mutex"
            | "std::sync::poison::rwlock::RwLock"
            | "std::sync::poison::condvar::Condvar"
            | "loom::sync::mutex::Mutex"
            | "loom::sync::rwlock::RwLock"
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
    let resolved = context.qpath_res(&path, rust_ty.hir_id);
    let forbidden = match resolved {
        Res::Def(DefKind::Struct, definition) => matches!(
            context.tcx.def_path_str(definition).as_str(),
            "std::sync::Mutex"
                | "std::sync::RwLock"
                | "std::sync::Condvar"
                | "std::sync::poison::mutex::Mutex"
                | "std::sync::poison::rwlock::RwLock"
                | "std::sync::poison::condvar::Condvar"
                | "loom::sync::mutex::Mutex"
                | "loom::sync::rwlock::RwLock"
        ),
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
    let ExprKind::MethodCall(..) = expression.kind else {
        return;
    };
    let Some(definition) = context
        .typeck_results()
        .type_dependent_def_id(expression.hir_id)
    else {
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
