//! Dual-path type lowering: AST shape + Semantics resolution (§4).
//!
//! Shape (lifetimes, mutability, arg order) comes from `ast::Type`.
//! Identity (`TypeReference.identifier`) comes from `Semantics::resolve_path`
//! when available; unresolvable paths fall back to the written path text.
//!
//! Prefer [`lower_ast_type`]. Use [`lower_hir_type_fallback`] only when the
//! AST is missing (macro junk / expansion holes). HIR erases lifetimes.
//!
//! **Semantics nodes:** `resolve_path` / `resolve_type` only accept AST nodes
//! derived from *this* `Semantics` instance (`sema.source` / `sema.parse`).
//! Raw `HasSource::source` trees are unregistered and panic with
//! "Failed to lookup … known nodes: (empty)". All resolve calls are therefore
//! wrapped in [`catch_unwind`] so a bad lookup falls back to the written path
//! instead of aborting the whole item.

use std::panic::{self, AssertUnwindSafe};

use ir::{
	function::Attribute as FnAttribute,
	generics::{ConstExpr, Constraint, GenericArg, Term, TraitRef, TypeExpr},
	parameter::{LiteralParameter, Parameter},
	pipeline::output_parameters_from_type,
	primitives::{Primitive, Width},
	protocols::GenericBound,
	ty::{
		DynTrait, FunctionPointer, GenericParam, PolyTrait, QualifiedPath, Type, TypeReference,
	},
};
use ra_ap_hir::{
	HirDisplay, ModuleDef, Mutability, PathResolution, attach_db,
};
use ra_ap_syntax::{
	AstNode,
	ast::{self, HasGenericArgs, HasTypeBounds, PathSegmentKind},
};
use tracing::debug;

use super::ctx::LowerCtx;

// ── public entry points ──────────────────────────────────────────────────────

/// Lower a written type. AST is the source of shape (lifetimes, mutability);
/// `sema` resolves path segments for `TypeReference` / `NudoxPath` linking.
pub(crate) fn lower_ast_type(ctx: &mut LowerCtx<'_>, node: &ast::Type) -> Type {
	match node {
		ast::Type::PathType(p) => lower_path_type(ctx, p),
		ast::Type::RefType(r) => lower_ref_type(ctx, r),
		ast::Type::PtrType(p) => lower_ptr_type(ctx, p),
		ast::Type::SliceType(s) => {
			let inner = s.ty().map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer);
			Type::Slice(Box::new(inner))
		}
		ast::Type::ArrayType(a) => lower_array_type(ctx, a),
		ast::Type::TupleType(t) => {
			let fields = t.fields().map(|f| lower_ast_type(ctx, &f)).collect();
			Type::Tuple(fields)
		}
		ast::Type::FnPtrType(f) => Type::FunctionPointer(lower_fn_ptr(ctx, f)),
		ast::Type::DynTraitType(d) => lower_dyn_trait(ctx, d),
		ast::Type::ImplTraitType(i) => {
			Type::ImplTrait(lower_generic_bounds(ctx, i.type_bound_list()))
		}
		ast::Type::NeverType(_) => Type::Never,
		ast::Type::InferType(_) => Type::Infer,
		ast::Type::ParenType(p) => p
			.ty()
			.map(|t| lower_ast_type(ctx, &t))
			.unwrap_or(Type::Infer),
		ast::Type::ForType(f) => lower_for_type(ctx, f),
		ast::Type::MacroType(m) => lower_macro_type(ctx, m),
		// Pattern types (`#is(...)`) are nightly-only; peel to the base type.
		ast::Type::PatternType(p) => p
			.ty()
			.map(|t| lower_ast_type(ctx, &t))
			.unwrap_or(Type::Infer),
	}
}

/// Convenience: `Some(ast)` → [`lower_ast_type`]; `None` → [`Type::Infer`].
pub(crate) fn lower_ast_type_opt(ctx: &mut LowerCtx<'_>, node: Option<&ast::Type>) -> Type {
	match node {
		Some(t) => lower_ast_type(ctx, t),
		None => Type::Infer,
	}
}

/// Prefer AST when present; otherwise fall back to HIR (lifetimes erased).
///
/// Typical call site: a `HasSource` node that may expose `ty()` plus a
/// semantic `hir::Type` from `Field::ty` / `Function::ret_type` / etc.
pub(crate) fn lower_type_prefer_ast(
	ctx: &mut LowerCtx<'_>,
	ast_ty: Option<&ast::Type>,
	hir_ty: &ra_ap_hir::Type<'_>,
) -> Type {
	match ast_ty {
		Some(t) => lower_ast_type(ctx, t),
		None => lower_hir_type_fallback(ctx, hir_ty),
	}
}

/// Fallback when only `hir::Type` is available (no useful AST).
///
/// **Limitation:** lifetimes are erased. `as_reference` returns
/// `(Type, Mutability)` with no lifetime; `type_arguments` skips lifetime
/// args. Prefer [`lower_ast_type`] whenever an AST node exists.
pub(crate) fn lower_hir_type_fallback(
	ctx: &mut LowerCtx<'_>,
	ty: &ra_ap_hir::Type<'_>,
) -> Type {
	if ty.is_never() {
		return Type::Never;
	}
	if ty.is_unit() {
		return Type::Tuple(Vec::new());
	}

	if let Some((inner, mutability)) = ty.as_reference() {
		return Type::BorrowedRef {
			// HIR erases the lifetime region.
			lifetime:   None,
			is_mutable: matches!(mutability, Mutability::Mut),
			r#type:     Box::new(lower_hir_type_fallback(ctx, &inner)),
		};
	}

	if let Some((inner, mutability)) = ty.as_raw_ptr() {
		return Type::RawPointer {
			is_mutable: matches!(mutability, Mutability::Mut),
			r#type:     Box::new(lower_hir_type_fallback(ctx, &inner)),
		};
	}

	if let Some(inner) = ty.as_slice() {
		return Type::Slice(Box::new(lower_hir_type_fallback(ctx, &inner)));
	}

	if let Some((inner, length)) = ty.as_array(ctx.db) {
		return Type::Array {
			r#type: Box::new(lower_hir_type_fallback(ctx, &inner)),
			length,
		};
	}

	if let Some(builtin) = ty.as_builtin() {
		let name = builtin.name().as_str().to_string();
		if let Some(prim) = primitive_from_str(&name) {
			return Type::Primitive(prim);
		}
	}

	if let Some(param) = ty.as_type_param(ctx.db) {
		return Type::GenericParam(GenericParam {
			name: param.name(ctx.db).as_str().to_string(),
			kind: None,
		});
	}

	let tuple_fields = ty.tuple_fields(ctx.db);
	if ty.is_tuple() {
		return Type::Tuple(
			tuple_fields
				.iter()
				.map(|f| lower_hir_type_fallback(ctx, f))
				.collect(),
		);
	}

	if let Some(trait_) = ty.as_dyn_trait() {
		let name = trait_.name(ctx.db).as_str().to_string();
		return Type::DynTrait(DynTrait {
			traits:   vec![PolyTrait {
				trait_ref: TraitRef { name, args: Vec::new() },
				lifetimes: Vec::new(),
			}],
			// HIR erases the dyn lifetime.
			lifetime: None,
		});
	}

	if let Some(traits) = ty.as_impl_traits(ctx.db) {
		let bounds = traits
			.map(|t| {
				GenericBound::Trait(TraitRef {
					name: t.name(ctx.db).as_str().to_string(),
					args: Vec::new(),
				})
			})
			.collect::<Vec<_>>();
		return Type::ImplTrait(bounds);
	}

	if let Some(adt) = ty.as_adt() {
		let def = ModuleDef::Adt(adt);
		let identifier = def_path_string(ctx, def).unwrap_or_else(|| {
			adt.name(ctx.db).as_str().to_string()
		});
		// type_arguments skips lifetimes — intentional HIR limitation.
		let args: Vec<GenericArg> = ty
			.type_arguments()
			.map(|arg| GenericArg::Type(lower_hir_type_fallback(ctx, &arg)))
			.collect();
		return Type::TypeReference(TypeReference {
			identifier,
			generic_args: if args.is_empty() { None } else { Some(args) },
		});
	}

	if let Some(callable) = ty.as_callable(ctx.db) {
		if matches!(callable.kind(), ra_ap_hir::CallableKind::FnPtr) {
			let inputs: Vec<Parameter> = callable
				.params()
				.iter()
				.map(|p| {
					Parameter::Literal(LiteralParameter {
						name:          String::new(),
						r#type:        Some(lower_hir_type_fallback(ctx, &p.ty())),
						attributes:    None,
						default_value: None,
						description:   None,
					})
				})
				.collect();
			let output = callable.return_type();
			return Type::FunctionPointer(FunctionPointer {
				inputs:     if inputs.is_empty() { None } else { Some(inputs) },
				outputs:    output_parameters_from_type(lower_hir_type_fallback(ctx, &output)),
				attributes: None,
			});
		}
	}

	// Last resort: rendered display string (no lifetimes in the HIR render either).
	let rendered = attach_db(ctx.db, || ty.display(ctx.db, ctx.display).to_string());
	if rendered == "!" {
		return Type::Never;
	}
	if rendered == "_" {
		return Type::Infer;
	}
	if let Some(prim) = primitive_from_str(&rendered) {
		return Type::Primitive(prim);
	}
	if rendered == "Self" {
		return Type::SelfType;
	}
	Type::TypeReference(TypeReference {
		identifier:   rendered,
		generic_args: None,
	})
}

// ── path types ───────────────────────────────────────────────────────────────

fn lower_path_type(ctx: &mut LowerCtx<'_>, path_ty: &ast::PathType) -> Type {
	let Some(path) = path_ty.path() else {
		return Type::Infer;
	};

	// `<T as Trait>::Assoc` — first segment is a type anchor.
	if let Some(first) = path.segments().next() {
		if let Some(PathSegmentKind::Type { type_ref, trait_ref }) = first.kind() {
			return lower_qualified_path(ctx, &path, type_ref, trait_ref);
		}
	}

	// Bare `Self`.
	if path
		.as_single_name_ref()
		.is_some_and(|n| n.text() == "Self")
	{
		return Type::SelfType;
	}

	let generic_args = last_segment_generic_args(ctx, &path);

	if let Some(res) = resolve_path_opt(ctx, &path) {
		match res {
			PathResolution::SelfType(_) => return Type::SelfType,
			PathResolution::TypeParam(tp) => {
				return Type::GenericParam(GenericParam {
					name: tp.name(ctx.db).as_str().to_string(),
					kind: None,
				});
			}
			PathResolution::Def(ModuleDef::BuiltinType(b)) => {
				let name = b.name().as_str().to_string();
				if let Some(prim) = primitive_from_str(&name) {
					return Type::Primitive(prim);
				}
				// Non-primitive builtins (notably `str`) stay TypeReference so
				// unsized `str` is never collapsed into Primitive::String.
				return Type::TypeReference(TypeReference {
					identifier:   name,
					generic_args: None,
				});
			}
			PathResolution::Def(def) => {
				let identifier = def_path_string(ctx, def)
					.unwrap_or_else(|| path_identifier_text(&path));
				return Type::TypeReference(TypeReference {
					identifier,
					generic_args,
				});
			}
			// Value-ns / attr noise — fall through to written path.
			PathResolution::Local(_)
			| PathResolution::ConstParam(_)
			| PathResolution::BuiltinAttr(_)
			| PathResolution::ToolModule(_)
			| PathResolution::DeriveHelper(_) => {}
		}
	}

	// Unresolvable (or unresolved builtin): written path as last resort.
	let written = path_identifier_text(&path);
	if path.as_single_name_ref().is_some() {
		if let Some(prim) = primitive_from_str(&written) {
			return Type::Primitive(prim);
		}
		if written == "Self" {
			return Type::SelfType;
		}
	}
	Type::TypeReference(TypeReference {
		identifier: written,
		generic_args,
	})
}

fn lower_qualified_path(
	ctx: &mut LowerCtx<'_>,
	path: &ast::Path,
	self_ty: Option<ast::Type>,
	trait_path: Option<ast::PathType>,
) -> Type {
	let self_type = Box::new(self_ty.map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer));

	let tr = trait_path.as_ref().map(|pt| {
		let args = pt
			.path()
			.as_ref()
			.and_then(|p| last_segment_generic_args(ctx, p));
		let identifier = pt
			.path()
			.as_ref()
			.map(|p| {
				resolve_path_opt(ctx, p)
					.and_then(|res| match res {
						PathResolution::Def(def) => def_path_string(ctx, def),
						_ => None,
					})
					.unwrap_or_else(|| path_identifier_text(p))
			})
			.unwrap_or_default();
		TypeReference {
			identifier,
			generic_args: args,
		}
	});

	// Name is the final path segment after the type anchor.
	let mut segs = path.segments();
	let _anchor = segs.next();
	let name = segs
		.next()
		.and_then(|s| s.name_ref())
		.map(|n| n.text().to_string())
		.unwrap_or_default();

	// Generic args on the assoc segment (e.g. `Assoc<T>`).
	let generic_arguments = path
		.segment()
		.and_then(|s| s.generic_arg_list())
		.map(|list| lower_generic_arg_list(ctx, &list))
		.filter(|v| !v.is_empty());

	Type::QualifiedPath(QualifiedPath {
		name,
		generic_arguments,
		self_type,
		tr,
	})
}

// ── ref / ptr / array / fn ───────────────────────────────────────────────────

fn lower_ref_type(ctx: &mut LowerCtx<'_>, r: &ast::RefType) -> Type {
	let lifetime = r.lifetime().map(|lt| lt.text().to_string());
	let is_mutable = r.mut_token().is_some();
	let inner = r.ty().map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer);
	Type::BorrowedRef {
		lifetime,
		is_mutable,
		r#type: Box::new(inner),
	}
}

fn lower_ptr_type(ctx: &mut LowerCtx<'_>, p: &ast::PtrType) -> Type {
	let is_mutable = p.mut_token().is_some();
	let inner = p.ty().map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer);
	Type::RawPointer {
		is_mutable,
		r#type: Box::new(inner),
	}
}

fn lower_array_type(ctx: &mut LowerCtx<'_>, a: &ast::ArrayType) -> Type {
	let inner = a.ty().map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer);
	let length = a
		.const_arg()
		.and_then(|c| c.expr())
		.map(|e| {
			let text = e.syntax().text().to_string().replace('_', "");
			text.parse::<usize>().unwrap_or(0)
		})
		.unwrap_or(0);
	Type::Array {
		r#type: Box::new(inner),
		length,
	}
}

fn lower_fn_ptr(ctx: &mut LowerCtx<'_>, f: &ast::FnPtrType) -> FunctionPointer {
	let mut attributes = Vec::new();
	if f.const_token().is_some() {
		attributes.push(FnAttribute::Const);
	}
	if f.async_token().is_some() {
		attributes.push(FnAttribute::Async);
	}
	if f.unsafe_token().is_some() {
		attributes.push(FnAttribute::Unsafe);
	}

	let inputs = f.param_list().map(|list| {
		list.params()
			.map(|param| {
				let name = param
					.pat()
					.map(|p| p.syntax().text().to_string())
					.unwrap_or_default();
				let ty = param.ty().map(|t| lower_ast_type(ctx, &t));
				Parameter::Literal(LiteralParameter {
					name,
					r#type:        ty,
					attributes:    None,
					default_value: None,
					description:   None,
				})
			})
			.collect::<Vec<_>>()
	});

	let outputs = f
		.ret_type()
		.and_then(|r| r.ty())
		.map(|t| lower_ast_type(ctx, &t))
		.and_then(output_parameters_from_type);

	FunctionPointer {
		inputs,
		outputs,
		attributes: if attributes.is_empty() {
			None
		} else {
			Some(attributes)
		},
	}
}

// ── dyn / impl / for / macro ─────────────────────────────────────────────────

fn lower_dyn_trait(ctx: &mut LowerCtx<'_>, d: &ast::DynTraitType) -> Type {
	let (traits, lifetime) = lower_poly_traits(ctx, d.type_bound_list());
	Type::DynTrait(DynTrait { traits, lifetime })
}

fn lower_for_type(ctx: &mut LowerCtx<'_>, f: &ast::ForType) -> Type {
	let hrtb = for_binder_lifetimes(f.for_binder());
	let Some(inner) = f.ty() else {
		return Type::Infer;
	};
	let mut lowered = lower_ast_type(ctx, &inner);
	if hrtb.is_empty() {
		return lowered;
	}
	// Attach HRTB lifetimes onto dyn polytraits when applicable.
	if let Type::DynTrait(ref mut dt) = lowered {
		for pt in &mut dt.traits {
			if pt.lifetimes.is_empty() {
				pt.lifetimes = hrtb.clone();
			}
		}
	}
	lowered
}

fn lower_macro_type(ctx: &mut LowerCtx<'_>, m: &ast::MacroType) -> Type {
	let Some(call) = m.macro_call() else {
		return Type::Infer;
	};
	// Expand and re-lower the resulting type node, if any.
	// `expand_macro_call` also requires a Semantics-registered AST node.
	let expanded = match panic::catch_unwind(AssertUnwindSafe(|| {
		ctx.sema.expand_macro_call(&call)
	})) {
		Ok(v) => v,
		Err(_) => {
			debug!("sema.expand_macro_call panicked; treating as unexpanded");
			None
		}
	};
	if let Some(expanded) = expanded {
		if let Some(ty) = ast::Type::cast(expanded.value.clone()) {
			return lower_ast_type(ctx, &ty);
		}
		if let Some(ty) = expanded
			.value
			.descendants()
			.find_map(ast::Type::cast)
		{
			return lower_ast_type(ctx, &ty);
		}
	}
	// Semantic resolve as a last try (may still work post-expansion in sema).
	let macro_ty = ast::Type::MacroType(m.clone());
	if let Some(hir_ty) = resolve_type_opt(ctx, &macro_ty) {
		return lower_hir_type_fallback(ctx, &hir_ty);
	}
	Type::Infer
}

// ── bounds / generic args ────────────────────────────────────────────────────

/// Lower a type-bound list into IR [`GenericBound`]s (for `impl Trait`, etc.).
pub(crate) fn lower_generic_bounds(
	ctx: &mut LowerCtx<'_>,
	list: Option<ast::TypeBoundList>,
) -> Vec<GenericBound> {
	let Some(list) = list else {
		return Vec::new();
	};
	let mut out = Vec::new();
	for bound in list.bounds() {
		match bound.kind() {
			Some(ast::TypeBoundKind::PathType(_for_binder, path_ty)) => {
				out.push(GenericBound::Trait(path_type_to_trait_ref(ctx, &path_ty)));
			}
			Some(ast::TypeBoundKind::Lifetime(lt)) => {
				out.push(GenericBound::Lifetime(lt.text().to_string()));
			}
			Some(ast::TypeBoundKind::Use(_)) | None => {}
		}
	}
	out
}

fn lower_poly_traits(
	ctx: &mut LowerCtx<'_>,
	list: Option<ast::TypeBoundList>,
) -> (Vec<PolyTrait>, Option<String>) {
	let mut traits = Vec::new();
	let mut lifetime = None;
	let Some(list) = list else {
		return (traits, lifetime);
	};
	for bound in list.bounds() {
		match bound.kind() {
			Some(ast::TypeBoundKind::PathType(for_binder, path_ty)) => {
				let lifetimes = for_binder_lifetimes(for_binder);
				traits.push(PolyTrait {
					trait_ref: path_type_to_trait_ref(ctx, &path_ty),
					lifetimes,
				});
			}
			Some(ast::TypeBoundKind::Lifetime(lt)) => {
				lifetime = Some(lt.text().to_string());
			}
			Some(ast::TypeBoundKind::Use(_)) | None => {}
		}
	}
	(traits, lifetime)
}

/// Written generic args on an `impl Trait<Args> for T` header, taken from the
/// AST. HIR erases these, so an impl of `From<String>` would otherwise lose the
/// `String` argument on its `TraitRef`. Returns empty when the impl is inherent
/// or the trait ref is not a plain path type.
pub(crate) fn impl_trait_ref_args(
	ctx: &mut LowerCtx<'_>,
	impl_ast: Option<&ast::Impl>,
) -> Vec<TypeExpr> {
	impl_ast
		.and_then(|i| i.trait_())
		.and_then(|t| match t {
			ast::Type::PathType(p) => Some(p),
			_ => None,
		})
		.map(|p| path_type_to_trait_ref(ctx, &p).args)
		.unwrap_or_default()
}

/// Shared by ty + generics: `PathType` → [`TraitRef`].
pub(crate) fn path_type_to_trait_ref(
	ctx: &mut LowerCtx<'_>,
	path_ty: &ast::PathType,
) -> TraitRef {
	let Some(path) = path_ty.path() else {
		return TraitRef {
			name: String::new(),
			args: Vec::new(),
		};
	};

	let name = resolve_path_opt(ctx, &path)
		.and_then(|res| match res {
			PathResolution::Def(def) => def_path_string(ctx, def),
			_ => None,
		})
		.unwrap_or_else(|| path_identifier_text(&path));

	let args = last_segment_generic_args(ctx, &path)
		.unwrap_or_default()
		.into_iter()
		.filter_map(generic_arg_to_type_expr)
		.collect();

	TraitRef { name, args }
}

// ── Semantics resolve (panic-safe) ───────────────────────────────────────────

/// `sema.resolve_path`, catching the "not derived from this Semantics" panic.
///
/// Returns `None` on panic or unresolved path so callers fall back to written text.
fn resolve_path_opt(
	ctx: &LowerCtx<'_>,
	path: &ast::Path,
) -> Option<PathResolution> {
	match panic::catch_unwind(AssertUnwindSafe(|| ctx.sema.resolve_path(path))) {
		Ok(res) => res,
		Err(_) => {
			debug!("sema.resolve_path panicked; using written path fallback");
			None
		}
	}
}

/// `sema.resolve_type`, panic-safe (same Semantics node-registration constraint).
fn resolve_type_opt<'db>(
	ctx: &LowerCtx<'db>,
	ty: &ast::Type,
) -> Option<ra_ap_hir::Type<'db>> {
	match panic::catch_unwind(AssertUnwindSafe(|| ctx.sema.resolve_type(ty))) {
		Ok(res) => res,
		Err(_) => {
			debug!("sema.resolve_type panicked; dropping semantic type");
			None
		}
	}
}

fn last_segment_generic_args(
	ctx: &mut LowerCtx<'_>,
	path: &ast::Path,
) -> Option<Vec<GenericArg>> {
	let seg = path.segment()?;
	// Parenthesized trait sugar: `Fn(i32) -> bool`.
	if let Some(paren) = seg.parenthesized_arg_list() {
		let mut args: Vec<GenericArg> = paren
			.type_args()
			.filter_map(|ta| ta.ty())
			.map(|t| GenericArg::Type(lower_ast_type(ctx, &t)))
			.collect();
		if let Some(ret) = seg.ret_type().and_then(|r| r.ty()) {
			args.push(GenericArg::Type(lower_ast_type(ctx, &ret)));
		}
		return if args.is_empty() { None } else { Some(args) };
	}
	let list = seg.generic_arg_list()?;
	let args = lower_generic_arg_list(ctx, &list);
	if args.is_empty() {
		None
	} else {
		Some(args)
	}
}

/// Lower angle-bracketed generic args (shared with generics module).
pub(crate) fn lower_generic_arg_list(
	ctx: &mut LowerCtx<'_>,
	list: &ast::GenericArgList,
) -> Vec<GenericArg> {
	let mut out = Vec::new();
	for arg in list.generic_args() {
		match arg {
			ast::GenericArg::TypeArg(ta) => {
				let ty = ta.ty().map(|t| lower_ast_type(ctx, &t)).unwrap_or(Type::Infer);
				out.push(GenericArg::Type(ty));
			}
			ast::GenericArg::LifetimeArg(la) => {
				if let Some(lt) = la.lifetime() {
					out.push(GenericArg::Lifetime(lt.text().to_string()));
				}
			}
			ast::GenericArg::ConstArg(ca) => {
				let text = ca
					.expr()
					.map(|e| e.syntax().text().to_string())
					.unwrap_or_default();
				out.push(GenericArg::ConstExpr(ConstExpr::Var(text)));
			}
			ast::GenericArg::AssocTypeArg(assoc) => {
				let name = assoc
					.name_ref()
					.map(|n| n.text().to_string())
					.unwrap_or_default();
				let args = assoc
					.generic_arg_list()
					.map(|l| lower_generic_arg_list(ctx, &l));

				if let Some(ty) = assoc.ty() {
					// `Item = T`
					out.push(GenericArg::Constraint(Constraint::AssociatedItem {
						name,
						args,
						term: Term::Equality(Box::new(lower_ast_type(ctx, &ty))),
					}));
				} else if let Some(bounds) = assoc.type_bound_list() {
					// `Item: Display + Debug`
					let mut inner = Vec::new();
					for bound in bounds.bounds() {
						match bound.kind() {
							Some(ast::TypeBoundKind::PathType(_, path_ty)) => {
								inner.push(Constraint::TraitBound {
									param:     String::new(),
									trait_ref: path_type_to_trait_ref(ctx, &path_ty),
								});
							}
							Some(ast::TypeBoundKind::Lifetime(lt)) => {
								inner.push(Constraint::LifetimeBound {
									shorter: String::new(),
									longer:  lt.text().to_string(),
								});
							}
							_ => {}
						}
					}
					out.push(GenericArg::Constraint(Constraint::AssociatedItem {
						name,
						args,
						term: Term::Bound(inner),
					}));
				}
			}
		}
	}
	out
}

// ── primitives ───────────────────────────────────────────────────────────────

/// Map a fixed-width / arch primitive name to IR [`Primitive`].
///
/// Widths are exact: `i32` → `Int(W32)`, `isize` → `Int(Arch)`, etc.
/// `str` is intentionally **not** mapped — callers leave it as
/// [`Type::TypeReference`] with identifier `"str"` so unsized `str` is not
/// collapsed into owned [`Primitive::String`] (which would make `&str` look
/// like a string primitive).
pub(crate) fn primitive_from_str(prim: &str) -> Option<Primitive> {
	Some(match prim {
		"i8" => Primitive::Int(Width::W8),
		"i16" => Primitive::Int(Width::W16),
		"i32" => Primitive::Int(Width::W32),
		"i64" => Primitive::Int(Width::W64),
		"i128" => Primitive::Int(Width::W128),
		"isize" => Primitive::Int(Width::Arch),
		"u8" => Primitive::UInt(Width::W8),
		"u16" => Primitive::UInt(Width::W16),
		"u32" => Primitive::UInt(Width::W32),
		"u64" => Primitive::UInt(Width::W64),
		"u128" => Primitive::UInt(Width::W128),
		"usize" => Primitive::UInt(Width::Arch),
		"f16" => Primitive::Float(Width::W16),
		"f32" => Primitive::Float(Width::W32),
		"f64" => Primitive::Float(Width::W64),
		"f128" => Primitive::Float(Width::W128),
		"bool" => Primitive::Bool,
		// `str` stays a TypeReference — do not map to Primitive::String.
		"char" => Primitive::Char,
		_ => return None,
	})
}

// ── path / text helpers ──────────────────────────────────────────────────────

/// Canonical def path: prefer `ctx.canonical` once wired; else
/// `ModuleDef::canonical_path`; else `None` so callers can use the written path.
fn def_path_string(ctx: &mut LowerCtx<'_>, def: ModuleDef) -> Option<String> {
	if let Some(key) = ctx.canonical(def) {
		return Some(key.to_string());
	}
	// TODO: once `LowerCtx::canonical` is fully wired, drop this fallback.
	def.canonical_path(ctx.db, ctx.edition)
}

/// Segment-name join of a path, ignoring generic args (rustdoc-style identifier).
fn path_identifier_text(path: &ast::Path) -> String {
	path.segments()
		.filter_map(|seg| match seg.kind() {
			Some(PathSegmentKind::Name(n)) => Some(n.text().to_string()),
			Some(PathSegmentKind::SelfTypeKw) => Some("Self".into()),
			Some(PathSegmentKind::SelfKw) => Some("self".into()),
			Some(PathSegmentKind::SuperKw) => Some("super".into()),
			Some(PathSegmentKind::CrateKw) => Some("crate".into()),
			Some(PathSegmentKind::Type { .. }) | None => None,
		})
		.collect::<Vec<_>>()
		.join("::")
}

fn for_binder_lifetimes(binder: Option<ast::ForBinder>) -> Vec<String> {
	binder
		.and_then(|b| b.generic_param_list())
		.map(|list| {
			list.lifetime_params()
				.filter_map(|lp| lp.lifetime().map(|lt| lt.text().to_string()))
				.collect()
		})
		.unwrap_or_default()
}

/// Convert a [`GenericArg`] into a shallow [`TypeExpr`] for `TraitRef.args`.
pub(crate) fn generic_arg_to_type_expr(arg: GenericArg) -> Option<TypeExpr> {
	match arg {
		GenericArg::Type(ty) => Some(type_to_type_expr(&ty)),
		GenericArg::Lifetime(lt) => Some(TypeExpr {
			name: lt,
			args: Vec::new(),
		}),
		GenericArg::ConstExpr(ce) => Some(TypeExpr {
			name: const_expr_text(&ce),
			args: Vec::new(),
		}),
		// Constraints are not TypeExpr args.
		GenericArg::Constraint(_) | GenericArg::Module(_) => None,
	}
}

/// Shallow type → [`TypeExpr`] used in defaults / trait-ref args.
pub(crate) fn type_to_type_expr(ty: &Type) -> TypeExpr {
	match ty {
		Type::Primitive(p) => TypeExpr {
			name: primitive_name(p).into(),
			args: Vec::new(),
		},
		Type::TypeReference(tr) => TypeExpr {
			name: tr.identifier.clone(),
			args: tr
				.generic_args
				.as_ref()
				.map(|args| {
					args.iter()
						.cloned()
						.filter_map(generic_arg_to_type_expr)
						.collect()
				})
				.unwrap_or_default(),
		},
		Type::GenericParam(gp) => TypeExpr {
			name: gp.name.clone(),
			args: Vec::new(),
		},
		Type::SelfType => TypeExpr {
			name: "Self".into(),
			args: Vec::new(),
		},
		Type::Tuple(ts) if ts.is_empty() => TypeExpr {
			name: "()".into(),
			args: Vec::new(),
		},
		Type::Never => TypeExpr {
			name: "!".into(),
			args: Vec::new(),
		},
		Type::Infer => TypeExpr {
			name: "_".into(),
			args: Vec::new(),
		},
		Type::BorrowedRef {
			lifetime,
			is_mutable,
			r#type,
		} => {
			let mut name = String::from("&");
			if let Some(lt) = lifetime {
				name.push_str(lt);
				name.push(' ');
			}
			if *is_mutable {
				name.push_str("mut ");
			}
			name.push_str(&type_to_type_expr(r#type).name);
			TypeExpr {
				name,
				args: Vec::new(),
			}
		}
		other => TypeExpr {
			// Debug is the same last-resort encoding the rustdoc path uses.
			name: format!("{other:?}"),
			args: Vec::new(),
		},
	}
}

fn const_expr_text(ce: &ConstExpr) -> String {
	match ce {
		ConstExpr::Int(i) => i.to_string(),
		ConstExpr::Float(f) => f.to_string(),
		ConstExpr::Bool(b) => b.to_string(),
		ConstExpr::Str(s) => s.clone(),
		ConstExpr::Var(s) => s.clone(),
		other => format!("{other:?}"),
	}
}

fn primitive_name(p: &Primitive) -> &'static str {
	match p {
		Primitive::Int(Width::W8) => "i8",
		Primitive::Int(Width::W16) => "i16",
		Primitive::Int(Width::W32) => "i32",
		Primitive::Int(Width::W64) => "i64",
		Primitive::Int(Width::W128) => "i128",
		Primitive::Int(Width::Arch) => "isize",
		Primitive::UInt(Width::W8) => "u8",
		Primitive::UInt(Width::W16) => "u16",
		Primitive::UInt(Width::W32) => "u32",
		Primitive::UInt(Width::W64) => "u64",
		Primitive::UInt(Width::W128) => "u128",
		Primitive::UInt(Width::Arch) => "usize",
		Primitive::Float(Width::W8 | Width::W16) => "f16",
		Primitive::Float(Width::W32) => "f32",
		Primitive::Float(Width::W64) => "f64",
		Primitive::Float(Width::W128) => "f128",
		Primitive::Float(Width::Arch) => "f64",
		Primitive::Bool => "bool",
		// Owned string encoding only — bare `str` is a TypeReference, not this.
		Primitive::String => "String",
		Primitive::Char => "char",
		Primitive::Bytes => "bytes",
		Primitive::Date => "date",
		Primitive::Address => "address",
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn primitive_widths_are_exact() {
		assert_eq!(primitive_from_str("i32"), Some(Primitive::Int(Width::W32)));
		assert_eq!(primitive_from_str("isize"), Some(Primitive::Int(Width::Arch)));
		assert_eq!(primitive_from_str("u32"), Some(Primitive::UInt(Width::W32)));
		assert_eq!(primitive_from_str("usize"), Some(Primitive::UInt(Width::Arch)));
		assert_eq!(primitive_from_str("f64"), Some(Primitive::Float(Width::W64)));
		assert_eq!(primitive_from_str("f128"), Some(Primitive::Float(Width::W128)));
	}

	#[test]
	fn str_is_not_a_primitive() {
		assert_eq!(
			primitive_from_str("str"),
			None,
			"str must stay TypeReference, not Primitive::String"
		);
	}
}
