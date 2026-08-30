#![feature(rustc_private)]
#![warn(unused_extern_crates)]

dylint_linting::dylint_library!();

extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod dynamic_dispatch;
mod erased_map_err;
mod redundant_public_accessor;
mod stringly_state_field;

use rustc_hir::{AmbigArg, Expr, FieldDef, ImplItem, Ty};
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_session::{Session, impl_lint_pass};

impl_lint_pass!(NudoxSemanticLints => [
    erased_map_err::NUDOX_ERASED_MAP_ERR,
    stringly_state_field::NUDOX_STRINGLY_STATE_FIELD,
    redundant_public_accessor::NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    dynamic_dispatch::NUDOX_DYNAMIC_DISPATCH,
]);

struct NudoxSemanticLints;

#[unsafe(no_mangle)]
pub fn register_lints(_session: &Session, lint_store: &mut LintStore) {
    lint_store.register_lints(&[
        erased_map_err::NUDOX_ERASED_MAP_ERR,
        stringly_state_field::NUDOX_STRINGLY_STATE_FIELD,
        redundant_public_accessor::NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
        dynamic_dispatch::NUDOX_DYNAMIC_DISPATCH,
    ]);
    lint_store.register_late_pass(|_| Box::new(NudoxSemanticLints));
}

impl<'tcx> LateLintPass<'tcx> for NudoxSemanticLints {
    fn check_expr(&mut self, context: &LateContext<'tcx>, expression: &'tcx Expr<'_>) {
        erased_map_err::check(context, expression);
    }

    fn check_field_def(&mut self, context: &LateContext<'tcx>, field: &'tcx FieldDef<'_>) {
        stringly_state_field::check(context, field);
    }

    fn check_impl_item(&mut self, context: &LateContext<'tcx>, item: &'tcx ImplItem<'_>) {
        redundant_public_accessor::check(context, item);
    }

    fn check_ty(&mut self, context: &LateContext<'tcx>, ty: &'tcx Ty<'tcx, AmbigArg>) {
        dynamic_dispatch::check(context, ty);
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
