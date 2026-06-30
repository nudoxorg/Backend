use ir::{entry::NudoxPath, function::Function, kind::{Entry, Symbol, Visibility}, module::Module, parameter::Parameter as IrParameter, protocols::{TraitDef, TraitMethod}, record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant}, ty::{Type as IrType, TypeReference}};
use pyrefly::{alt::answers::Answers, binding::{binding::{KeyClassField, KeyClassMetadata}, bindings::Bindings}, state::state::Transaction};
use pyrefly_build::handle::Handle;
use pyrefly_types::{class::{Class, ClassFields}, literal::Lit, types::Type as PyType};

use super::{function, types};

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

	// Module entry
	let nudox_path = module_path.clone();
	entries.push((
		nudox_path.clone(),
		Entry::Module(Symbol {
			name:          module_name.clone(),
			path:          nudox_path.clone(),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
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

		let entry = lower_binding(&export_name, ty, handle, tx, &bindings, &answers);

		if let Some((path, entry)) = entry {
			entries.push((path, entry));
		}
	}

	entries
}

fn lower_binding(
	name: &str,
	ty: &pyrefly_types::types::Type,
	handle: &Handle,
	tx: &Transaction,
	bindings: &Bindings,
	answers: &Answers,
) -> Option<(NudoxPath, Entry)> {
	let path =
		NudoxPath::Local(std::path::PathBuf::from(format!("{}::{}", handle.module().as_str(), name)));

	let entry = match ty {
		// Class definition — extract fields and methods
		pyrefly_types::types::Type::ClassDef(cls) => {
			lower_class(name, cls, handle, tx, bindings, answers, &path)
		}

		// Function definition
		pyrefly_types::types::Type::Function(_)
		| pyrefly_types::types::Type::Callable(_)
		| pyrefly_types::types::Type::BoundMethod(_) => {
			let ir_func = function::lower_function(handle, tx, bindings, answers, name, ty);
			Some(Entry::Function(Symbol {
				name:          name.to_string(),
				path:          path.clone(),
				aliases:       None,
				visibility:    Visibility::Public,
				documentation: None,
				inner:         ir_func,
			}))
		}

		// Type alias
		pyrefly_types::types::Type::TypeAlias(_) | pyrefly_types::types::Type::UntypedAlias(_) => {
			Some(Entry::TypeAlias(Symbol {
				name:          name.to_string(),
				path:          path.clone(),
				aliases:       None,
				visibility:    Visibility::Public,
				documentation: None,
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
			visibility:    Visibility::Public,
			documentation: None,
			inner:         (),
		})),
	};

	entry.map(|e| (path, e))
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
) -> Option<Entry> {
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
			visibility:    Visibility::Public,
			documentation: None,
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
					required_methods.push(function_to_trait_method(&fname_s, func));
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
			visibility:    Visibility::Public,
			documentation: None,
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
				let func =
					function::lower_method(handle, tx, &bindings, &answers, &fname_s, &ty, class_fields);
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
		visibility:    Visibility::Public,
		documentation: None,
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
