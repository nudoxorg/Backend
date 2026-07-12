//! Syntactic expression inference (OXC-PLAN §1.2) — the closed table deno_doc's
//! `infer_ts_type_from_expr` implemented, ported onto `oxc_ast::Expression`.
//!
//! LEAF FILE — fill the `todo!()` bodies. This is a total, closed dispatch over
//! the inference-relevant `Expression` variants; unhandled expressions return
//! `None` (no inference), matching deno_doc.

use oxc_ast::ast::{Expression, Function};

use ir::ty::Type;

use super::Extractor;

impl<'a> Extractor<'a> {
	/// Infer a type from an initializer/default expression (OXC-PLAN §1.2).
	///
	/// `is_const` widens literal handling (`const x = 1` → literal `1` type vs
	/// `number`; `x as const` re-infers with `is_const = true`). Returns `None`
	/// for calls / bare identifiers / member exprs (no syntactic inference).
	pub(crate) fn infer_type_from_expr(
		&mut self,
		expr: &Expression<'a>,
		is_const: bool,
	) -> Option<Type> {
		let _ = (expr, is_const);
		todo!("infer.rs: Expression → inferred Type (§1.2)")
	}

	/// Return-type fallback: a function with no annotation and no `return <expr>`
	/// anywhere in its body infers `void` (sync) / `Promise<void>` (async).
	/// Recursively walks body statements.
	pub(crate) fn infer_return_type_fallback(&self, func: &Function<'a>) -> Option<Type> {
		let _ = func;
		todo!("infer.rs: return-type void/Promise<void> fallback")
	}
}
