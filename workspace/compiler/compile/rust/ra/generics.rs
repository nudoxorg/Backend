//! Generic params + where clauses (AST-driven; hir where-predicates are private).
//!
//! Lowers `ast::GenericParamList` + `ast::WhereClause` into IR [`Generics`].
//! Matches the rustdoc path's encoding: type/const/lifetime params, then
//! where-preds as `TraitBound` / `LifetimeBound` / `AssociatedTypeBound`.

use ir::{
	generics::{
		ConstExpr, Constraint, Generics, Kind, TraitRef, TypeExpr, Variance,
	},
	parameter::{ConstParam, LifetimeParam, Parameter, TypeParam, TypeParamOrigin},
};
use ra_ap_syntax::{
	AstNode,
	ast::{self, HasGenericArgs, HasName, HasTypeBounds},
};

use super::{
	ctx::LowerCtx,
	ty::{
		lower_ast_type, path_type_to_trait_ref, type_to_type_expr,
	},
};

/// Lower `ast::GenericParamList` + `ast::WhereClause` into IR generics.
///
/// Primary name used by the plan / other modules.
pub(crate) fn lower_ast_generics(
	ctx: &mut LowerCtx<'_>,
	params: Option<ast::GenericParamList>,
	where_clause: Option<ast::WhereClause>,
) -> Generics {
	let mut ir_params = Vec::new();
	let mut constraints = Vec::new();

	if let Some(list) = params {
		for param in list.generic_params() {
			match param {
				ast::GenericParam::TypeParam(tp) => {
					// Skip implicit `Self` (rustdoc `is_synthetic` / hir `is_implicit`).
					// AST param lists rarely include it, but macro expansions might.
					let name = tp.name().map(|n| n.text().to_string());
					if name.as_deref() == Some("Self") {
						continue;
					}

					// Inline bounds `T: Clone` → constraints (rustdoc desugars
					// these into where-predicates; we surface them the same way).
					if let Some(n) = name.as_deref() {
						push_bound_constraints(
							ctx,
							&mut constraints,
							n,
							tp.type_bound_list(),
						);
					}

					let default_type = tp
						.default_type()
						.map(|t| type_to_type_expr(&lower_ast_type(ctx, &t)));

					ir_params.push(Parameter::Type(TypeParam {
						name,
						kind:         Kind::Type,
						variance:     Variance::Invariant,
						default_type,
						params:       None,
						origin:       TypeParamOrigin::Free,
					}));
				}
				ast::GenericParam::ConstParam(cp) => {
					let name = cp
						.name()
						.map(|n| n.text().to_string())
						.unwrap_or_default();
					let ty = cp
						.ty()
						.map(|t| type_to_type_expr(&lower_ast_type(ctx, &t)))
						.unwrap_or(TypeExpr {
							name: String::new(),
							args: Vec::new(),
						});
					// Const default → ConstExpr::Var(text) — same encoding as rustdoc.
					let default_value = cp.default_val().map(|ca| {
						let text = ca
							.expr()
							.map(|e| e.syntax().text().to_string())
							.unwrap_or_default();
						ConstExpr::Var(text)
					});
					ir_params.push(Parameter::Const(ConstParam {
						name,
						r#type: ty,
						default_value,
					}));
				}
				ast::GenericParam::LifetimeParam(lp) => {
					let name = lp
						.lifetime()
						.map(|lt| lt.text().to_string())
						.unwrap_or_default();
					// `'a: 'b` bounds on the lifetime param itself.
					if let Some(bounds) = lp.type_bound_list() {
						for bound in bounds.bounds() {
							if let Some(ast::TypeBoundKind::Lifetime(longer)) = bound.kind() {
								constraints.push(Constraint::LifetimeBound {
									shorter: name.clone(),
									longer:  longer.text().to_string(),
								});
							}
						}
					}
					ir_params.push(Parameter::Lifetime(LifetimeParam {
						name,
						variance: Variance::Invariant,
					}));
				}
			}
		}
	}

	if let Some(wc) = where_clause {
		for pred in wc.predicates() {
			lower_where_pred(ctx, &mut constraints, &pred);
		}
	}

	Generics {
		params: ir_params,
		constraints,
	}
}

/// Alias kept for the scaffold name in this module's stub.
pub(crate) fn lower_generics(
	ctx: &mut LowerCtx<'_>,
	params: Option<ast::GenericParamList>,
	where_clause: Option<ast::WhereClause>,
) -> Generics {
	lower_ast_generics(ctx, params, where_clause)
}

// ── where predicates ─────────────────────────────────────────────────────────

fn lower_where_pred(
	ctx: &mut LowerCtx<'_>,
	out: &mut Vec<Constraint>,
	pred: &ast::WherePred,
) {
	// Lifetime predicate: `'a: 'b + 'c`
	if let Some(lt) = pred.lifetime() {
		let shorter = lt.text().to_string();
		if let Some(bounds) = pred.type_bound_list() {
			for bound in bounds.bounds() {
				if let Some(ast::TypeBoundKind::Lifetime(longer)) = bound.kind() {
					out.push(Constraint::LifetimeBound {
						shorter: shorter.clone(),
						longer:  longer.text().to_string(),
					});
				}
			}
		}
		return;
	}

	// Type predicate: `T: Bound`, `T::Item: Display`, `for<'a> T: Foo<'a>`.
	// Encoded as TraitBound / LifetimeBound (rustdoc BoundPredicate); assoc
	// equalities inside trait args become AssociatedTypeBound in
	// `push_bound_constraints`.
	let Some(ty) = pred.ty() else {
		return;
	};
	let param_name = where_param_name(ctx, &ty);
	push_bound_constraints(ctx, out, &param_name, pred.type_bound_list());
}

fn push_bound_constraints(
	ctx: &mut LowerCtx<'_>,
	out: &mut Vec<Constraint>,
	param: &str,
	list: Option<ast::TypeBoundList>,
) {
	let Some(list) = list else {
		return;
	};
	for bound in list.bounds() {
		match bound.kind() {
			Some(ast::TypeBoundKind::PathType(_for_binder, path_ty)) => {
				// Also pull assoc bindings out of the trait's generic args
				// (`T: Iterator<Item = u8>`) so they surface as AssociatedItem
				// constraints when present — TraitBound still carries the trait.
				let trait_ref = path_type_to_trait_ref(ctx, &path_ty);
				// Surface assoc equalities as AssociatedTypeBound too when the
				// path has `Item = …` args (parity with rustdoc EqPredicate).
				if let Some(path) = path_ty.path() {
					if let Some(seg) = path.segment() {
						if let Some(args) = seg.generic_arg_list() {
							for arg in args.generic_args() {
								if let ast::GenericArg::AssocTypeArg(assoc) = arg {
									let assoc_name = assoc
										.name_ref()
										.map(|n| n.text().to_string())
										.unwrap_or_default();
									if let Some(rhs) = assoc.ty() {
										out.push(Constraint::AssociatedTypeBound {
											param:      param.to_string(),
											assoc_name,
											bound:      type_to_type_expr(
												&lower_ast_type(ctx, &rhs),
											),
										});
									}
								}
							}
						}
					}
				}
				out.push(Constraint::TraitBound {
					param:     param.to_string(),
					trait_ref,
				});
			}
			Some(ast::TypeBoundKind::Lifetime(lt)) => {
				out.push(Constraint::LifetimeBound {
					shorter: param.to_string(),
					longer:  lt.text().to_string(),
				});
			}
			Some(ast::TypeBoundKind::Use(_)) => {
				// Precise capturing: `use<…>` — encode as a named marker trait
				// so the constraint is not silently dropped (matches rustdoc
				// `GenericBound::Use` → TraitRef { name: "Use", … }).
				out.push(Constraint::TraitBound {
					param:     param.to_string(),
					trait_ref: TraitRef {
						name: "Use".into(),
						args: Vec::new(),
					},
				});
			}
			None => {}
		}
	}
}

/// Name used as `Constraint::TraitBound.param` — generic name when possible,
/// otherwise the written type text (rustdoc uses Debug for non-generic).
fn where_param_name(ctx: &mut LowerCtx<'_>, ty: &ast::Type) -> String {
	if let ast::Type::PathType(path_ty) = ty {
		if let Some(path) = path_ty.path() {
			// Single-segment paths are usually generic params (`T`).
			if let Some(name) = path.as_single_name_ref() {
				return name.text().to_string();
			}
			// Multi-segment: keep the written identifier (e.g. `T::Item`).
			let written: String = path
				.segments()
				.filter_map(|s| s.name_ref().map(|n| n.text().to_string()))
				.collect::<Vec<_>>()
				.join("::");
			if !written.is_empty() {
				return written;
			}
		}
	}
	// Fallback: full syntax text (covers refs, tuples, …).
	let _ = ctx;
	ty.syntax().text().to_string()
}
