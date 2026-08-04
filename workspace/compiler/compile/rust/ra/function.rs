//! Function / trait-method lowering (receivers, signatures, attrs, type_links).

use ir::{
	function::{Attribute as FnAttribute, Function},
	generics::Generics,
	parameter::{LiteralParameter, Parameter},
	pipeline::{output_parameters_from_type, parameter_link_key},
	protocols::{ReceiverKind, TraitMethod},
	ty::Type,
};
use ra_ap_hir::{Access, Function as HirFunction, HasSource, SelfParam};
use ra_ap_syntax::ast::HasGenericParams;
use rustc_hash::FxHashMap as HashMap;

use super::{
	ctx::{LowerCtx, PathKey, stable_id},
	generics, ty,
};

/// Lower a free or inherent/trait-impl method into the IR function shape.
pub(crate) fn lower_function(ctx: &mut LowerCtx<'_>, f: HirFunction) -> Option<Function> {
	let (receiver, input_parameters) = function_inputs(ctx, f);
	let output_parameters = output_ty(ctx, f).and_then(output_parameters_from_type);
	let attributes = function_attributes(ctx, f);
	let generics = fn_generics(ctx, f);
	let type_links = collect_type_links(input_parameters.as_ref(), output_parameters.as_ref());

	// Free / associated methods list: Static → None (trait methods keep Static).
	let receiver = match receiver {
		Some(ReceiverKind::Static) | None => None,
		other => other,
	};

	Some(Function {
		input_parameters,
		output_parameters,
		attributes,
		generics,
		receiver,
		overloads: None,
		implemented: f.has_body(ctx.db),
		type_links,
		implemented_protocols: None,
		members: None,
	})
}

/// Trait method signature — receiver may be `Static` when there is no `self`.
pub(crate) fn lower_trait_method(ctx: &mut LowerCtx<'_>, f: HirFunction) -> Option<TraitMethod> {
	let (receiver, parameters) = function_inputs(ctx, f);
	// Trait methods always surface a receiver kind (incl. Static).
	let receiver = Some(receiver.unwrap_or(ReceiverKind::Static));

	let return_type = output_ty(ctx, f).map(Box::new);
	let generics = fn_generics(ctx, f);
	let attributes = function_attributes(ctx, f);
	let name = f.name(ctx.db).as_str().to_owned();
	let documentation = super::docs::documentation(ctx, f);

	Some(TraitMethod {
		name,
		parameters,
		return_type,
		generics,
		attributes,
		documentation,
		receiver,
		has_default_implementation: f.has_body(ctx.db),
	})
}

fn function_inputs(
	ctx: &mut LowerCtx<'_>,
	f: HirFunction,
) -> (Option<ReceiverKind>, Option<Vec<Parameter>>) {
	let receiver = receiver_kind(ctx, f);

	// Prefer AST param types (lifetimes / shape); fall back to Infer per param.
	let ast_tys = ast_param_types(ctx, f);
	let params_hir = f.params_without_self(ctx.db);

	let params: Vec<Parameter> = params_hir
		.iter()
		.enumerate()
		.map(|(i, p)| {
			let name = p
				.name(ctx.db)
				.map(|n| n.as_str().to_owned())
				.unwrap_or_else(|| format!("_{i}"));
			let r#type = ast_tys.get(i).cloned().or_else(|| Some(Type::Infer));
			Parameter::Literal(LiteralParameter {
				name,
				r#type,
				attributes: None,
				default_value: None,
				description: None,
			})
		})
		.collect();

	(receiver, empty_to_none(params))
}

fn receiver_kind(ctx: &LowerCtx<'_>, f: HirFunction) -> Option<ReceiverKind> {
	let sp = f.self_param(ctx.db)?;
	Some(map_self_param(ctx, sp))
}

/// `SelfParam::access` for ordinary receivers; AST `ty()` → Arbitrary for typed self.
fn map_self_param(ctx: &LowerCtx<'_>, sp: SelfParam) -> ReceiverKind {
	// `self: Pin<&mut Self>` etc. — explicit type annotation on the self param.
	// Prefer Semantics::source so the node is registered (defensive; no resolve here).
	if let Some(src) = ctx.sema.source(sp).or_else(|| sp.source(ctx.db))
		&& src.value.ty().is_some()
	{
		return ReceiverKind::Arbitrary;
	}
	match sp.access(ctx.db) {
		Access::Shared => ReceiverKind::SharedRef,
		Access::Exclusive => ReceiverKind::MutRef,
		Access::Owned => ReceiverKind::Owned,
	}
}

fn function_attributes(ctx: &LowerCtx<'_>, f: HirFunction) -> Option<Vec<FnAttribute>> {
	let mut attrs = Vec::new();
	if f.is_const(ctx.db) {
		attrs.push(FnAttribute::Const);
	}
	if f.is_async(ctx.db) {
		attrs.push(FnAttribute::Async);
	}
	if f.is_unsafe(ctx.db) {
		attrs.push(FnAttribute::Unsafe);
	}
	if f.is_varargs(ctx.db) {
		attrs.push(FnAttribute::Variadic);
	}
	empty_to_none(attrs)
}

fn fn_generics(ctx: &mut LowerCtx<'_>, f: HirFunction) -> Option<Generics> {
	// Semantics::source registers the parse tree for later resolve_path in bounds.
	let src = ctx.sema.source(f).or_else(|| f.source(ctx.db))?;
	let g = generics::lower_generics(
		ctx,
		src.value.generic_param_list(),
		src.value.where_clause(),
	);
	if g.params.is_empty() && g.constraints.is_empty() {
		None
	} else {
		Some(g)
	}
}

fn output_ty(ctx: &mut LowerCtx<'_>, f: HirFunction) -> Option<Type> {
	let src = ctx.sema.source(f).or_else(|| f.source(ctx.db));
	if let Some(src) = &src
		&& let Some(ret) = src.value.ret_type()
		&& let Some(ty_node) = ret.ty()
	{
		// AST shape + resolve; HIR identity as fallback when resolve panics/fails.
		let hir_ty = f.ret_type(ctx.db);
		return Some(ty::lower_type_prefer_ast(ctx, Some(&ty_node), &hir_ty));
	}
	// No written return type → unit when AST is present; else HIR / Infer.
	if src.is_some() {
		Some(Type::Tuple(vec![]))
	} else {
		Some(ty::lower_hir_type_fallback(ctx, &f.ret_type(ctx.db)))
	}
}

/// AST types for non-self params, in declaration order.
/// Prefer Semantics-sourced AST so path resolution can succeed.
fn ast_param_types(ctx: &mut LowerCtx<'_>, f: HirFunction) -> Vec<Type> {
	let Some(src) = ctx.sema.source(f).or_else(|| f.source(ctx.db)) else {
		// No AST: lower HIR param types (lifetimes erased).
		return f
			.params_without_self(ctx.db)
			.iter()
			.map(|p| ty::lower_hir_type_fallback(ctx, &p.ty()))
			.collect();
	};
	let Some(list) = src.value.param_list() else {
		return Vec::new();
	};
	let hir_params = f.params_without_self(ctx.db);
	list.params()
		.enumerate()
		.map(|(i, p)| match p.ty() {
			Some(t) => {
				if let Some(hp) = hir_params.get(i) {
					ty::lower_type_prefer_ast(ctx, Some(&t), &hp.ty())
				} else {
					ty::lower_ast_type(ctx, &t)
				}
			}
			None => hir_params
				.get(i)
				.map(|hp| ty::lower_hir_type_fallback(ctx, &hp.ty()))
				.unwrap_or(Type::Infer),
		})
		.collect()
}

fn collect_type_links(
	inputs: Option<&Vec<Parameter>>,
	outputs: Option<&Vec<Parameter>>,
) -> Option<HashMap<String, i64>> {
	let mut links = HashMap::default();

	if let Some(params) = inputs {
		for (idx, param) in params.iter().enumerate() {
			if let Parameter::Literal(l) = param
				&& let Some(id) = type_link_id(l.r#type.as_ref())
			{
				links.insert(parameter_link_key("in", idx, params.len(), &l.name), id);
			}
		}
	}
	if let Some(params) = outputs {
		for (idx, param) in params.iter().enumerate() {
			if let Parameter::Literal(l) = param
				&& let Some(id) = type_link_id(l.r#type.as_ref())
			{
				links.insert(parameter_link_key("out", idx, params.len(), &l.name), id);
			}
		}
	}

	if links.is_empty() { None } else { Some(links) }
}

/// Path-hash of a resolved type-reference identifier (stable across runs).
///
/// Walks through wrappers (`&T`, `*mut T`, `[T]`, `[T; N]`, tuples, fn-ptrs)
/// so nested named types still produce links — not only top-level
/// `TypeReference`s.
fn type_link_id(ty: Option<&Type>) -> Option<i64> {
	fn walk(ty: &Type) -> Option<i64> {
		match ty {
			Type::TypeReference(tr) if !tr.identifier.is_empty() => {
				Some(stable_id(&PathKey::from(tr.identifier.as_str())))
			}
			Type::BorrowedRef { r#type, .. }
			| Type::RawPointer { r#type, .. }
			| Type::Slice(r#type) => walk(r#type),
			Type::Array { r#type, .. } => walk(r#type),
			Type::Tuple(ts) => ts.iter().find_map(walk),
			Type::Union(ts) => ts.iter().find_map(walk),
			Type::QualifiedPath(qp) => walk(qp.self_type.as_ref()),
			Type::FunctionPointer(fp) => {
				let inputs = fp
					.inputs
					.as_ref()
					.into_iter()
					.flatten()
					.filter_map(|p| match p {
						Parameter::Literal(l) => l.r#type.as_ref(),
						_ => None,
					});
				let outputs = fp
					.outputs
					.as_ref()
					.into_iter()
					.flatten()
					.filter_map(|p| match p {
						Parameter::Literal(l) => l.r#type.as_ref(),
						_ => None,
					});
				inputs.chain(outputs).find_map(walk)
			}
			_ => None,
		}
	}
	walk(ty?)
}

fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}
