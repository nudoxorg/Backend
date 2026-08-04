//! `ModuleDef` → `Entry` lowering (modules, ADTs, traits, impls, …).
//!
//! ## Method dual-emit
//!
//! Inherent / trait methods are kept nested under `Record.methods` for shape,
//! **and** emitted as indexable `Entry::Function` at `AdtPath::method` so
//! SymbolTable / treesitter fq paths (`crate::Counter::new`) resolve. Trait
//! assoc items are similarly dual-emitted under `Trait::item`. Enum / union
//! methods get the index path even when the ADT kind has no `methods` field.

use ir::{
	entry::NudoxPath,
	function::Function,
	generics::{ConstExpr, Generics, TraitRef},
	kind::{Entry, TypeAliasBody, TypedBinding, Visibility},
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
use ra_ap_syntax::{
	AstNode,
	ast::{self, HasArgList, HasGenericParams, HasTypeBounds},
};
use rustc_hash::FxHashSet as HashSet;

use super::{
	ctx::{LowerCtx, PathKey},
	docs, function, generics, source, ty,
};

// ── public entry points ──────────────────────────────────────────────────────

/// `Entry::Module` with member paths from declarations **and** re-exports.
pub(crate) fn module_entry(ctx: &mut LowerCtx<'_>, module: Module) -> Option<Entry> {
	let def = ModuleDef::Module(module);
	let mut members = Vec::new();
	let mut seen = HashSet::default();

	// Declared items first.
	for child in module.declarations(ctx.db) {
		if !ctx.include(child) || matches!(child, ModuleDef::EnumVariant(_)) {
			continue;
		}
		if let Some(p) = ctx.nudox_path(child)
			&& seen.insert(path_key_from_nudox(&p))
		{
			members.push(p);
		}
	}

	// Re-exports (`pub use`, globs) surface in `Module::scope` but not always
	// in `declarations`. Index their primary (or alias) path so `pub use`
	// targets remain resolvable as module members.
	for (name, scope_def) in module.scope(ctx.db, None) {
		let ra_ap_hir::ScopeDef::ModuleDef(child) = scope_def else {
			continue;
		};
		if !ctx.include(child) || matches!(child, ModuleDef::EnumVariant(_)) {
			continue;
		}
		// Prefer the scope spelling (re-export path) as a member when it
		// differs from the defining path; always ensure the def is listed.
		if let Some(module_segs) = ctx_path_segments_for_module(ctx, module) {
			let mut alias_segs = module_segs;
			alias_segs.push(name.as_str().to_owned());
			let alias_key = PathKey::from(alias_segs.join("::"));
			if seen.insert(alias_key.clone()) {
				// Local re-export path under this module.
				members.push(NudoxPath::Local(std::path::PathBuf::from(
					alias_key.as_str(),
				)));
				continue;
			}
		}
		if let Some(p) = ctx.nudox_path(child)
			&& seen.insert(path_key_from_nudox(&p))
		{
			members.push(p);
		}
	}

	let inner = IrModule {
		members: empty_to_none(members),
	};
	Some(Entry::Module(ctx.symbol_shell(def, inner)?))
}

/// Lower one module-level def, or `None` to drop (lenient / unsupported).
///
/// Trait assoc items dual-emitted into `ctx.deferred` under `Trait::item`.
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
			Some(Entry::SumType(ctx.symbol_shell(
				def,
				ir::record::SumType::from_variants(variants),
			)?))
		}
		ModuleDef::Adt(Adt::Union(u)) => {
			let fields = lower_union(ctx, u);
			Some(Entry::UnionType(ctx.symbol_shell(def, fields)?))
		}
		ModuleDef::Trait(t) => {
			let (trait_def, member_paths) = lower_trait(ctx, t);
			let mut sym = ctx.symbol_shell(def, trait_def)?;
			sym.inner.members = empty_to_none(member_paths);
			Some(Entry::TraitDef(sym))
		}
		ModuleDef::TypeAlias(ta) => {
			let body = lower_type_alias(ctx, ta);
			Some(Entry::TypeAlias(ctx.symbol_shell(def, body)?))
		}
		ModuleDef::Const(c) => {
			let binding = lower_const_binding(ctx, c);
			Some(Entry::Constant(ctx.symbol_shell(def, binding)?))
		}
		ModuleDef::Static(s) => {
			let binding = lower_static_binding(ctx, s);
			Some(Entry::Variable(ctx.symbol_shell(def, binding)?))
		}
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

/// Second pass: attach inherent/trait methods + protocol paths onto records
/// and dual-emit indexable method paths for **all** ADTs (structs, enums,
/// unions). Also fills `Record.members` with those paths.
///
/// Covers:
/// - ADT-keyed inherent + concrete trait impls (`impls.by_self_ty`)
/// - Blanket trait impls (`impls.blanket`, e.g. `impl<T> BlanketView for T`)
///   so methods like `view` and the protocol path land on every local ADT.
pub(crate) fn attach_record_impls(ctx: &mut LowerCtx<'_>, entries: &mut Vec<Entry>) {
	// Snapshot ADT entries we can attach to (path → kind).
	let mut adt_slots: Vec<(usize, PathKey, AdtKind)> = Vec::new();
	for (idx, entry) in entries.iter().enumerate() {
		let (path, kind) = match entry {
			Entry::RecordType(sym) if matches!(sym.path, NudoxPath::Local(_)) => {
				(sym.path.clone(), AdtKind::Record)
			}
			Entry::SumType(sym) if matches!(sym.path, NudoxPath::Local(_)) => {
				(sym.path.clone(), AdtKind::Sum)
			}
			Entry::UnionType(sym) if matches!(sym.path, NudoxPath::Local(_)) => {
				(sym.path.clone(), AdtKind::Union)
			}
			_ => continue,
		};
		adt_slots.push((idx, path_key_from_nudox(&path), kind));
	}

	for (idx, path_key, kind) in adt_slots {
		let adt_impls = ctx
			.impls
			.by_self_ty
			.get(&path_key)
			.cloned()
			.unwrap_or_default();
		let blankets = ctx.impls.blanket.clone();
		if adt_impls.is_empty() && blankets.is_empty() {
			continue;
		}

		let mut methods = Vec::new();
		let mut protocols = Vec::new();
		let mut member_paths = Vec::new();
		let mut seen_protocol = HashSet::default();
		let mut seen_method = HashSet::default();

		for imp in adt_impls {
			attach_one_impl(
				ctx,
				imp,
				&path_key,
				&mut methods,
				&mut protocols,
				&mut member_paths,
				&mut seen_protocol,
				&mut seen_method,
			);
		}
		for (_tr, imp) in blankets {
			attach_one_impl(
				ctx,
				imp,
				&path_key,
				&mut methods,
				&mut protocols,
				&mut member_paths,
				&mut seen_protocol,
				&mut seen_method,
			);
		}

		// Shape: only RecordType has methods / implemented_protocols fields.
		if let Some(entry) = entries.get_mut(idx) {
			match (kind, entry) {
				(AdtKind::Record, Entry::RecordType(sym)) => {
					sym.inner.methods = empty_to_none(methods);
					sym.inner.implemented_protocols = empty_to_none(protocols);
					// Merge dual-emitted method paths into members.
					if !member_paths.is_empty() {
						let mut members = sym.inner.members.take().unwrap_or_default();
						for p in member_paths {
							if !members.iter().any(|m| m == &p) {
								members.push(p);
							}
						}
						sym.inner.members = empty_to_none(members);
					}
				}
				(AdtKind::Sum, Entry::SumType(sym)) => {
					sym.inner.methods = empty_to_none(methods);
					sym.inner.implemented_protocols = empty_to_none(protocols);
					if !member_paths.is_empty() {
						let mut members = sym.inner.members.take().unwrap_or_default();
						for p in member_paths {
							if !members.iter().any(|m| m == &p) {
								members.push(p);
							}
						}
						sym.inner.members = empty_to_none(members);
					}
				}
				// Unions: index dual-emit only (no shape methods field yet).
				_ => {}
			}
		}
	}
}

#[derive(Clone, Copy)]
enum AdtKind {
	Record,
	Sum,
	Union,
}

/// Lower methods + protocol path from one impl; dual-emit index Function paths
/// under `adt_path::method`. Shared by ADT-keyed and blanket attach paths.
fn attach_one_impl(
	ctx: &mut LowerCtx<'_>,
	imp: Impl,
	adt_path: &PathKey,
	methods: &mut Vec<Function>,
	protocols: &mut Vec<NudoxPath>,
	member_paths: &mut Vec<NudoxPath>,
	seen_protocol: &mut HashSet<NudoxPath>,
	seen_method: &mut HashSet<PathKey>,
) {
	if imp.is_negative(ctx.db) {
		return;
	}
	let trait_path = match imp.trait_(ctx.db) {
		None => None,
		Some(tr) => {
			if let Some(np) = ctx.nudox_path(ModuleDef::Trait(tr))
				&& seen_protocol.insert(np.clone())
			{
				protocols.push(np);
			}
			ctx.nudox_path(ModuleDef::Trait(tr))
		}
	};

	for item in imp.items(ctx.db) {
		let AssocItem::Function(f) = item else {
			continue;
		};
		// Inherent: honour private gate; trait methods always surface.
		if trait_path.is_none()
			&& !ctx.document_private
			&& !matches!(f.visibility(ctx.db), HirVisibility::Public)
		{
			continue;
		}
		let Some(mut fn_) = function::lower_function(ctx, f) else {
			continue;
		};
		if let Some(ref tp) = trait_path {
			fn_.implemented_protocols = Some(vec![tp.clone()]);
		}
		methods.push(fn_.clone());

		// Dual-emit: indexable Entry::Function at AdtPath::method.
		let method_name = f.name(ctx.db).as_str().to_owned();
		let method_key = PathKey::from(format!("{adt_path}::{method_name}"));
		if !seen_method.insert(method_key.clone()) {
			continue; // already dual-emitted (e.g. inherent + trait collision)
		}
		let path = NudoxPath::Local(std::path::PathBuf::from(method_key.as_str()));
		let visibility = ctx.visibility(f);
		let documentation = docs::documentation(ctx, f);
		let deprecation = docs::deprecation(ctx, ModuleDef::Function(f));
		member_paths.push(path.clone());
		ctx.deferred.push(Entry::Function(ir::kind::Symbol {
			name: method_name,
			path,
			aliases: None,
			visibility,
			documentation,
			deprecation,
			doc_links: None,
			inner: fn_,
		}));
		// Source map under the dual-emitted fq path.
		if let Some((_key, text)) = source::fn_source(ctx, f) {
			ctx.deferred_sources
				.push((method_key.to_string(), text));
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
			SumVariant {
				name,
				data,
				documentation,
			discriminant: None,
			}
		})
		.collect()
}

fn lower_union(ctx: &mut LowerCtx<'_>, u: Union) -> Vec<Type> {
	u.fields(ctx.db).into_iter().map(|f| field_type(ctx, f)).collect()
}

/// Lower a trait and dual-emit indexable assoc items under `Trait::item`.
///
/// Returns `(TraitDef, member paths of dual-emitted entries)`.
fn lower_trait(ctx: &mut LowerCtx<'_>, t: Trait) -> (TraitDef, Vec<NudoxPath>) {
	let trait_ast = ctx
		.sema
		.source(t)
		.or_else(|| t.source(ctx.db))
		.map(|src| src.value);
	let generics = source_generics(ctx, trait_ast.clone());

	// Supertrait bounds: prefer full canonical path for the trait name (C11).
	// Written generic args come from AST (HIR drops them).
	let ast_super_args = super_trait_args_from_ast(ctx, trait_ast.as_ref());
	let super_traits: Vec<TraitRef> = t
		.direct_supertraits(ctx.db)
		.into_iter()
		.map(|st| {
			let name = ctx
				.canonical(ModuleDef::Trait(st))
				.map(|k| k.to_string())
				.unwrap_or_else(|| st.name(ctx.db).as_str().to_owned());
			let leaf = st.name(ctx.db).as_str().to_owned();
			let args = ast_super_args
				.as_ref()
				.and_then(|m| {
					m.iter()
						.find(|(n, _)| ast_name_matches(n, &leaf) || ast_name_matches(n, &name))
						.map(|(_, a)| a.clone())
				})
				.unwrap_or_default();
			TraitRef { name, args }
		})
		.collect();

	let trait_path_key = ctx.canonical(ModuleDef::Trait(t));
	let mut required_methods = Vec::new();
	let mut provided_methods = Vec::new();
	let mut associated_types = Vec::new();
	let mut required_constants = Vec::new();
	let mut member_paths = Vec::new();

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
				// Dual-emit indexable Function under Trait::method.
				if let Some(trait_key) = trait_path_key.as_ref()
					&& let Some(fn_) = function::lower_function(ctx, f)
				{
					let method_name = f.name(ctx.db).as_str().to_owned();
					let method_key = PathKey::from(format!("{trait_key}::{method_name}"));
					let path = NudoxPath::Local(std::path::PathBuf::from(method_key.as_str()));
					member_paths.push(path.clone());
					ctx.deferred.push(Entry::Function(ir::kind::Symbol {
						name: method_name,
						path,
						aliases: None,
						visibility: ctx.visibility(f),
						documentation: docs::documentation(ctx, f),
						deprecation: docs::deprecation(ctx, ModuleDef::Function(f)),
						doc_links: None,
						inner: fn_,
					}));
					if let Some((_k, text)) = source::fn_source(ctx, f) {
						ctx.deferred_sources
							.push((method_key.to_string(), text));
					}
				}
			}
			AssocItem::TypeAlias(ta) => {
				let name = ta.name(ctx.db).as_str().to_owned();
				let ta_ast = ctx
					.sema
					.source(ta)
					.or_else(|| ta.source(ctx.db))
					.map(|src| src.value);
				// Bounds from AST type-bound list on the assoc type.
				let bounds = ta_ast
					.as_ref()
					.and_then(|a| a.type_bound_list())
					.map(|list| ty::lower_generic_bounds(ctx, Some(list)))
					.filter(|b| !b.is_empty());
				let default_type = ta_ast
					.as_ref()
					.and_then(|a| a.ty())
					.map(|ty_node| ty::lower_ast_type(ctx, &ty_node));
				associated_types.push(AssociatedType {
					name: name.clone(),
					bounds,
					default_type: default_type.clone(),
				});
				// Dual-emit TypeAlias under Trait::Assoc.
				if let Some(trait_key) = trait_path_key.as_ref() {
					let item_key = PathKey::from(format!("{trait_key}::{name}"));
					let path = NudoxPath::Local(std::path::PathBuf::from(item_key.as_str()));
					member_paths.push(path.clone());
					let body = TypeAliasBody {
						generics: None,
						target: default_type.unwrap_or(Type::Infer),
					};
					ctx.deferred.push(Entry::TypeAlias(ir::kind::Symbol {
						name,
						path,
						aliases: None,
						visibility: Visibility::Public,
						documentation: docs::documentation(ctx, ta),
						deprecation: None,
						doc_links: None,
						inner: body,
					}));
				}
			}
			AssocItem::Const(c) => {
				let name = c
					.name(ctx.db)
					.map(|n| n.as_str().to_owned())
					.unwrap_or_default();
				// Prefer AST type; fall back to HIR — never leave bare Infer when
				// recoverable (C8).
				let const_ty = lower_assoc_const_type(ctx, c);
				let default_value = lower_const_value_from_hir(ctx, c);
				required_constants.push(TraitConstant {
					name: name.clone(),
					r#type: Box::new(const_ty.clone()),
					default_value: default_value.clone(),
				});
				// Dual-emit Constant under Trait::CONST.
				if let Some(trait_key) = trait_path_key.as_ref() {
					let item_key = PathKey::from(format!("{trait_key}::{name}"));
					let path = NudoxPath::Local(std::path::PathBuf::from(item_key.as_str()));
					member_paths.push(path.clone());
					ctx.deferred.push(Entry::Constant(ir::kind::Symbol {
						name,
						path,
						aliases: None,
						visibility: Visibility::Public,
						documentation: docs::documentation(ctx, c),
						deprecation: None,
						doc_links: None,
						inner: TypedBinding {
							ty: Some(const_ty),
							value: default_value,
							mutable: Some(false),
						},
					}));
				}
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

	// Object-safety (dyn-compatibility): `dyn_compatibility` returns the first
	// violation, so `None` means the trait IS usable as `dyn Trait`.
	let object_safe = Some(t.dyn_compatibility(ctx.db).is_none());

	// Sealed-trait detection (classic `pub trait Foo: private::Sealed {}`).
	let sealed = Some(
		t.direct_supertraits(ctx.db)
			.into_iter()
			.any(|st| !supertrait_reachable_downstream(ctx, st)),
	);

	let cfg = docs::cfg_string(ctx, t);

	(
		TraitDef {
			generics,
			super_traits: empty_to_none(super_traits),
			associated_types: empty_to_none(associated_types),
			properties: None,
			required_methods: empty_to_none(required_methods),
			provided_methods: empty_to_none(provided_methods),
			required_constants: empty_to_none(required_constants),
			attributes,
			object_safe,
			sealed,
			cfg,
			members: None, // filled by caller from member_paths
		},
		member_paths,
	)
}

fn lower_trait_impl(ctx: &mut LowerCtx<'_>, imp: Impl, trait_: Trait) -> Option<Entry> {
	let for_type = Box::new(lower_self_ty(ctx, imp));
	let impl_ast = ctx
		.sema
		.source(imp)
		.or_else(|| imp.source(ctx.db))
		.map(|s| s.value);
	// Prefer full canonical path for the trait ref (C11). Written generic
	// args (`impl Trait<Args> for T`) come from the AST — HIR drops them.
	let trait_name = ctx
		.canonical(ModuleDef::Trait(trait_))
		.map(|k| k.to_string())
		.unwrap_or_else(|| trait_.name(ctx.db).as_str().to_owned());
	let tr = TraitRef {
		name: trait_name,
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
				let const_ty = lower_assoc_const_type(ctx, c);
				let default_value = lower_const_value_from_hir(ctx, c);
				associated_constants.push(TraitConstant {
					name,
					r#type: Box::new(const_ty),
					default_value,
				});
			}
		}
	}

	// Disambiguate by self-type so two impls of the same trait do not collide
	// under `{module}::<impl Trait>` (C3).
	let for_label = self_ty_path_label(for_type.as_ref());
	// Short trait leaf for the display name; full path lives on TraitRef.
	let trait_leaf = tr
		.name
		.rsplit("::")
		.next()
		.unwrap_or(tr.name.as_str())
		.to_owned();
	let name = format!("<impl {trait_leaf} for {for_label}>");
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

// ── const / static / type alias ──────────────────────────────────────────────

/// Lower a free `const` into a [`TypedBinding`] (type from AST, value when
/// the RHS is a simple literal / path).
fn lower_const_binding(ctx: &mut LowerCtx<'_>, c: ra_ap_hir::Const) -> TypedBinding {
	let src = ctx.sema.source(c).or_else(|| c.source(ctx.db));
	let ty = src
		.as_ref()
		.and_then(|s| s.value.ty())
		.map(|t| ty::lower_ast_type(ctx, &t))
		.or_else(|| {
			// HIR fallback when AST ty is missing.
			let hir_ty = attach_db(ctx.db, || c.ty(ctx.db));
			Some(ty::lower_hir_type_fallback(ctx, &hir_ty))
		});
	let value = src
		.as_ref()
		.and_then(|s| s.value.body())
		.and_then(|e| lower_simple_const_expr(&e));
	TypedBinding {
		ty,
		value,
		mutable: Some(false),
	}
}

/// Lower a free `static` into a [`TypedBinding`]. `static mut` → mutable.
fn lower_static_binding(ctx: &mut LowerCtx<'_>, s: ra_ap_hir::Static) -> TypedBinding {
	let src = ctx.sema.source(s).or_else(|| s.source(ctx.db));
	let ty = src
		.as_ref()
		.and_then(|s| s.value.ty())
		.map(|t| ty::lower_ast_type(ctx, &t))
		.or_else(|| {
			let hir_ty = attach_db(ctx.db, || s.ty(ctx.db));
			Some(ty::lower_hir_type_fallback(ctx, &hir_ty))
		});
	let value = src
		.as_ref()
		.and_then(|s| s.value.body())
		.and_then(|e| lower_simple_const_expr(&e));
	// Prefer HIR `is_mut` (authoritative) over AST mut_token.
	let mutable = Some(s.is_mut(ctx.db));
	TypedBinding { ty, value, mutable }
}

fn lower_type_alias(ctx: &mut LowerCtx<'_>, ta: ra_ap_hir::TypeAlias) -> TypeAliasBody {
	let src = ctx.sema.source(ta).or_else(|| ta.source(ctx.db));
	let generics = source_generics(ctx, src.as_ref().map(|s| s.value.clone()));
	let target = if let Some(src) = &src
		&& let Some(ty_node) = src.value.ty()
	{
		ty::lower_ast_type(ctx, &ty_node)
	} else {
		// No AST — HIR display path (lifetimes erased).
		let hir_ty = attach_db(ctx.db, || ta.ty(ctx.db));
		ty::lower_hir_type_fallback(ctx, &hir_ty)
	};
	TypeAliasBody::with_generics(generics, target)
}

/// Recover a structured [`ConstExpr`] from AST.
///
/// Handles literals, paths, unary/binary arithmetic & bitops, calls,
/// casts, fields, indexes, arrays/tuples (as `Call`), blocks with a
/// single tail expr, and `if` with both arms. Falls back to
/// [`ConstExpr::Var`] with source text only when structure cannot be
/// recovered — never silent-drops.
fn lower_simple_const_expr(expr: &ast::Expr) -> Option<ConstExpr> {
	use ir::generics::{BinOp, UnaryOp};
	match expr {
		ast::Expr::Literal(lit) => lower_literal_const_expr(lit),
		ast::Expr::PrefixExpr(p) => {
			let kind = p.op_kind()?;
			let inner = p.expr().and_then(|e| lower_simple_const_expr(&e))?;
			let op = match kind {
				ast::UnaryOp::Neg => UnaryOp::Neg,
				ast::UnaryOp::Not => UnaryOp::Not,
				ast::UnaryOp::Deref => UnaryOp::Deref,
				// `*const`/`&` prefixes in const positions are rare; keep text.
				_ => {
					return Some(ConstExpr::Var(expr.syntax().text().to_string()));
				}
			};
			// Fold integer/float negation.
			if matches!(op, UnaryOp::Neg) {
				match inner {
					ConstExpr::Int(i) => return Some(ConstExpr::Int(-i)),
					ConstExpr::Float(f) => return Some(ConstExpr::Float(-f)),
					other => {
						return Some(ConstExpr::UnaryOp {
							op,
							operand: Box::new(other),
						});
					}
				}
			}
			Some(ConstExpr::UnaryOp {
				op,
				operand: Box::new(inner),
			})
		}
		ast::Expr::BinExpr(b) => {
			let op_kind = b.op_kind()?;
			let lhs = b.lhs().and_then(|e| lower_simple_const_expr(&e))?;
			let rhs = b.rhs().and_then(|e| lower_simple_const_expr(&e))?;
			let op = match op_kind {
				ast::BinaryOp::ArithOp(ast::ArithOp::Add) => BinOp::Add,
				ast::BinaryOp::ArithOp(ast::ArithOp::Sub) => BinOp::Sub,
				ast::BinaryOp::ArithOp(ast::ArithOp::Mul) => BinOp::Mul,
				ast::BinaryOp::ArithOp(ast::ArithOp::Div) => BinOp::Div,
				ast::BinaryOp::ArithOp(ast::ArithOp::Rem) => BinOp::Rem,
				ast::BinaryOp::ArithOp(ast::ArithOp::Shl) => BinOp::Shl,
				ast::BinaryOp::ArithOp(ast::ArithOp::Shr) => BinOp::Shr,
				ast::BinaryOp::ArithOp(ast::ArithOp::BitAnd) => BinOp::BitAnd,
				ast::BinaryOp::ArithOp(ast::ArithOp::BitOr) => BinOp::BitOr,
				ast::BinaryOp::ArithOp(ast::ArithOp::BitXor) => BinOp::BitXor,
				ast::BinaryOp::CmpOp(ast::CmpOp::Eq { negated: false }) => BinOp::Eq,
				ast::BinaryOp::CmpOp(ast::CmpOp::Eq { negated: true }) => BinOp::Ne,
				ast::BinaryOp::CmpOp(ast::CmpOp::Ord {
					ordering: ast::Ordering::Less,
					strict: true,
				}) => BinOp::Lt,
				ast::BinaryOp::CmpOp(ast::CmpOp::Ord {
					ordering: ast::Ordering::Less,
					strict: false,
				}) => BinOp::Le,
				ast::BinaryOp::CmpOp(ast::CmpOp::Ord {
					ordering: ast::Ordering::Greater,
					strict: true,
				}) => BinOp::Gt,
				ast::BinaryOp::CmpOp(ast::CmpOp::Ord {
					ordering: ast::Ordering::Greater,
					strict: false,
				}) => BinOp::Ge,
				ast::BinaryOp::LogicOp(ast::LogicOp::And) => BinOp::And,
				ast::BinaryOp::LogicOp(ast::LogicOp::Or) => BinOp::Or,
				_ => {
					return Some(ConstExpr::Var(expr.syntax().text().to_string()));
				}
			};
			// Fold pure integer arithmetic when both sides are Int.
			if let (ConstExpr::Int(a), ConstExpr::Int(b)) = (&lhs, &rhs) {
				let folded = match op {
					BinOp::Add => a.checked_add(*b),
					BinOp::Sub => a.checked_sub(*b),
					BinOp::Mul => a.checked_mul(*b),
					BinOp::Div if *b != 0 => Some(a / b),
					BinOp::Rem if *b != 0 => Some(a % b),
					BinOp::BitAnd => Some(a & b),
					BinOp::BitOr => Some(a | b),
					BinOp::BitXor => Some(a ^ b),
					BinOp::Shl if (0..64).contains(b) => a.checked_shl(*b as u32),
					BinOp::Shr if (0..64).contains(b) => a.checked_shr(*b as u32),
					_ => None,
				};
				if let Some(n) = folded {
					return Some(ConstExpr::Int(n));
				}
			}
			Some(ConstExpr::BinOp {
				op,
				lhs: Box::new(lhs),
				rhs: Box::new(rhs),
			})
		}
		ast::Expr::PathExpr(p) => {
			let text = p
				.path()
				.map(|path| path.syntax().text().to_string())
				.unwrap_or_else(|| p.syntax().text().to_string());
			Some(ConstExpr::Var(text))
		}
		ast::Expr::ParenExpr(p) => p.expr().and_then(|e| lower_simple_const_expr(&e)),
		ast::Expr::CallExpr(c) => {
			let func = c
				.expr()
				.map(|e| e.syntax().text().to_string())
				.unwrap_or_else(|| "call".into());
			let args = c
				.arg_list()
				.map(|al| {
					al.args()
						.filter_map(|a| lower_simple_const_expr(&a))
						.collect::<Vec<_>>()
				})
				.unwrap_or_default();
			Some(ConstExpr::Call { func, args })
		}
		ast::Expr::MethodCallExpr(m) => {
			let recv = m
				.receiver()
				.and_then(|r| lower_simple_const_expr(&r))
				.unwrap_or_else(|| ConstExpr::Var("self".into()));
			let name = m
				.name_ref()
				.map(|n| n.text().to_string())
				.unwrap_or_else(|| "method".into());
			let mut args = vec![recv];
			if let Some(al) = m.arg_list() {
				args.extend(al.args().filter_map(|a| lower_simple_const_expr(&a)));
			}
			Some(ConstExpr::Call {
				func: name,
				args,
			})
		}
		ast::Expr::IndexExpr(ix) => {
			let base = ix.base().and_then(|b| lower_simple_const_expr(&b))?;
			let index = ix.index().and_then(|i| lower_simple_const_expr(&i))?;
			Some(ConstExpr::Call {
				func: "index".into(),
				args: vec![base, index],
			})
		}
		ast::Expr::FieldExpr(f) => {
			let base = f.expr().and_then(|e| lower_simple_const_expr(&e))?;
			let name = f
				.name_ref()
				.map(|n| n.text().to_string())
				.unwrap_or_else(|| "field".into());
			Some(ConstExpr::Call {
				func: format!("field:{name}"),
				args: vec![base],
			})
		}
		ast::Expr::CastExpr(c) => {
			let inner = c.expr().and_then(|e| lower_simple_const_expr(&e))?;
			let ty_text = c
				.ty()
				.map(|t| t.syntax().text().to_string())
				.unwrap_or_else(|| "_".into());
			Some(ConstExpr::Call {
				func: format!("as {ty_text}"),
				args: vec![inner],
			})
		}
		ast::Expr::RefExpr(r) => {
			let inner = r.expr().and_then(|e| lower_simple_const_expr(&e))?;
			Some(ConstExpr::UnaryOp {
				op: UnaryOp::Ref,
				operand: Box::new(inner),
			})
		}
		ast::Expr::ArrayExpr(a) => {
			let args: Vec<ConstExpr> = a.exprs().filter_map(|e| lower_simple_const_expr(&e)).collect();
			Some(ConstExpr::Call {
				func: "array".into(),
				args,
			})
		}
		ast::Expr::TupleExpr(t) => {
			let args: Vec<ConstExpr> = t.fields().filter_map(|e| lower_simple_const_expr(&e)).collect();
			Some(ConstExpr::Call {
				func: "tuple".into(),
				args,
			})
		}
		ast::Expr::BlockExpr(b) => {
			// Single-tail-expression blocks: `{ N + 1 }`
			let tail = b.tail_expr().or_else(|| {
				b.statements().find_map(|s| match s {
					ast::Stmt::ExprStmt(es) => es.expr(),
					_ => None,
				})
			});
			tail.and_then(|e| lower_simple_const_expr(&e)).or_else(|| {
				let text = expr.syntax().text().to_string();
				if text.is_empty() {
					None
				} else {
					Some(ConstExpr::Var(text))
				}
			})
		}
		ast::Expr::IfExpr(i) => {
			// Represent as Call("if", [cond, then, else?]) so structure survives.
			let mut args = Vec::new();
			if let Some(cond) = i.condition() {
				// Condition may be a plain expr or `let` pattern; plain only.
				if let Some(c) = lower_simple_const_expr(&cond) {
					args.push(c);
				} else {
					args.push(ConstExpr::Var(cond.syntax().text().to_string()));
				}
			}
			if let Some(then_block) = i.then_branch() {
				if let Some(t) = then_block.tail_expr().and_then(|e| lower_simple_const_expr(&e)) {
					args.push(t);
				} else {
					args.push(ConstExpr::Var(then_block.syntax().text().to_string()));
				}
			}
			if let Some(else_branch) = i.else_branch() {
				match else_branch {
					ast::ElseBranch::Block(b) => {
						if let Some(t) = b.tail_expr().and_then(|e| lower_simple_const_expr(&e)) {
							args.push(t);
						} else {
							args.push(ConstExpr::Var(b.syntax().text().to_string()));
						}
					}
					ast::ElseBranch::IfExpr(nested) => {
						args.push(
							lower_simple_const_expr(&ast::Expr::IfExpr(nested))
								.unwrap_or_else(|| {
									ConstExpr::Var(expr.syntax().text().to_string())
								}),
						);
					}
				}
			}
			Some(ConstExpr::Call {
				func: "if".into(),
				args,
			})
		}
		// Non-trivial body: keep the written text so we do not silent-drop.
		_ => {
			let text = expr.syntax().text().to_string();
			if text.is_empty() {
				None
			} else {
				Some(ConstExpr::Var(text))
			}
		}
	}
}

fn lower_literal_const_expr(lit: &ast::Literal) -> Option<ConstExpr> {
	use ra_ap_syntax::ast::AstToken;
	let kind = lit.kind();
	match kind {
		ast::LiteralKind::IntNumber(n) => {
			let text = n.text().replace('_', "");
			// Strip type suffix (`4i32`, `0u8`).
			let digits: String = text
				.chars()
				.take_while(|c| c.is_ascii_digit() || *c == '-')
				.collect();
			digits
				.parse::<i64>()
				.ok()
				.map(ConstExpr::Int)
				.or_else(|| Some(ConstExpr::Var(text)))
		}
		ast::LiteralKind::FloatNumber(n) => {
			let text = n.text().replace('_', "");
			let digits: String = text
				.chars()
				.take_while(|c| {
					c.is_ascii_digit()
						|| *c == '.' || *c == '-' || *c == 'e' || *c == 'E' || *c == '+'
				})
				.collect();
			digits
				.parse::<f64>()
				.ok()
				.map(ConstExpr::Float)
				.or_else(|| Some(ConstExpr::Var(text)))
		}
		ast::LiteralKind::String(s) => {
			// `value()` → Result; fall back to quote-stripped raw text.
			let v = s
				.value()
				.map(|cow| cow.into_owned())
				.unwrap_or_else(|_| s.text().trim_matches('"').to_string());
			Some(ConstExpr::Str(v))
		}
		ast::LiteralKind::ByteString(s) => Some(ConstExpr::Str(s.text().to_string())),
		ast::LiteralKind::CString(s) => Some(ConstExpr::Str(s.text().to_string())),
		ast::LiteralKind::Char(c) => {
			let v = c
				.value()
				.ok()
				.map(|ch| ch.to_string())
				.unwrap_or_else(|| c.text().trim_matches('\'').to_string());
			Some(ConstExpr::Str(v))
		}
		ast::LiteralKind::Byte(b) => Some(ConstExpr::Var(b.text().to_string())),
		ast::LiteralKind::Bool(true) => Some(ConstExpr::Bool(true)),
		ast::LiteralKind::Bool(false) => Some(ConstExpr::Bool(false)),
	}
}

fn lower_assoc_const_type(ctx: &mut LowerCtx<'_>, c: ra_ap_hir::Const) -> Type {
	if let Some(src) = ctx.sema.source(c).or_else(|| c.source(ctx.db))
		&& let Some(ty_node) = src.value.ty()
	{
		return ty::lower_ast_type(ctx, &ty_node);
	}
	let hir_ty = attach_db(ctx.db, || c.ty(ctx.db));
	ty::lower_hir_type_fallback(ctx, &hir_ty)
}

fn lower_const_value_from_hir(
	ctx: &mut LowerCtx<'_>,
	c: ra_ap_hir::Const,
) -> Option<ConstExpr> {
	ctx.sema
		.source(c)
		.or_else(|| c.source(ctx.db))
		.and_then(|s| s.value.body())
		.and_then(|e| lower_simple_const_expr(&e))
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
	if let Some(tp) = attach_db(ctx.db, || ty.as_type_param(ctx.db)) {
		let name = tp.name(ctx.db).as_str().to_owned();
		return Type::GenericParam(ir::ty::GenericParam {
			name: if name.is_empty() { "_".into() } else { name },
			kind: None,
		});
	}
	Type::Infer
}

/// Human-readable self-type label for TraitImpl path disambiguation.
fn self_ty_path_label(ty: &Type) -> String {
	match ty {
		Type::TypeReference(tr) => tr
			.identifier
			.rsplit("::")
			.next()
			.unwrap_or(&tr.identifier)
			.to_owned(),
		Type::GenericParam(gp) => {
			if gp.name.is_empty() {
				"T".into()
			} else {
				gp.name.clone()
			}
		}
		Type::Primitive(p) => format!("{p:?}"),
		Type::Tuple(ts) if ts.is_empty() => "()".into(),
		Type::SelfType => "Self".into(),
		Type::Infer => "_".into(),
		other => {
			// Last resort: compact debug, strip spaces for path safety.
			format!("{other:?}").replace(' ', "")
		}
	}
}

/// `(resolved-name, written-args)` for each path-type bound in the trait's AST
/// bound list (`trait T: A + B<X>`).
fn super_trait_args_from_ast(
	ctx: &mut LowerCtx<'_>,
	trait_ast: Option<&ast::Trait>,
) -> Option<Vec<(String, Vec<ir::generics::TypeExpr>)>> {
	let list = trait_ast?.type_bound_list()?;
	let mut out = Vec::new();
	for bound in list.bounds() {
		if let Some(ast::TypeBoundKind::PathType(_for_binder, path_ty)) = bound.kind() {
			let tr = ty::path_type_to_trait_ref(ctx, &path_ty);
			if !tr.args.is_empty() {
				out.push((tr.name, tr.args));
			}
		}
	}
	Some(out)
}

/// Match a resolved bound name (possibly canonical, e.g. `core::cmp::PartialOrd`)
/// against an unqualified HIR supertrait name (`PartialOrd`).
fn ast_name_matches(resolved: &str, hir_name: &str) -> bool {
	resolved == hir_name
		|| resolved
			.rsplit("::")
			.next()
			.is_some_and(|last| last == hir_name)
}

/// Whether a supertrait can be named (and thus implemented) by a downstream
/// crate: its own declared visibility must be `Public` and every enclosing module
/// below the crate root must be `pub`.
fn supertrait_reachable_downstream(ctx: &LowerCtx<'_>, st: Trait) -> bool {
	if !matches!(st.visibility(ctx.db), HirVisibility::Public) {
		return false;
	}
	let module = st.module(ctx.db);
	module
		.path_to_root(ctx.db)
		.into_iter()
		.filter(|m| !m.is_crate_root(ctx.db))
		.all(|m| matches!(m.visibility(ctx.db), HirVisibility::Public))
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

/// Module path segments for building re-export member paths. Uses the same
/// canonicalisation as [`LowerCtx::canonical`] without requiring a public
/// `path_segments` method.
fn ctx_path_segments_for_module(ctx: &mut LowerCtx<'_>, module: Module) -> Option<Vec<String>> {
	let key = ctx.canonical(ModuleDef::Module(module))?;
	Some(key.split("::").map(str::to_owned).collect())
}

fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}
