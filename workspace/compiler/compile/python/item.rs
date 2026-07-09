use std::collections::HashMap as StdHashMap;

use ir::{entry::NudoxPath, function::Function, generics::GenericArg, kind::{Entry, Symbol, Visibility}, module::Module, parameter::Parameter as IrParameter, protocols::{TraitDef, TraitMethod}, record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant}, ty::{Type as IrType, TypeReference}};
use pyrefly::{alt::answers::Answers, binding::{binding::{KeyClassField, KeyClassMetadata}, bindings::Bindings}, state::state::Transaction};
use pyrefly_build::handle::Handle;
use pyrefly_python::module_name::ModuleName;
use pyrefly_types::{callable::{FuncMetadata, FunctionKind}, class::{Class, ClassFields}, literal::Lit, types::{Forallable, Type as PyType}};

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

		let entry = lower_binding(&export_name, ty, handle, tx, &bindings, &answers, &catalog);

		if let Some((path, entry)) = entry {
			entries.push((path, entry));
		}
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
) -> Option<(NudoxPath, Entry)> {
	let path =
		NudoxPath::Local(std::path::PathBuf::from(format!("{}::{}", handle.module().as_str(), name)));

	// Phase 3 #2/#3: docstring + decorator-derived shape for top-level callables.
	let doc = catalog.item(name);

	let entry = if is_function_like(ty) {
		let mut ir_func = function::lower_function(handle, tx, bindings, answers, name, ty);
		if let Some(doc) = doc {
			apply_param_docs(&mut ir_func, &doc.params);
		}
		Some(Entry::Function(Symbol {
			name:          name.to_string(),
			path:          path.clone(),
			aliases:       None,
			visibility:    member_visibility(name),
			documentation: doc.and_then(ParsedDocstring::documentation),
			inner:         ir_func,
		}))
	} else {
		match ty {
			// Class definition — extract fields and methods
			pyrefly_types::types::Type::ClassDef(cls) => {
				lower_class(name, cls, handle, tx, bindings, answers, &path, catalog)
			}

			// Type alias
			pyrefly_types::types::Type::TypeAlias(_) | pyrefly_types::types::Type::UntypedAlias(_) => {
				Some(Entry::TypeAlias(Symbol {
					name:          name.to_string(),
					path:          path.clone(),
					aliases:       None,
					visibility:    member_visibility(name),
					documentation: doc.and_then(ParsedDocstring::documentation),
					inner:         types::lower_type(ty),
				}))
			}

			// Module-level constant (immutable)
			_ if name.starts_with('_') && name != "__init__" => None,

			// Everything else — treat as a constant or variable
			_ => Some(Entry::Constant(Symbol {
				name:          name.to_string(),
				path:          path.clone(),
				aliases:       None,
				visibility:    member_visibility(name),
				documentation: doc.and_then(ParsedDocstring::documentation),
				inner:         (),
			})),
		}
	};

	entry.map(|e| (path, e))
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
fn is_callable_type(ty: &PyType) -> bool {
	matches!(
		ty,
		PyType::Function(_)
			| PyType::Callable(_)
			| PyType::BoundMethod(_)
			| PyType::Overload(_)
			| PyType::Forall(_)
	)
}

/// Build a `KnownField` for a class data attribute. `has_default` marks fields
/// initialized in the class body (`x: int = 0`) as optional.
fn build_field(name: &str, ty: Option<IrType>, has_default: bool) -> Field {
	Field::Known(KnownField {
		key:           FieldKey::Ident(name.to_string()),
		r#type:        ty.map(Box::new),
		default_value: None,
		attributes:    FieldAttributes {
			// Python instance attributes are mutable by default.
			is_mutable:  true,
			is_optional: has_default,
			is_static:   false,
			decorators:  Vec::new(),
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
		generics: None,
		attributes: func.attributes,
		documentation: None,
		receiver: func.receiver,
		// Protocol bodies in stubs are `...`; default impls are a later phase.
		has_default_implementation: false,
	}
}

fn lower_class(
	name: &str,
	cls: &Class,
	handle: &Handle,
	tx: &Transaction,
	_bindings: &Bindings,
	_answers: &Answers,
	path: &NudoxPath,
	catalog: &DocCatalog,
) -> Option<Entry> {
	// Class-level docstring (shared by the Record / SumType / TraitDef shapes).
	let class_doc = catalog.item(name).and_then(ParsedDocstring::documentation);
	// Resolve bindings/answers for the class's *defining* module, so imported
	// classes resolve against the right tables. Mirrors `Transaction::
	// get_class_fields`, which keys a fresh `Handle` off the class's qname.
	let cls_handle =
		Handle::new(cls.module_name(), cls.module_path().clone(), handle.sys_info().clone());
	let bindings = tx.get_bindings(&cls_handle)?;
	let answers = tx.get_answers(&cls_handle)?;

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

	let is_enum = metadata.as_ref().is_some_and(|m| m.is_enum());
	let is_protocol = metadata.as_ref().is_some_and(|m| m.is_protocol());

	// The declared (non-synthesized) fields of the class, with their properties.
	let empty_fields = ClassFields::empty();
	let class_fields = bindings.get_class_fields(cls.index()).unwrap_or(&empty_fields);

	// Resolve a single field's pyrefly-inferred `Type` (the resolved attribute
	// or method type) via the per-field `KeyClassField` answer.
	let resolve_ty = |fname: &_| -> Option<PyType> {
		let idx = bindings.key_to_idx(&KeyClassField(cls.index(), Clone::clone(fname)));
		answers.get_idx(idx).map(|field| field.ty())
	};

	// --- enum.Enum subclass → SumType ------------------------------------
	if is_enum {
		let mut variants = Vec::new();
		for fname in class_fields.names() {
			// Enum members resolve to an enum *literal* type (`Literal[E.X]`);
			// methods/`_ignore_`/etc. do not, and are skipped.
			let is_member = matches!(
					resolve_ty(fname),
					Some(PyType::Literal(lit)) if matches!(&lit.value, Lit::Enum(_))
			);
			if is_member {
				variants.push(SumVariant {
					name:          fname.to_string(),
					data:          None,
					documentation: None,
				});
			}
		}
		return Some(Entry::SumType(Symbol {
			name:          name.to_string(),
			path:          path.clone(),
			aliases:       None,
			visibility:    member_visibility(name),
			documentation: class_doc,
			inner:         variants,
		}));
	}

	// --- typing.Protocol → TraitDef --------------------------------------
	if is_protocol {
		let mut required_methods = Vec::new();
		let mut properties = Vec::new();
		for fname in class_fields.names() {
			let fname_s = fname.to_string();
			match resolve_ty(fname) {
				Some(ty) if is_callable_type(&ty) => {
					let func =
						function::lower_method(handle, tx, &bindings, &answers, &fname_s, &ty, class_fields);
					let mut method = function_to_trait_method(&fname_s, func);
					// Phase 3 #2: method docstring + per-parameter descriptions.
					if let Some(mdoc) = catalog.member(name, &fname_s) {
						method.documentation = mdoc.documentation();
						apply_param_descs(&mut method.parameters, &mdoc.params);
					}
					required_methods.push(method);
				}
				other => {
					let irt = other.as_ref().map(types::lower_type);
					properties.push(build_field(
						&fname_s,
						irt,
						class_fields.is_field_initialized_on_class(fname),
					));
				}
			}
		}
		let trait_def = TraitDef {
			generics:           None,
			super_traits:       None,
			associated_types:   None,
			properties:         if properties.is_empty() { None } else { Some(properties) },
			required_methods:   if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods:   None,
			required_constants: None,
			attributes:         None,
			members:            None,
		};
		return Some(Entry::TraitDef(Symbol {
			name:          name.to_string(),
			path:          path.clone(),
			aliases:       None,
			visibility:    member_visibility(name),
			documentation: class_doc,
			inner:         trait_def,
		}));
	}

	// --- plain class / @dataclass / NamedTuple / TypedDict → RecordType ---
	let mut ir_fields = Vec::new();
	let mut ir_methods = Vec::new();
	let mut ir_constructors = Vec::new();

	for fname in class_fields.names() {
		let fname_s = fname.to_string();
		match resolve_ty(fname) {
			Some(ty) if is_callable_type(&ty) => {
				let mut func =
					function::lower_method(handle, tx, &bindings, &answers, &fname_s, &ty, class_fields);
				// Phase 3 #2: `Record` methods are bare `Function`s with no
				// documentation slot, so only per-parameter descriptions can be
				// attached here (the method summary has nowhere to live).
				if let Some(mdoc) = catalog.member(name, &fname_s) {
					apply_param_docs(&mut func, &mdoc.params);
				}
				if fname_s == "__init__" || fname_s == "__new__" {
					ir_constructors.push(func);
				} else {
					ir_methods.push(func);
				}
			}
			other => {
				let irt = other.as_ref().map(types::lower_type);
				ir_fields.push(build_field(
					&fname_s,
					irt,
					class_fields.is_field_initialized_on_class(fname),
				));
			}
		}
	}

	Some(Entry::RecordType(Symbol {
		name:          name.to_string(),
		path:          path.clone(),
		aliases:       None,
		visibility:    member_visibility(name),
		documentation: class_doc,
		inner:         Record {
			name:                  Some(name.to_string()),
			generics:              None,
			fields:                ir_fields,
			call_signatures:       None,
			constructors:          if ir_constructors.is_empty() { None } else { Some(ir_constructors) },
			methods:               if ir_methods.is_empty() { None } else { Some(ir_methods) },
			index_signatures:      None,
			super_types:           if super_types.is_empty() { None } else { Some(super_types) },
			members:               None,
			implemented_protocols: None,
		},
	}))
}
