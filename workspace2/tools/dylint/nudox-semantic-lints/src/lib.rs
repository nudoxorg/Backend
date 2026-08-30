#![feature(rustc_private)]
#![warn(unused_extern_crates)]

dylint_linting::dylint_library!();

extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_session;

use rustc_lint::{LateLintPass, LintStore};
use rustc_session::{Session, declare_lint, impl_lint_pass};

declare_lint! {
    pub NUDOX_ERASED_MAP_ERR,
    Warn,
    "a Result::map_err wildcard directly replaces a source error"
}

declare_lint! {
    pub NUDOX_STRINGLY_STATE_FIELD,
    Warn,
    "a semantic state field is represented by a string slice"
}

declare_lint! {
    pub NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    Warn,
    "a public method only returns an already-public field"
}

impl_lint_pass!(NudoxSemanticLints => [
    NUDOX_ERASED_MAP_ERR,
    NUDOX_STRINGLY_STATE_FIELD,
    NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
]);

#[unsafe(no_mangle)]
pub fn register_lints(_session: &Session, lint_store: &mut LintStore) {
    lint_store.register_lints(&[
        NUDOX_ERASED_MAP_ERR,
        NUDOX_STRINGLY_STATE_FIELD,
        NUDOX_REDUNDANT_PUBLIC_ACCESSOR,
    ]);
    lint_store.register_late_pass(|_| Box::new(NudoxSemanticLints));
}

impl<'tcx> LateLintPass<'tcx> for NudoxSemanticLints {}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}

