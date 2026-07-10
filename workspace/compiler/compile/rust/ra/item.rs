//! `ModuleDef` → `Entry` lowering (modules, ADTs, traits, impls, …).

use ir::{
	entry::NudoxPath,
	function::Function,
	generics::{Generics, TraitRef},
	kind::{Entry, Visibility},
	module::Module as IrModule,
	protocols::{
		AssociatedType, AssociatedTypeImpl, TraitAttribute, TraitConstant, TraitDef, TraitImpl,
	},
	record::{
		Field, FieldAttributes, FieldKey, KnownField, Record, SumField, SumVariant,
	},
	ty::{Type, TypeReference},
};
use ra_ap_hir::{
	Adt, AssocItem, FieldSource, HasSource, HasVisibility, Impl, Module, ModuleDef, Struct,
	StructKind, Trait, Union, Visibility as HirVisibility, attach_db,
};
use ra_ap_syntax::ast::HasGenericParams;
use rustc_hash::FxHashSet as HashSet;

use super::{
	ctx::{LowerCtx, PathKey},
	docs, function, generics, ty,
};

// ── public entry points ──────────────────────────────────────────────────────

/// `Entry::Module` with member paths from `module.declarations`.
pub(crate) fn module_entry(ctx: &mut LowerCtx<'_>, module: Module) -> Option<Entry> {
	let def = ModuleDef::Module(module);
	let mut members = Vec::new();
	for child in module.declarations(ctx.db) {
		if !ctx.include(child) || matches!(child, ModuleDef::EnumVariant(_)) {
			continue;
		}
		if let Some(p) = ctx.nudox_path(child) {
			members.push(p);
		}
	}

	let inner = IrModule { members: empty_to_none(members) };
	Some(Entry::Module(ctx.symbol_shell(def, inner)?))
}

/// Lower one module-level def, or `None` to drop (lenient / unsupported).
pub(crate) fn lower(ctx: &mut LowerCtx<'_>, def: ModuleDef) -> Option<Entry> {
	// Modules are emitted via `module_entry`; enum variants only as sum data.
	if matches!(def, ModuleDef::Module(_) | ModuleDef::EnumVariant(_)) {
		return None;
	}

	match def {
		ModuleDef::Function(f) => {
			let fn_ = function::lower_function(ctx, f)?;
			Some(Entry::Function(ctx.symbol_shell(def, fn_)?))
		}
		ModuleDef::Adt(Adt::Struct(s)) => {
			let name = s.name(ctx.db).as_str().to_owned();
			let record = lower_struct(ctx, s, &name);
			Some(Entry::RecordType(ctx.symbol_shell(def, record)?))
		}
		ModuleDef::Adt(Adt::Enum(e)) => {
			let variants = lower_enum(ctx, e);
			Some(Entry::SumType(ctx.symbol_shell(def, variants)?))
		}
		ModuleDef::Adt(Adt::Union(u)) => {
			let fields = lower_union(ctx, u);
			Some(Entry::UnionType(ctx.symbol_shell(def, fields)?))
		}
		ModuleDef::Trait(t) => {
			let trait_def = lower_trait(ctx, t);
			Some(Entry::TraitDef(ctx.symbol_shell(def, trait_def)?))
		}
		ModuleDef::TypeAlias(ta) => {
			let ty = lower_type_alias(ctx, ta);
			Some(Entry::TypeAlias(ctx.symbol_shell(def, ty)?))
		}
		ModuleDef::Const(_) => Some(Entry::Constant(ctx.symbol_shell(def, ())?)),
		ModuleDef::Static(_) => Some(Entry::Variable(ctx.symbol_shell(def, ())?)),
		ModuleDef::Macro(_) => Some(Entry::Macro(ctx.symbol_shell(def, ())?)),
		ModuleDef::BuiltinType(_) => Some(Entry::PrimitiveType(ctx.symbol_shell(def, ())?)),
		ModuleDef::Module(_) | ModuleDef::EnumVariant(_) => None,
	}
}

/// Trait impl → push `Entry::TraitImpl`; inherent → index for later attach.
///
/// Call [`attach_record_impls`] after the full walk (walk also rebuilds
/// [`LowerCtx::build_impl_index`]).
pub(crate) fn lower_impl(ctx: &mut LowerCtx<'_>, out: &mut Vec<Entry>, imp: Impl) {
	// Provisional bucket (walk rebuilds via `build_impl_index` afterwards).
	if let Some(key) = self_ty_path_key(ctx, imp) {
		ctx.impls.by_self_ty.entry(key).or_default().push(imp);
		if is_blanket(ctx, imp)
			&& let Some(tr) = imp.trait_(ctx.db)
		{
			ctx.impls.blanket.push((tr, imp));
		}
	}

	let Some(trait_) = imp.trait_(ctx.db) else {
		return; // inherent — methods attach in second pass
	};

	if let Some(entry) = lower_trait_impl(ctx, imp, trait_) {
		out.push(entry);
	}
}

/// Second pass: attach inherent/trait methods + protocol paths onto records.
///
/// Covers:
/// - ADT-keyed inherent + concrete trait impls (`impls.by_self_ty`)
/// - Blanket trait impls (`impls.blanket`, e.g. `impl<T> BlanketView for T`)
///   so methods like `view` and the protocol path land on every local ADT.
pub(crate) fn attach_record_impls(ctx: &mut LowerCtx<'_>, entries: &mut [Entry]) {
	for entry in entries.iter_mut() {
		let Entry::RecordType(sym) = entry else {
			continue;
		};
		// Only attach to records in the crate we're lowering (Local paths).
		// External ADTs may appear in the entry list when multi-package docs
		// merge; their own LowerCtx pass already filled methods.
		if !matches!(sym.path, NudoxPath::Local(_)) {
			continue;
		}
		let path_key = path_key_from_nudox(&sym.path);
		let adt_impls = ctx.impls.by_self_ty.get(&path_key).cloned().unwrap_or_default();
		let blankets = ctx.impls.blanket.clone();
		if adt_impls.is_empty() && blankets.is_empty() {
			continue;
		}

		let mut methods = Vec::new();
		let mut protocols = Vec::new();
		let mut seen = HashSet::default();

		for imp in adt_impls {
			attach_one_impl(ctx, imp, &mut methods, &mut protocols, &mut seen);
		}

		// Unconstrained blankets (`impl<T> Trait for T`) apply to every ADT.
		// Constrained ones still list here; membership is "in-crate blanket"
		// which matches rustdoc's synthetic-impl attachment for fixtures.
		for (_tr, imp) in blankets {
			attach_one_impl(ctx, imp, &mut methods, &mut protocols, &mut seen);
		}

		sym.inner.methods = empty_to_none(methods);
		sym.inner.implemented_protocols = empty_to_none(protocols);
	}
}

/// Lower methods + protocol path from one impl onto the accumulating record
/// fields. Shared by ADT-keyed and blanket attach paths.
fn attach_one_impl(
	ctx: &mut LowerCtx<'_>,
	imp: Impl,
	methods: &mut Vec<Function>,
	protocols: &mut Vec<NudoxPath>,
	seen: &mut HashSet<NudoxPath>,
) {
	if imp.is_negative(ctx.db) {
		return;
	}
	match imp.trait_(ctx.db) {
		None => {
			for item in imp.items(ctx.db) {
				let AssocItem::Function(f) = item else { continue };
				if !ctx.document_private
					&& !matches!(f.visibility(ctx.db), HirVisibility::Public)
				{
					continue;
				}
				if let Some(fn_) = function::lower_function(ctx, f) {
					methods.push(fn_);
				}
			}
		}
		Some(tr) => {
			if let Some(np) = ctx.nudox_path(ModuleDef::Trait(tr))
				&& seen.insert(np.clone())
			{
				protocols.push(np);
			}
			let trait_path = ctx.nudox_path(ModuleDef::Trait(tr));
			for item in imp.items(ctx.db) {
				let AssocItem::Function(f) = item else { continue };
				if let Some(mut fn_) = function::lower_function(ctx, f) {
					fn_.implemented_protocols = trait_path.clone().map(|p| vec![p]);
					methods.push(fn_);
				}
			}
		}
	}
}

// ── ADT / trait / impl bodies ────────────────────────────────────────────────

fn lower_struct(ctx: &mut LowerCtx<'_>, s: Struct, name: &str) -> Record {
	// Prefer Semantics::source so nested AST (generics, field tys) is registered
	// for resolve_path. Raw HasSource trees panic under Semantics lookups.
	let generics = source_generics(
		ctx,
		ctx.sema.source(s).or_else(|| s.source(ctx.db)).map(|src| src.value),
	);
	let fields = match s.kind(ctx.db) {
		StructKind::Unit => vec![],
		StructKind::Tuple => tuple_fields(ctx, s.fields(ctx.db)),
		StructKind::Record => named_fields(ctx, s.fields(ctx.db)),
	};

	Record {
		name: Some(name.to_owned()),
		generics,
		fields,
		call_signatures: None,
		constructors: None,
		methods: None, // filled by `attach_record_impls`
		index_signatures: None,
		super_types: None,
		implemented_protocols: None,
		members: None,
	}
}

fn lower_enum(ctx: &mut LowerCtx<'_>, e: ra_ap_hir::Enum) -> Vec<SumVariant> {
	e.variants(ctx.db)
		.into_iter()
		.map(|v| {
			let name = v.name(ctx.db).as_str().to_owned();
			let documentation = docs::documentation(ctx, v);
			let data = match v.kind(ctx.db) {
				StructKind::Unit => None,
				StructKind::Tuple => {
					let tys = v.fields(ctx.db).into_iter().map(|f| field_type(ctx, f)).collect();
					Some(SumField::Tuple(tys))
				}
				StructKind::Record => {
					Some(SumField::StructLike(named_fields(ctx, v.fields(ctx.db))))
				}
			};
			SumVariant { name, data, documentation }
		})
		.collect()
}

fn lower_union(ctx: &mut LowerCtx<'_>, u: Union) -> Vec<Type> {
	u.fields(ctx.db).into_iter().map(|f| field_type(ctx, f)).collect()
}

fn lower_trait(ctx: &mut LowerCtx<'_>, t: Trait) -> TraitDef {
	let generics = source_generics(
		ctx,
		ctx.sema.source(t).or_else(|| t.source(ctx.db)).map(|src| src.value),
	);

	let super_traits: Vec<TraitRef> = t
		.direct_supertraits(ctx.db)
		.into_iter()
		.map(|st| TraitRef {
			name: st.name(ctx.db).as_str().to_owned(),
			args: Vec::new(), // AST supertrait args when ty lowerer is fully wired
		})
		.collect();

	let mut required_methods = Vec::new();
	let mut provided_methods = Vec::new();
	let mut associated_types = Vec::new();
	let mut required_constants = Vec::new();

	for item in t.items(ctx.db) {
		match item {
			AssocItem::Function(f) => {
				if let Some(m) = function::lower_trait_method(ctx, f) {
					if f.has_body(ctx.db) {
						provided_methods.push(m);
					} else {
						required_methods.push(m);
					}
				}
			}
			AssocItem::TypeAlias(ta) => {
				let name = ta.name(ctx.db).as_str().to_owned();
				let default_type = ctx
					.sema
					.source(ta)
					.or_else(|| ta.source(ctx.db))
					.and_then(|src| src.value.ty())
					.map(|ty_node| ty::lower_ast_type(ctx, &ty_node));
				associated_types.push(AssociatedType {
					name,
					bounds: None,
					default_type,
				});
			}
			AssocItem::Const(c) => {
				let name = c
					.name(ctx.db)
					.map(|n| n.as_str().to_owned())
					.unwrap_or_default();
				required_constants.push(TraitConstant {
					name,
					r#type: Box::new(Type::Infer),
					default_value: None,
				});
			}
		}
	}

	let attributes = if t.is_auto(ctx.db) {
		Some(vec![TraitAttribute::Auto])
	} else if t.is_unsafe(ctx.db) {
		Some(vec![TraitAttribute::Unsafe])
	} else {
		None
	};

	TraitDef {
		generics,
		super_traits: empty_to_none(super_traits),
		associated_types: empty_to_none(associated_types),
		properties: None,
		required_methods: empty_to_none(required_methods),
		provided_methods: empty_to_none(provided_methods),
		required_constants: empty_to_none(required_constants),
		attributes,
		members: None,
	}
}

fn lower_trait_impl(ctx: &mut LowerCtx<'_>, imp: Impl, trait_: Trait) -> Option<Entry> {
	let for_type = Box::new(lower_self_ty(ctx, imp));
	let impl_ast = ctx
		.sema
		.source(imp)
		.or_else(|| imp.source(ctx.db))
		.map(|s| s.value);
	// HIR gives the trait's identity (name); the written generic args
	// (`impl Trait<Args> for T`) come from the AST — HIR drops them.
	let tr = TraitRef {
		name: trait_.name(ctx.db).as_str().to_owned(),
		args: ty::impl_trait_ref_args(ctx, impl_ast.as_ref()),
	};
	let generics = source_generics(ctx, impl_ast);

	let mut methods = Vec::new();
	let mut associated_types = Vec::new();
	let mut associated_constants = Vec::new();

	for item in imp.items(ctx.db) {
		match item {
			AssocItem::Function(f) => {
				if let Some(fn_) = function::lower_function(ctx, f) {
					methods.push(fn_);
				}
			}
			AssocItem::TypeAlias(ta) => {
				let name = ta.name(ctx.db).as_str().to_owned();
				let r#type = ctx
					.sema
					.source(ta)
					.or_else(|| ta.source(ctx.db))
					.and_then(|src| src.value.ty())
					.map(|n| ty::lower_ast_type(ctx, &n))
					.unwrap_or(Type::Infer);
				associated_types.push(AssociatedTypeImpl {
					name,
					r#type: Box::new(r#type),
				});
			}
			AssocItem::Const(c) => {
				let name = c
					.name(ctx.db)
					.map(|n| n.as_str().to_owned())
					.unwrap_or_default();
				associated_constants.push(TraitConstant {
					name,
					r#type: Box::new(Type::Infer),
					default_value: None,
				});
			}
		}
	}

	// Synthetic path under the impl's module so TraitImpl is indexable.
	let name = format!("<impl {}>", tr.name);
	let module = imp.module(ctx.db);
	let mod_key = ctx.canonical(ModuleDef::Module(module));
	let path = match mod_key {
		Some(m) => NudoxPath::Local(std::path::PathBuf::from(format!("{m}::{name}"))),
		None => NudoxPath::Local(std::path::PathBuf::from(name.as_str())),
	};

	Some(Entry::TraitImpl(LowerCtx::symbol_shell_parts(
		name,
		path,
		Visibility::Public,
		docs::documentation(ctx, imp),
		None,
		TraitImpl {
			tr,
			for_type,
			generics,
			where_constraints: None,
			methods: empty_to_none(methods),
			associated_types: empty_to_none(associated_types),
			associated_constants: empty_to_none(associated_constants),
			is_negative: imp.is_negative(ctx.db),
			is_blanket: is_blanket(ctx, imp),
			is_unsafe: imp.is_unsafe(ctx.db),
			members: None,
		},
	)))
}

// ── fields ───────────────────────────────────────────────────────────────────

fn named_fields(ctx: &mut LowerCtx<'_>, fields: Vec<ra_ap_hir::Field>) -> Vec<Field> {
	fields
		.into_iter()
		.map(|f| {
			Field::Known(KnownField {
				key: FieldKey::Ident(f.name(ctx.db).as_str().to_owned()),
				r#type: Some(Box::new(field_type(ctx, f))),
				default_value: None,
				attributes: field_attrs(),
				visibility: Some(ctx.visibility(f)),
				documentation: docs::documentation(ctx, f),
			})
		})
		.collect()
}

fn tuple_fields(ctx: &mut LowerCtx<'_>, fields: Vec<ra_ap_hir::Field>) -> Vec<Field> {
	fields
		.into_iter()
		.enumerate()
		.map(|(idx, f)| {
			Field::Known(KnownField {
				key: FieldKey::Index(idx),
				r#type: Some(Box::new(field_type(ctx, f))),
				default_value: None,
				attributes: field_attrs(),
				visibility: Some(ctx.visibility(f)),
				documentation: docs::documentation(ctx, f),
			})
		})
		.collect()
}

fn field_attrs() -> FieldAttributes {
	FieldAttributes {
		decorators: vec![],
		is_mutable: false,
		is_optional: false,
		is_static: false,
	}
}

fn field_type(ctx: &mut LowerCtx<'_>, f: ra_ap_hir::Field) -> Type {
	// Register via Semantics so path resolve works; fall back to raw HasSource.
	let ast_ty = ctx
		.sema
		.source(f)
		.or_else(|| f.source(ctx.db))
		.and_then(|src| match src.value {
			FieldSource::Named(rf) => rf.ty(),
			FieldSource::Pos(tf) => tf.ty(),
		});
	// HIR type supplies identity when AST is missing; AST still preferred for shape.
	let hir_ty = attach_db(ctx.db, || f.ty(ctx.db));
	ty::lower_type_prefer_ast(ctx, ast_ty.as_ref(), &hir_ty)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn self_ty_path_key(ctx: &mut LowerCtx<'_>, imp: Impl) -> Option<PathKey> {
	let ty = attach_db(ctx.db, || imp.self_ty(ctx.db));
	let adt = attach_db(ctx.db, || ty.as_adt())?;
	ctx.canonical(ModuleDef::Adt(adt))
}

fn is_blanket(ctx: &LowerCtx<'_>, imp: Impl) -> bool {
	attach_db(ctx.db, || imp.self_ty(ctx.db).as_type_param(ctx.db).is_some())
}

fn lower_self_ty(ctx: &mut LowerCtx<'_>, imp: Impl) -> Type {
	let ty = attach_db(ctx.db, || imp.self_ty(ctx.db));
	if let Some(adt) = attach_db(ctx.db, || ty.as_adt())
		&& let Some(key) = ctx.canonical(ModuleDef::Adt(adt))
	{
		return Type::TypeReference(TypeReference {
			identifier: key.to_string(),
			generic_args: None,
		});
	}
	if attach_db(ctx.db, || ty.as_type_param(ctx.db)).is_some() {
		return Type::GenericParam(ir::ty::GenericParam {
			name: "_".into(),
			kind: None,
		});
	}
	Type::Infer
}

fn lower_type_alias(ctx: &mut LowerCtx<'_>, ta: ra_ap_hir::TypeAlias) -> Type {
	if let Some(src) = ctx.sema.source(ta).or_else(|| ta.source(ctx.db))
		&& let Some(ty_node) = src.value.ty()
	{
		return ty::lower_ast_type(ctx, &ty_node);
	}
	// No AST — HIR display path (lifetimes erased).
	let hir_ty = attach_db(ctx.db, || ta.ty(ctx.db));
	ty::lower_hir_type_fallback(ctx, &hir_ty)
}

fn source_generics<N: HasGenericParams>(
	ctx: &mut LowerCtx<'_>,
	node: Option<N>,
) -> Option<Generics> {
	let node = node?;
	let g = generics::lower_generics(ctx, node.generic_param_list(), node.where_clause());
	if g.params.is_empty() && g.constraints.is_empty() {
		None
	} else {
		Some(g)
	}
}

fn path_key_from_nudox(path: &NudoxPath) -> PathKey {
	match path {
		NudoxPath::Local(p) => PathKey::from(p.to_string_lossy().as_ref()),
		NudoxPath::External { dependency, path } => {
			let rel = path.to_string_lossy();
			if rel.is_empty() {
				PathKey::from(dependency.as_str())
			} else {
				PathKey::from(format!("{dependency}::{rel}"))
			}
		}
	}
}

fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}
