//! Defines the loadable semantic boundary lint library.
//! Registers one small late pass for four independently named laws.
//! Delegates each law to a focused, compiler-fact-based module.
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod hidden_mutability;
mod lifetime_escape;
mod semantic_scalar;
mod unsafe_share_contract;

use rustc_errors::{Diag, DiagCtxtHandle, Diagnostic, EmissionGuarantee, Level};
use rustc_hir::Item;
use rustc_lint::{LateContext, LateLintPass, Lint, LintContext, LintStore};
use rustc_span::Span;

dylint_linting::dylint_library!();

rustc_session::declare_lint! {
    /// Rejects primitive scalar fields in an exported data boundary.
    pub SEMANTIC_SCALAR,
    Deny,
    "exported data boundary uses a primitive scalar rather than a semantic type"
}

rustc_session::declare_lint! {
    /// Rejects standard interior-mutability containers in an exported field.
    pub HIDDEN_MUTABILITY,
    Deny,
    "exported data boundary hides mutable coordination behind a standard container"
}

rustc_session::declare_lint! {
    /// Rejects explicit static reference escapes in an exported API.
    pub LIFETIME_ESCAPE,
    Deny,
    "exported data boundary exposes a static reference escape"
}

rustc_session::declare_lint! {
    /// Rejects manual unsafe `Send` or `Sync` implementations.
    pub UNSAFE_SHARE_CONTRACT,
    Deny,
    "manual unsafe `Send` or `Sync` implementation needs an external proof boundary"
}

/// Visits exported boundaries and routes each node to the law that owns it.
#[derive(Default)]
struct SemanticBoundaryPass;

rustc_session::impl_lint_pass!(SemanticBoundaryPass => [
    SEMANTIC_SCALAR,
    HIDDEN_MUTABILITY,
    LIFETIME_ESCAPE,
    UNSAFE_SHARE_CONTRACT,
]);

impl<'tcx> LateLintPass<'tcx> for SemanticBoundaryPass {
    /// Checks exported item and field facts after type resolution.
    fn check_item(&mut self, context: &LateContext<'tcx>, item: &'tcx Item<'_>) {
        semantic_scalar::check_item(context, item);
        hidden_mutability::check_item(context, item);
        lifetime_escape::check_item(context, item);
        unsafe_share_contract::check_item(context, item);
    }
}

/// Carries one stable, non-allocating lint message into rustc's diagnostic channel.
struct BoundaryDiagnostic {
    message: &'static str,
}

impl<'diagnostic, Guarantee: EmissionGuarantee> Diagnostic<'diagnostic, Guarantee>
    for BoundaryDiagnostic
{
    /// Builds the rustc diagnostic while preserving the compiler-selected lint level.
    fn into_diag(
        self,
        diagnostics: DiagCtxtHandle<'diagnostic>,
        level: Level,
    ) -> Diag<'diagnostic, Guarantee> {
        Diag::new(diagnostics, level, self.message)
    }
}

/// Emits a named lint without allocating a formatting-only diagnostic payload.
pub(crate) fn emit(
    context: &LateContext<'_>,
    lint: &'static Lint,
    span: Span,
    message: &'static str,
) {
    context.emit_span_lint(lint, span, BoundaryDiagnostic { message });
}

/// Registers every declared lint and the single shared late pass with Dylint.
#[expect(clippy::no_mangle_with_rust_abi)]
#[unsafe(no_mangle)]
pub fn register_lints(_session: &rustc_session::Session, lint_store: &mut LintStore) {
    lint_store.register_lints(&[
        SEMANTIC_SCALAR,
        HIDDEN_MUTABILITY,
        LIFETIME_ESCAPE,
        UNSAFE_SHARE_CONTRACT,
    ]);
    lint_store.register_late_lint_pass(Box::new(|_| Box::new(SemanticBoundaryPass)));
}

/// Runs the frozen valid and invalid UI neighbors through Dylint's harness.
#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
