use std::collections::HashMap as StdHashMap;
use std::sync::Arc;

use ir::{
	entry::NudoxPath,
	function::Function,
	generics::{GenericArg, TraitRef},
	kind::{Deprecation, Entry, Symbol, TypedBinding, TypeAliasBody, Visibility},
	module::Module,
	parameter::Parameter as IrParameter,
	protocols::{TraitDef, TraitMethod},
	record::{
		Field, FieldAttributes, FieldKey, KnownField, Record, SumField, SumVariant,
	},
	ty::{Type as IrType, TypeReference},
};
use pyrefly::{
	alt::answers::Answers,
	binding::{
		binding::{KeyClassField, KeyClassMetadata, KeyTParams},
		bindings::Bindings,
	},
	state::state::Transaction,
};
use pyrefly_build::handle::Handle;
use pyrefly_python::module_name::ModuleName;
use pyrefly_types::{
	callable::{FuncMetadata, FunctionKind},
	class::{Class, ClassFields},
	literal::Lit,
	types::{Forallable, TParams, Type as PyType},
};

use super::docstring::{DocCatalog, ParsedDocstring};
use super::{docstring, function, types};

/// Lower the module-level items (classes, functions, constants, type aliases)
/// from pyrefly's `Bindings` into a `Vec<(NudoxPath, Entry)>`.
///
/// Called from `PythonContext::lower_handle`. Each entry is paired with its
/// fully-qualified `NudoxPath` for insertion into the IR index.
pub fn lower_module(
	handle: &Handle,
	tx: &Transaction,
	module_path: &NudoxPath,
) -> Vec<(NudoxPath, Entry)> {
	let mut entries: Vec<(NudoxPath, Entry)> = Vec::new();

	let bindings = match tx.get_bindings(handle) {
		Some(b) => b,
		None => return entries,
	};
	let answers = match tx.get_answers(handle) {
		Some(a) => a,
		None => return entries,
	};

	let module_name = handle.module().as_str().to_string();

	// Phase 3 "doc brain": build a per-module docstring catalog from the Ruff
	// AST. Pyrefly does not attach docstrings to the `Type` values, and the
	// per-export `docstring_range` lives behind a private `ExportLocation`, so
	// the AST is the reachable source for method docstrings.
	let module_info = tx.get_module_info(handle);
	let catalog = match (tx.get_ast(handle), &module_info) {
		(Some(ast), Some(mi)) => docstring::build_catalog(&ast.body, mi),
		_ => DocCatalog::default(),
	};

	// Module entry, carrying the module-level docstring.
	let nudox_path = module_path.clone();
	entries.push((
		nudox_path.clone(),
		Entry::Module(Symbol {
			name:          module_name.clone(),
			path:          nudox_path.clone(),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: catalog.module.as_ref().and_then(ParsedDocstring::documentation),
			deprecation:   catalog.module.as_ref().and_then(deprecation_from_doc),
			doc_links:     None,
			inner:         Module { members: None },
		}),
	));

	// Iterate over exported names in the module's bindings.
	// We inspect the KeyExport table to find what names are exported
	// from this module, then check their types via Answers.
	for export_idx in bindings.keys::<pyrefly::binding::binding::KeyExport>() {
		let key = bindings.idx_to_key(export_idx);
		let export_name = key.0.to_string();

		// Resolve the exported binding's *solved* type from the answers table.
		// `get_idx` returns the type Pyrefly inferred/checked for this export
		// (the `KeyExport` answer is a `Type`), which is exactly the resolved
		// type we want to lower — not a syntactic guess.
		let resolved = answers.get_idx(export_idx);
		let any = pyrefly_types::types::Type::Any(pyrefly_types::types::AnyStyle::Implicit);
		let ty = resolved.as_deref().unwrap_or(&any);

		// Phase 3 #1: a module re-exports every name it can see — including
		// `from enum import Enum`. Drop names whose *definition* lives in
		// another module so imports don't leak out as bogus entries.
		if !is_local_definition(ty, handle) {
			continue;
		}

		entries.extend(lower_binding(
			&export_name,
			ty,
			handle,
			tx,
			&bindings,
			&answers,
			&catalog,
		));
	}

	// Phase 3 #4: best-effort intra-module type linking.
	link_local_types(&mut entries, &module_name);

	entries
}

/// Phase 3 #1 — import-vs-local rule.
///
/// A re-exported name is kept only when its *definition* lives in this module.
/// The signal is the defining module recorded on the resolved type: a class's
/// `module_name()` and a function's `FuncId.module` (only `FunctionKind::Def`
/// is a user definition — every other `FunctionKind` is a special/builtin
/// callable such as `dataclasses.dataclass`). Submodule references and typing
/// special forms (`Protocol`, `Final`, …) are always imports.
///
/// Constants/instances carry no definition-module on their value type, so they
/// are kept conservatively (an imported *constant* can still leak — a
/// documented limitation; classes/functions/modules, the common imports, are
/// filtered reliably).
fn is_local_definition(ty: &PyType, handle: &Handle) -> bool {
	let this = handle.module();
	match ty {
		PyType::ClassDef(cls) => cls.module_name() == this,
		PyType::Function(f) => func_def_in_module(&f.metadata, this),
		PyType::Overload(o) => func_def_in_module(&o.metadata, this),
		PyType::Forall(forall) => match &forall.body {
			Forallable::Function(f) => func_def_in_module(&f.metadata, this),
			_ => true,
		},
		// Bound methods are not module-level exports; stay conservative.
		PyType::BoundMethod(_) => true,
		// `import os` surfaces a module value — not a local definition.
		PyType::Module(_) => false,
		// `from typing import Protocol/Final/...` — typing special forms.
		PyType::SpecialForm(_) => false,
		// Module-level `T = TypeVar("T")` is a local binding, but not a useful
		// public API surface for documentation IR — skip TypeVar/ParamSpec/
		// TypeVarTuple *values* so they don't pollute the index as Constants.
		PyType::TypeVar(_) | PyType::ParamSpec(_) | PyType::TypeVarTuple(_) => false,
		_ => true,
	}
}

fn func_def_in_module(meta: &FuncMetadata, this: ModuleName) -> bool {
	match &meta.kind {
		FunctionKind::Def(id) => id.module.name() == this,
		// A user-defined local function is always `Def`; any other kind is a
		// special/builtin callable (`dataclass`, `cast`, `overload`, …).
		_ => false,
	}
}

fn lower_binding(
	name: &str,
	ty: &pyrefly_types::types::Type,
	handle: &Handle,
	tx: &Transaction,
	bindings: &Bindings,
	answers: &Answers,
	catalog: &DocCatalog,
) -> Vec<(NudoxPath, Entry)> {
	let path =
		NudoxPath::Local(std::path::PathBuf::from(format!("{}::{}", handle.module().as_str(), name)));

	// Phase 3 #2/#3: docstring + decorator-derived shape for top-level callables.
	let doc = catalog.item(name);

	if is_function_like(ty) {
		let mut ir_func = function::lower_function(handle, tx, bindings, answers, name, ty);
		if let Some(doc) = doc {
			apply_param_docs(&mut ir_func, &doc.params);
			apply_return_doc(&mut ir_func, doc.returns.as_deref());
		}
		let deprecation = function::deprecation_from_flags(ty)
			.or_else(|| doc.and_then(deprecation_from_doc));
		return vec![(
			path.clone(),
			Entry::Function(Symbol {
				name:          name.to_string(),
				path,
				aliases:       None,
				visibility:    member_visibility(name),
				documentation: doc.and_then(ParsedDocstring::documentation),
				deprecation,
				doc_links:     None,
				inner:         ir_func,
			}),
		)];
	}

	match ty {
		// Class definition — extract fields, methods (as index entries), nested classes.
		pyrefly_types::types::Type::ClassDef(cls) => {
			lower_class(name, cls, handle, tx, bindings, answers, &path, catalog, None)
		}

		// Type alias
		pyrefly_types::types::Type::TypeAlias(alias)
		| pyrefly_types::types::Type::UntypedAlias(alias) => {
			// Prefer declaration-site generics when the alias is a Forall wrapper.
			// TypeAlias / UntypedAlias payloads do not currently expose tparams
			// separately from the RHS, so we only store the target type.
			let _ = alias;
			vec![(
				path.clone(),
				Entry::TypeAlias(Symbol {
					name:          name.to_string(),
					path,
					aliases:       None,
					visibility:    member_visibility(name),
					documentation: doc.and_then(ParsedDocstring::documentation),
					deprecation:   doc.and_then(deprecation_from_doc),
					doc_links:     None,
					inner:         TypeAliasBody::plain(types::lower_type(ty)),
				}),
			)]
		}

		// Leading-underscore module names are private; skip (except dunder init).
		_ if name.starts_with('_') && name != "__init__" => Vec::new(),

		// Everything else — treat as a constant or variable (typed payload).
		_ => {
			let value = types::const_expr_from_type(ty);
			vec![(
				path.clone(),
				Entry::Constant(Symbol {
					name:          name.to_string(),
					path,
					aliases:       None,
					visibility:    member_visibility(name),
					documentation: doc.and_then(ParsedDocstring::documentation),
					deprecation:   doc.and_then(deprecation_from_doc),
					doc_links:     None,
					inner: TypedBinding {
						ty: Some(types::lower_type(ty)),
						value,
						// Module-level annotated bindings are treated as constants
						// (immutable); Python has no true const, but ALL_CAPS and
						// Final annotations conventionally are.
						mutable: Some(false),
					},
				}),
			)]
		}
	}
}

/// Whether a resolved type should be lowered as a function entry. Covers plain
/// functions/callables/bound-methods, pre-merged overloads, and generic
/// (`Forall`) functions — but not a `Forall` wrapping a non-callable.
fn is_function_like(ty: &PyType) -> bool {
	match ty {
		PyType::Function(_)
		| PyType::Callable(_)
		| PyType::BoundMethod(_)
		| PyType::Overload(_) => true,
		PyType::Forall(f) => matches!(&f.body, Forallable::Function(_)),
		_ => false,
	}
}

/// Attach per-parameter docstring descriptions to a lowered function's literal
/// input parameters (matched by name).
fn apply_param_docs(func: &mut Function, params: &StdHashMap<String, String>) {
	apply_param_descs(&mut func.input_parameters, params);
}

/// Attach a `Returns:` docstring description onto the function's first output
/// parameter (the conventional single return slot).
fn apply_return_doc(func: &mut Function, returns: Option<&str>) {
	let Some(desc) = returns else { return };
	if let Some(outs) = func.output_parameters.as_mut() {
		if let Some(IrParameter::Literal(lp)) = outs.first_mut() {
			if lp.description.is_none() {
				lp.description = Some(desc.to_string());
			}
		}
	}
}

/// Attach per-parameter docstring descriptions to a parameter list (matched by
/// name). Shared by free functions, record methods, and protocol methods.
fn apply_param_descs(
	inputs: &mut Option<Vec<IrParameter>>,
	params: &StdHashMap<String, String>,
) {
	if params.is_empty() {
		return;
	}
	if let Some(inputs) = inputs.as_mut() {
		for p in inputs.iter_mut() {
			if let IrParameter::Literal(lp) = p {
				if let Some(desc) = params.get(&lp.name) {
					lp.description = Some(desc.clone());
				}
			}
		}
	}
}

/// Detect a deprecation marker from docstring prose (`Deprecated: …` /
/// `.. deprecated::` / bare `Deprecated`).
fn deprecation_from_doc(doc: &ParsedDocstring) -> Option<Deprecation> {
	let text = doc.documentation()?;
	// Sphinx `.. deprecated:: 1.2` style.
	if let Some(rest) = text.split(".. deprecated::").nth(1) {
		let mut lines = rest.lines();
		let first = lines.next().unwrap_or("").trim();
		let (since, note_start): (Option<String>, String) = if first.is_empty() {
			(None, rest.to_string())
		} else if first
			.chars()
			.next()
			.is_some_and(|c| c.is_ascii_digit())
		{
			let mut parts = first.splitn(2, char::is_whitespace);
			let ver = parts.next().map(str::to_string);
			let rest_note = parts.next().unwrap_or("").trim();
			let body: String = std::iter::once(rest_note)
				.chain(lines.map(str::trim))
				.filter(|s| !s.is_empty())
				.collect::<Vec<_>>()
				.join(" ");
			(ver, body)
		} else {
			(None, first.to_string())
		};
		let note = {
			let n = note_start.trim();
			if n.is_empty() { None } else { Some(n.to_string()) }
		};
		return Some(Deprecation { since, note });
	}
	// Google / prose: line starting with "Deprecated".
	for line in text.lines() {
		let t = line.trim();
		if let Some(rest) = t.strip_prefix("Deprecated:") {
			return Some(Deprecation {
				since: None,
				note:  Some(rest.trim().to_string()),
			});
		}
		if t == "Deprecated" || t.starts_with("Deprecated ") {
			return Some(Deprecation {
				since: None,
				note:  Some(t.to_string()),
			});
		}
	}
	None
}

/// Phase 3 #4 — best-effort intra-module type linking.
///
/// SCOPE: *intra-module only*. We map a type-reference identifier that names a
/// type defined in **this** module (Pyrefly qname `"<module>.<Name>"`) to a
/// synthetic, stable id for the corresponding local entry. Cross-module linking
/// is DEFERRED — it needs the global registry's id assignment, which is owned
/// outside this lowering pass.
///
/// The id is a stable 64-bit FNV-1a hash of the target entry's `NudoxPath`
/// string, so a consumer can recompute the same id from any entry's path; these
/// are not (yet) the registry's canonical ids.
fn link_local_types(entries: &mut [(NudoxPath, Entry)], module_name: &str) {
	// Build identifier -> id for every type-like local entry.
	let mut local: StdHashMap<String, i64> = StdHashMap::new();
	for (path, entry) in entries.iter() {
		if is_type_entry(entry) {
			local.insert(format!("{}.{}", module_name, entry.name()), path_id(path));
		}
	}
	if local.is_empty() {
		return;
	}

	for (_path, entry) in entries.iter_mut() {
		if let Entry::Function(sym) = entry {
			let mut refs = Vec::new();
			collect_function_refs(&sym.inner, &mut refs);

			let mut links = sym.inner.type_links.take().unwrap_or_default();
			for ident in refs {
				if let Some(&id) = local.get(&ident) {
					links.insert(ident, id);
				}
			}
			if !links.is_empty() {
				sym.inner.type_links = Some(links);
			}
		}
	}
}

fn is_type_entry(entry: &Entry) -> bool {
	matches!(
		entry,
		Entry::RecordType(_) | Entry::SumType(_) | Entry::TraitDef(_) | Entry::TypeAlias(_)
	)
}

/// Stable 64-bit FNV-1a hash of a `NudoxPath`'s debug form, as a non-negative
/// `i64` (the IR's `type_links` value type).
fn path_id(path: &NudoxPath) -> i64 {
	let key = format!("{path:?}");
	let mut hash: u64 = 0xcbf29ce484222325;
	for byte in key.as_bytes() {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
	}
	(hash >> 1) as i64
}

/// Collect every `TypeReference` identifier mentioned by a function's input and
/// output parameter types (including overload branches).
fn collect_function_refs(func: &Function, acc: &mut Vec<String>) {
	for list in [&func.input_parameters, &func.output_parameters] {
		if let Some(params) = list {
			for p in params {
				if let IrParameter::Literal(lp) = p {
					if let Some(ty) = &lp.r#type {
						collect_type_refs(ty, acc);
					}
				}
			}
		}
	}
	if let Some(overloads) = &func.overloads {
		for branch in overloads {
			collect_function_refs(branch, acc);
		}
	}
}

/// Walk an IR type, pushing every `TypeReference` identifier (and those nested
/// in generic args / unions / tuples / containers) onto `acc`.
fn collect_type_refs(ty: &IrType, acc: &mut Vec<String>) {
	match ty {
		IrType::TypeReference(r) => {
			acc.push(r.identifier.clone());
			if let Some(args) = &r.generic_args {
				for arg in args {
					if let GenericArg::Type(t) = arg {
						collect_type_refs(t, acc);
					}
				}
			}
		}
		IrType::Union(v) | IrType::Intersection(v) | IrType::Tuple(v) => {
			v.iter().for_each(|t| collect_type_refs(t, acc));
		}
		IrType::Slice(b) | IrType::Variadic(b) => collect_type_refs(b, acc),
		IrType::FunctionPointer(fp) => {
			for list in [&fp.inputs, &fp.outputs] {
				if let Some(params) = list {
					for p in params {
						if let IrParameter::Literal(lp) = p {
							if let Some(t) = &lp.r#type {
								collect_type_refs(t, acc);
							}
						}
					}
				}
			}
		}
		_ => {}
	}
}

/// Leading-underscore members are conventionally private in Python.
fn member_visibility(name: &str) -> Visibility {
	if name.starts_with('_') { Visibility::Private } else { Visibility::Public }
}

/// A resolved field type is a method (rather than a data attribute) when it is
/// one of pyrefly's callable type variants. Note: a data attribute explicitly
/// annotated with a `Callable[...]` type is (cheaply) indistinguishable here
/// and will be routed as a method — acceptable for this phase.
///
/// `Forall` is only treated as callable when it wraps a function (not a
/// type-alias body).
fn is_callable_type(ty: &PyType) -> bool {
	match ty {
		PyType::Function(_)
		| PyType::Callable(_)
		| PyType::BoundMethod(_)
		| PyType::Overload(_) => true,
		PyType::Forall(f) => matches!(&f.body, Forallable::Function(_)),
		_ => false,
	}
}

/// Build a `KnownField` for a class data attribute.
fn build_field(
	name: &str,
	ty: Option<IrType>,
	has_default: bool,
	is_class_var: bool,
	is_final: bool,
	is_property: bool,
) -> Field {
	let mut decorators = Vec::new();
	if is_class_var {
		decorators.push("ClassVar".into());
	}
	if is_final {
		decorators.push("Final".into());
	}
	if is_property {
		decorators.push("property".into());
	}
	Field::Known(KnownField {
		key:           FieldKey::Ident(name.to_string()),
		r#type:        ty.map(Box::new),
		default_value: None,
		attributes:    FieldAttributes {
			// Final / ClassVar are immutable at the type level; other
			// instance attributes remain mutable by default.
			is_mutable:  !(is_final || is_class_var),
			is_optional: has_default,
			is_static:   is_class_var,
			decorators,
		},
		visibility:    Some(member_visibility(name)),
		documentation: None,
	})
}

/// Project an already-lowered `Function` into a protocol `TraitMethod` shape.
fn function_to_trait_method(name: &str, func: Function) -> TraitMethod {
	let return_type = func
		.output_parameters
		.as_ref()
		.and_then(|outs| outs.first())
		.and_then(|p| match p {
			IrParameter::Literal(lp) => lp.r#type.clone(),
			_ => None,
		})
		.map(Box::new);

	TraitMethod {
		name: name.to_string(),
		parameters: func.input_parameters,
		return_type,
		generics: func.generics,
		attributes: func.attributes,
		documentation: None,
		receiver: func.receiver,
		// Protocol bodies in stubs are `...`; default impls are a later phase.
		has_default_implementation: false,
	}
}

/// Resolve the class's declaration-site type parameters.
fn resolve_class_tparams(
	cls: &Class,
	bindings: &Bindings,
	answers: &Answers,
) -> Option<Arc<TParams>> {
	if let Some(tp) = cls.precomputed_tparams() {
		return Some(tp.clone());
	}
	// Legacy TypeVar Generic[T] — solved via KeyTParams.
	for idx in bindings.keys::<KeyTParams>() {
		let key = bindings.idx_to_key(idx);
		if key.0 == cls.index() {
			// `get_idx` already returns `Option<Arc<TParams>>`.
			return answers.get_idx(idx);
		}
	}
	None
}

/// Member path under a class: `module::Class.member` (dot after the class,
/// matching Java/C# member-key minting so `path_segments` expands cleanly).
fn member_path(class_path: &NudoxPath, member: &str) -> NudoxPath {
	let base = match class_path {
		NudoxPath::Local(p) => p.display().to_string(),
		NudoxPath::External { path, dependency } => {
			format!("{}::{}", dependency, path.display())
		}
	};
	NudoxPath::Local(std::path::PathBuf::from(format!("{base}.{member}")))
}

/// Lower a class (and its methods / nested classes) into one or more index entries.
///
/// `owner_path` is the path of this class (`module::Name` or `module::Outer.Inner`).
/// `parent_class_name` is used for docstring catalog lookups of nested members.
fn lower_class(
	name: &str,
	cls: &Class,
	handle: &Handle,
	tx: &Transaction,
	_bindings: &Bindings,
	_answers: &Answers,
	path: &NudoxPath,
	catalog: &DocCatalog,
	// Fully-qualified class name used for docstring member lookups. For
	// top-level classes this is just `name`; for nested classes it is the
	// dotted chain (`Outer.Inner`).
	doc_class_name: Option<&str>,
) -> Vec<(NudoxPath, Entry)> {
	let mut out: Vec<(NudoxPath, Entry)> = Vec::new();
	let doc_key = doc_class_name.unwrap_or(name);

	// Class-level docstring (shared by the Record / SumType / TraitDef shapes).
	let class_doc_parsed = catalog.item(doc_key);
	let class_doc = class_doc_parsed.and_then(ParsedDocstring::documentation);
	let class_deprecation = class_doc_parsed.and_then(deprecation_from_doc);

	// Resolve bindings/answers for the class's *defining* module, so imported
	// classes resolve against the right tables. Mirrors `Transaction::
	// get_class_fields`, which keys a fresh `Handle` off the class's qname.
	let cls_handle =
		Handle::new(cls.module_name(), cls.module_path().clone(), handle.sys_info().clone());
	let Some(bindings) = tx.get_bindings(&cls_handle) else {
		return out;
	};
	let Some(answers) = tx.get_answers(&cls_handle) else {
		return out;
	};

	// Solved class metadata: kind flags (enum/protocol/...) + direct base classes.
	let metadata = answers.get_idx(bindings.key_to_idx(&KeyClassMetadata(cls.index())));

	// Direct base classes → `super_types` (pyrefly already omits the implicit
	// `object` from `base_class_objects`).
	let super_types: Vec<IrType> = metadata
		.as_ref()
		.map(|m| {
			m.base_class_objects()
				.iter()
				.map(|base| {
					IrType::TypeReference(TypeReference {
						identifier:   types::strip_loc(&format!("{}", base.qname())),
						generic_args: None,
					})
				})
				.collect()
		})
		.unwrap_or_default();

	// Protocol inheritance → super_traits (TraitRef form).
	let super_traits: Vec<TraitRef> = super_types
		.iter()
		.filter_map(|t| match t {
			IrType::TypeReference(r) => Some(TraitRef {
				name: r.identifier.clone(),
				args: Vec::new(),
			}),
			_ => None,
		})
		.collect();

	let is_enum = metadata.as_ref().is_some_and(|m| m.is_enum());
	let is_protocol = metadata.as_ref().is_some_and(|m| m.is_protocol());

	let generics = resolve_class_tparams(cls, &bindings, &answers)
		.as_ref()
		.and_then(|tp| types::lower_tparams(tp.as_ref()));

	// The declared (non-synthesized) fields of the class, with their properties.
	let empty_fields = ClassFields::empty();
	let class_fields = bindings.get_class_fields(cls.index()).unwrap_or(&empty_fields);

	// Resolve a single field's pyrefly ClassField answer (type + flags).
	let resolve_field = |fname: &_| {
		let idx = bindings.key_to_idx(&KeyClassField(cls.index(), Clone::clone(fname)));
		answers.get_idx(idx)
	};

	// --- enum.Enum subclass → SumType ------------------------------------
	if is_enum {
		let mut variants = Vec::new();
		for fname in class_fields.names() {
			// Enum members resolve to an enum *literal* type (`Literal[E.X]`);
			// methods/`_ignore_`/etc. do not, and are skipped.
			let field = resolve_field(fname);
			let ty = field.as_ref().map(|f| f.ty());
			let is_member = matches!(
				&ty,
				Some(PyType::Literal(lit)) if matches!(&lit.value, Lit::Enum(_))
			);
			if is_member {
				// Capture the member's assigned value from Lit::Enum.ty metadata.
				let (data, documentation) = match &ty {
					Some(PyType::Literal(lit)) => {
						if let Lit::Enum(e) = &lit.value {
							let value_ty = types::lower_type(&e.ty);
							let data = Some(SumField::Tuple(vec![value_ty]));
							let documentation = types::const_expr_from_type(&e.ty)
								.map(|c| format!("value = {c:?}"));
							(data, documentation)
						} else {
							(None, None)
						}
					}
					_ => (None, None),
				};
				variants.push(SumVariant {
					name: fname.to_string(),
					data,
					documentation,
				discriminant: None,
				});
			}
		}
		out.push((
			path.clone(),
			Entry::SumType(Symbol {
				name:          name.to_string(),
				path:          path.clone(),
				aliases:       None,
				visibility:    member_visibility(name),
				documentation: class_doc,
				deprecation:   class_deprecation,
				doc_links:     None,
				inner: ir::record::SumType::from_variants(variants),
			}),
		));
		return out;
	}

	// --- typing.Protocol → TraitDef --------------------------------------
	if is_protocol {
		let mut required_methods = Vec::new();
		let mut properties = Vec::new();
		let mut members: Vec<NudoxPath> = Vec::new();

		for fname in class_fields.names() {
			let fname_s = fname.to_string();
			let field = resolve_field(fname);
			let ty = field.as_ref().map(|f| f.ty());
			match ty {
				Some(ref t) if is_callable_type(t) => {
					let func =
						function::lower_method(handle, tx, &bindings, &answers, &fname_s, t, class_fields);
					let mut method = function_to_trait_method(&fname_s, func);
					// Phase 3 #2: method docstring + per-parameter descriptions.
					if let Some(mdoc) = catalog.member(doc_key, &fname_s) {
						method.documentation = mdoc.documentation();
						apply_param_descs(&mut method.parameters, &mdoc.params);
					}
					// Also emit a standalone Function entry for resolution.
					let mpath = member_path(path, &fname_s);
					let mut ir_func =
						function::lower_method(handle, tx, &bindings, &answers, &fname_s, t, class_fields);
					let mdoc = catalog.member(doc_key, &fname_s);
					if let Some(md) = mdoc {
						apply_param_docs(&mut ir_func, &md.params);
						apply_return_doc(&mut ir_func, md.returns.as_deref());
					}
					members.push(mpath.clone());
					out.push((
						mpath.clone(),
						Entry::Function(Symbol {
							name:          fname_s.clone(),
							path:          mpath,
							aliases:       None,
							visibility:    member_visibility(&fname_s),
							documentation: mdoc.and_then(ParsedDocstring::documentation),
							deprecation:   function::deprecation_from_flags(t)
								.or_else(|| mdoc.and_then(deprecation_from_doc)),
							doc_links:     None,
							inner:         ir_func,
						}),
					));
					required_methods.push(method);
				}
				other => {
					let irt = other.as_ref().map(types::lower_type);
					let is_class_var = field.as_ref().is_some_and(|f| f.is_class_var());
					let is_final = field.as_ref().is_some_and(|f| f.is_final());
					let is_property = field.as_ref().is_some_and(|f| {
						// Property fields carry a property-decorated type.
						f.ty().is_property_getter() || f.ty().is_cached_property()
					});
					properties.push(build_field(
						&fname_s,
						irt,
						class_fields.is_field_initialized_on_class(fname),
						is_class_var,
						is_final,
						is_property,
					));
				}
			}
		}
		let trait_def = TraitDef {
			generics,
			super_traits:       if super_traits.is_empty() { None } else { Some(super_traits) },
			associated_types:   None,
			properties:         if properties.is_empty() { None } else { Some(properties) },
			required_methods:   if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods:   None,
			required_constants: None,
			attributes:         None,
			object_safe:        None,
			sealed:             None,
			cfg:                None,
			members:            if members.is_empty() { None } else { Some(members) },
		};
		out.push((
			path.clone(),
			Entry::TraitDef(Symbol {
				name:          name.to_string(),
				path:          path.clone(),
				aliases:       None,
				visibility:    member_visibility(name),
				documentation: class_doc,
				deprecation:   class_deprecation,
				doc_links:     None,
				inner:         trait_def,
			}),
		));
		return out;
	}

	// --- plain class / @dataclass / NamedTuple / TypedDict → RecordType ---
	let mut ir_fields = Vec::new();
	let mut ir_methods = Vec::new();
	let mut ir_constructors = Vec::new();
	let mut members: Vec<NudoxPath> = Vec::new();

	for fname in class_fields.names() {
		let fname_s = fname.to_string();
		let field = resolve_field(fname);
		let ty = field.as_ref().map(|f| f.ty());

		// Nested class definitions → separate Record/Sum/Trait entries under members.
		if let Some(PyType::ClassDef(nested_cls)) = &ty {
			let nested_path = member_path(path, &fname_s);
			let nested_doc_key = format!("{doc_key}.{fname_s}");
			let nested_entries = lower_class(
				&fname_s,
				nested_cls,
				handle,
				tx,
				&bindings,
				&answers,
				&nested_path,
				catalog,
				Some(&nested_doc_key),
			);
			if !nested_entries.is_empty() {
				members.push(nested_path);
				out.extend(nested_entries);
			}
			continue;
		}

		match ty {
			Some(ref t) if is_callable_type(t) => {
				let mut func =
					function::lower_method(handle, tx, &bindings, &answers, &fname_s, t, class_fields);
				let mdoc = catalog.member(doc_key, &fname_s);
				if let Some(md) = mdoc {
					apply_param_docs(&mut func, &md.params);
					apply_return_doc(&mut func, md.returns.as_deref());
				}

				// Emit a standalone Function index entry so SymbolTable can
				// resolve `Class.method`. Keep a shape copy on the Record too.
				let mpath = member_path(path, &fname_s);
				let is_ctor = fname_s == "__init__" || fname_s == "__new__";
				if !is_ctor {
					members.push(mpath.clone());
					out.push((
						mpath.clone(),
						Entry::Function(Symbol {
							name:          fname_s.clone(),
							path:          mpath,
							aliases:       None,
							visibility:    member_visibility(&fname_s),
							documentation: mdoc.and_then(ParsedDocstring::documentation),
							deprecation:   function::deprecation_from_flags(t)
								.or_else(|| mdoc.and_then(deprecation_from_doc)),
							doc_links:     None,
							inner:         func.clone(),
						}),
					));
					// Properties also surface as fields with a `property` marker
					// so consumers that walk fields still see them.
					if t.is_property_getter() || t.is_cached_property() {
						let ret = func
							.output_parameters
							.as_ref()
							.and_then(|o| o.first())
							.and_then(|p| match p {
								IrParameter::Literal(lp) => lp.r#type.clone(),
								_ => None,
							});
						ir_fields.push(build_field(
							&fname_s,
							ret,
							false,
							false,
							function::func_flags(t).is_some_and(|f| f.has_final_decoration),
							true,
						));
					} else {
						ir_methods.push(func);
					}
				} else {
					ir_constructors.push(func);
				}
			}
			other => {
				let irt = other.as_ref().map(types::lower_type);
				let is_class_var = field.as_ref().is_some_and(|f| f.is_class_var());
				let is_final = field.as_ref().is_some_and(|f| f.is_final());
				ir_fields.push(build_field(
					&fname_s,
					irt,
					class_fields.is_field_initialized_on_class(fname),
					is_class_var,
					is_final,
					false,
				));
			}
		}
	}

	out.push((
		path.clone(),
		Entry::RecordType(Symbol {
			name:          name.to_string(),
			path:          path.clone(),
			aliases:       None,
			visibility:    member_visibility(name),
			documentation: class_doc,
			deprecation:   class_deprecation,
			doc_links:     None,
			inner:         Record {
				name:                  Some(name.to_string()),
				generics,
				fields:                ir_fields,
				call_signatures:       None,
				constructors:          if ir_constructors.is_empty() {
					None
				} else {
					Some(ir_constructors)
				},
				methods:               if ir_methods.is_empty() { None } else { Some(ir_methods) },
				index_signatures:      None,
				super_types:           if super_types.is_empty() { None } else { Some(super_types) },
				members:               if members.is_empty() { None } else { Some(members) },
				implemented_protocols: None,
			},
		}),
	));
	out
}
