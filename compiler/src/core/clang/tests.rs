use std::path::PathBuf;

use clang::Clang;
use ir::{entry::Entry, function::Attribute as FnAttribute, generics::Kind, kind::Visibility, parameter::{Parameter, ParameterAttribute}, primitives::{Primitive, Width}, protocols::ReceiverKind, ty::Type};
use serial_test::serial;

use crate::core::clang::{ClangProject, parser::ClangParser};

fn parse_files(files: Vec<PathBuf>) -> Vec<Entry> {
	let parser = ClangParser::from_files(Clang::new().unwrap(), files);
	parser.parse().expect("failed to parse files")
}

fn test_dir(name: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/core/clang/tests").join(name)
}

fn test_file(name: &str) -> PathBuf { test_dir(name).join(name).with_extension("c") }

fn test_file_with_extension(name: &str, extension: &str) -> PathBuf {
	test_dir(name).join(name).with_extension(extension)
}

fn check(file: &str) { check_with_extension(file, "c"); }

fn check_with_extension(file: &str, extension: &str) {
	let files = vec![test_file(file)];
	let files =
		if extension == "c" { files } else { vec![test_file_with_extension(file, extension)] };
	let entries = parse_files(files);

	let mut settings = insta::Settings::clone_current();
	settings.add_filter(env!("CARGO_MANIFEST_DIR"), "$$CARGO_MANIFEST_DIR");
	settings.bind(|| insta::assert_debug_snapshot!(file, &entries));
}

fn find_entry<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
	entries
		.iter()
		.find(|entry| entry.name() == name)
		.unwrap_or_else(|| panic!("missing entry `{name}`"))
}

fn known_field<'a>(record: &'a ir::record::Record, name: &str) -> &'a ir::record::KnownField {
	record
		.fields
		.iter()
		.find_map(|field| match field {
			ir::record::Field::Known(field)
				if matches!(&field.key, ir::record::FieldKey::Ident(field_name) if field_name == name) =>
			{
				Some(field)
			}
			_ => None,
		})
		.unwrap_or_else(|| panic!("missing field `{name}`"))
}

#[test]
#[serial]
fn test_simple01_functions() { check("simple01"); }

#[test]
#[serial]
fn test_simple02_struct() { check("simple02"); }

#[test]
#[serial]
fn test_simple03_mixed() { check("simple03"); }

#[test]
#[serial]
fn test_simple04_c_metadata_snapshot() { check("simple04"); }

#[test]
#[serial]
fn test_simple05_cpp_namespace_template_snapshot() { check_with_extension("simple05", "cpp"); }

#[test]
#[serial]
fn test_simple06_cpp_advanced_templates_snapshot() { check_with_extension("simple06", "cpp"); }

#[test]
#[serial]
fn test_simple07_cpp_type_shapes_snapshot() { check_with_extension("simple07", "cpp"); }

#[test]
#[serial]
fn test_simple04_c_metadata_and_types() {
	let entries = parse_files(vec![test_file("simple04")]);

	let point = entries
		.iter()
		.find(|entry| entry.name() == "Point" && matches!(entry, Entry::RecordType(_)))
		.unwrap();

	let Entry::RecordType(point) = point else { unreachable!() };

	assert_eq!(point.documentation.as_deref(), Some("A documented point."));
	assert_eq!(point.inner.fields.len(), 2);

	let scale = find_entry(&entries, "scale_point");

	let Entry::Function(scale) = scale else { unreachable!() };
	assert_eq!(scale.documentation.as_deref(), Some("Scale a point by a scalar."));

	let inputs = scale.inner.input_parameters.as_ref().unwrap();
	assert_eq!(inputs.len(), 2);

	let Parameter::Literal(first) = &inputs[0] else { unreachable!() };

	assert!(matches!(first.r#type, Some(Type::RawPointer { is_mutable: false, .. })));
	assert!(scale.inner.output_parameters.is_some());
	// assert!(scale.inner.type_links.as_ref().is_some_and(|links| {
	// 	links.iter().any(|link| {
	// 				matches!(&link.position, TypeLinkPosition::Input { name: Some(name), .. }
	// if name == "point") 			}) && links.iter().any(|link| matches!(link.position,
	// TypeLinkPosition::Output { .. })) }));

	let scalar = find_entry(&entries, "Scalar");

	let Entry::TypeAlias(scalar) = scalar else { unreachable!() };

	assert_eq!(scalar.documentation.as_deref(), Some("Numeric scalar alias."));
	assert_eq!(scalar.inner, Type::Primitive(Primitive::Float(Width::W64)));

	assert!(
		entries
			.iter()
			.any(|entry| matches!(entry, Entry::UnionType(symbol) if symbol.name == "Number"))
	);
	assert!(
		entries.iter().any(|entry| matches!(entry, Entry::Constant(symbol) if symbol.name == "answer"))
	);
	assert!(
		entries
			.iter()
			.any(|entry| matches!(entry, Entry::Variable(symbol) if symbol.name == "mutable_counter"))
	);
}

#[test]
#[serial]
fn test_simple05_cpp_namespaces_records_and_templates() {
	let entries = parse_files(vec![test_file_with_extension("simple05", "cpp")]);

	let module = find_entry(&entries, "math");
	assert!(matches!(module, Entry::Module(_)));

	let box_record = find_entry(&entries, "Box");
	let Entry::RecordType(box_record) = box_record else { unreachable!() };

	assert_eq!(box_record.documentation.as_deref(), Some("Generic fixed-size box."));
	assert_eq!(box_record.inner.generics.as_ref().unwrap().params.len(), 2);
	assert_eq!(box_record.inner.super_types.as_ref().unwrap().len(), 1);

	assert!(box_record.inner.constructors.as_ref().is_some_and(|ctors| !ctors.is_empty()));
	assert!(box_record.inner.methods.as_ref().is_some_and(|methods| methods.len() >= 2));

	let members = box_record.inner.members.as_ref().unwrap();

	assert!(members.iter().any(|path| matches!(path, ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::Box::constructor"))));
	assert!(members.iter().any(|path| matches!(path, ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::Box::get"))));
	assert!(members.iter().any(|path| matches!(path, ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::Box::reset"))));

	let hidden = known_field(&box_record.inner, "hidden");

	assert_eq!(hidden.visibility, Some(Visibility::Private));

	let static_field = known_field(&box_record.inner, "created");

	assert!(static_field.attributes.is_static);

	let get = find_entry(&entries, "get");

	let Entry::Function(get) = get else { unreachable!() };

	assert_eq!(get.documentation.as_deref(), Some("Return the stored value."));
	assert!(
		matches!(&get.path, ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::Box::get"))
	);

	let reset = find_entry(&entries, "reset");
	let Entry::Function(reset) = reset else { unreachable!() };

	assert!(reset.inner.overloads.as_ref().is_some_and(|overloads| overloads.len() == 1));

	let add = find_entry(&entries, "add");
	let Entry::Function(add_symbol) = add else { unreachable!() };

	assert!(add_symbol.inner.type_links.is_none());
	assert!(
		matches!(add.path(), ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::add"))
	);
}

#[test]
#[serial]
fn test_simple06_template_class_parameters() {
	let entries = parse_files(vec![test_file_with_extension("simple06", "cpp")]);
	let adapter = find_entry(&entries, "Adapter");
	let Entry::RecordType(adapter) = adapter else { unreachable!() };
	let generics = adapter.inner.generics.as_ref().unwrap();

	assert_eq!(generics.params.len(), 3);
	assert!(
		matches!(&generics.params[0], Parameter::Type(param) if matches!(param.kind, Kind::Arrow(_, _)))
	);
	assert!(
		matches!(&generics.params[1], Parameter::Type(param) if param.name.as_deref() == Some("T"))
	);
	assert!(matches!(&generics.params[2], Parameter::Const(param) if param.name == "N"));

	let box_field = known_field(&adapter.inner, "box");
	assert!(
		matches!(box_field.r#type.as_deref(), Some(Type::TypeReference(reference)) if reference.identifier.starts_with("template-parameter"))
	);
}

#[test]
#[serial]
fn test_simple06_template_function_and_overloads() {
	let entries = parse_files(vec![test_file_with_extension("simple06", "cpp")]);

	let identity = find_entry(&entries, "identity");
	let Entry::Function(identity) = identity else { unreachable!() };
	let generics = identity.inner.generics.as_ref().unwrap();
	assert_eq!(generics.params.len(), 1);
	assert!(
		matches!(&generics.params[0], Parameter::Type(param) if param.name.as_deref() == Some("T"))
	);
	assert_eq!(identity.documentation.as_deref(), Some("Return the input unchanged."));

	let choose = find_entry(&entries, "choose");
	let Entry::Function(choose) = choose else { unreachable!() };
	assert!(choose.inner.generics.as_ref().is_some_and(|generics| generics.params.len() == 1));
	assert!(choose.inner.overloads.as_ref().is_some_and(|overloads| overloads.len() == 1));
}

#[test]
#[serial]
fn test_simple06_template_alias_resolution() {
	let entries = parse_files(vec![test_file_with_extension("simple06", "cpp")]);
	let alias = find_entry(&entries, "IntHolder");
	let Entry::TypeAlias(alias) = alias else { unreachable!() };

	assert!(
		matches!(&alias.inner, Type::TypeReference(reference) if reference.identifier == "Holder<int>")
	);
}

#[test]
#[serial]
fn test_simple07_enum_and_global_bindings() {
	let entries = parse_files(vec![test_file_with_extension("simple07", "cpp")]);

	let mode = find_entry(&entries, "Mode");
	let Entry::SumType(mode) = mode else { unreachable!() };
	assert_eq!(mode.documentation.as_deref(), Some("Runtime mode."));
	assert_eq!(mode.inner.len(), 2);
	assert_eq!(mode.inner[0].name, "Idle");
	assert_eq!(mode.inner[0].documentation.as_deref(), Some("Waiting for work."));
	assert_eq!(mode.inner[1].name, "Busy");
	assert_eq!(mode.inner[1].documentation, None);

	let internal_counter = find_entry(&entries, "internal_counter");
	let Entry::Variable(internal_counter) = internal_counter else { unreachable!() };
	assert_eq!(internal_counter.visibility, Visibility::Internal);

	let max_items = find_entry(&entries, "max_items");
	let Entry::Constant(_) = max_items else { unreachable!() };

	let fixed_values = find_entry(&entries, "fixed_values");
	let Entry::Variable(_) = fixed_values else { unreachable!() };
}

#[test]
#[serial]
fn test_simple07_function_and_member_type_shapes() {
	let entries = parse_files(vec![test_file_with_extension("simple07", "cpp")]);

	let callback = find_entry(&entries, "Callback");
	let Entry::TypeAlias(callback) = callback else { unreachable!() };
	assert!(
		matches!(callback.inner, Type::RawPointer { ref r#type, .. } if matches!(r#type.as_ref(), Type::FunctionPointer(_)))
	);

	let apply = find_entry(&entries, "apply");
	let Entry::Function(apply) = apply else { unreachable!() };
	assert!(
		apply.inner.attributes.as_ref().is_some_and(|attrs| attrs.contains(&FnAttribute::Variadic))
	);
	let params = apply.inner.input_parameters.as_ref().unwrap();
	assert_eq!(params.len(), 4);
	let Parameter::Literal(bias) = &params[1] else { unreachable!() };
	assert!(
		bias.attributes.as_ref().is_some_and(|attrs| attrs.contains(&ParameterAttribute::Borrowing))
	);
	let Parameter::Literal(scratch) = &params[2] else { unreachable!() };
	assert!(
		scratch.attributes.as_ref().is_some_and(|attrs| attrs.contains(&ParameterAttribute::Inout))
	);

	let convertible = find_entry(&entries, "Convertible");
	let Entry::RecordType(convertible) = convertible else { unreachable!() };
	assert_eq!(known_field(&convertible.inner, "secret").visibility, Some(Visibility::Protected));
	let constructors = convertible.inner.constructors.as_ref().unwrap();
	assert!(constructors.iter().any(|method| { method.receiver == Some(ReceiverKind::Owned) }));
	assert!(convertible.inner.methods.as_ref().is_some_and(|methods| {
		methods.iter().any(|method| method.receiver == Some(ReceiverKind::SharedRef))
	}));
}

#[test]
#[serial]
fn test_simple07_pointer_constness_and_nesting() {
	let entries = parse_files(vec![test_file_with_extension("simple07", "cpp")]);
	let pointer_shapes = find_entry(&entries, "pointer_shapes");
	let Entry::Function(pointer_shapes) = pointer_shapes else { unreachable!() };
	let params = pointer_shapes.inner.input_parameters.as_ref().unwrap();
	assert_eq!(params.len(), 4);

	let Parameter::Literal(readonly_ptr) = &params[0] else { unreachable!() };
	assert!(matches!(readonly_ptr.r#type, Some(Type::RawPointer { is_mutable: false, .. })));

	let Parameter::Literal(mutable_ptr) = &params[1] else { unreachable!() };
	assert!(matches!(mutable_ptr.r#type, Some(Type::RawPointer { is_mutable: true, .. })));

	let Parameter::Literal(fixed_ptr) = &params[2] else { unreachable!() };
	assert!(matches!(fixed_ptr.r#type, Some(Type::RawPointer { is_mutable: true, .. })));

	let Parameter::Literal(nested_ptr) = &params[3] else { unreachable!() };
	assert!(matches!(
		nested_ptr.r#type,
		Some(Type::RawPointer { is_mutable: true, ref r#type })
			if matches!(r#type.as_ref(), Type::RawPointer { is_mutable: false, .. })
	));
}

#[test]
#[serial]
fn test_compile_commands_project_entrypoint() {
	let ir = ClangProject::generate_ir(test_dir("simple04")).unwrap();
	let index = ir.index().into_index();

	assert!(index.entries_by_path.values().any(|entry| entry.name() == "scale_point"));
	assert!(index.entries_by_path.values().any(|entry| entry.name() == "Point"));
}
