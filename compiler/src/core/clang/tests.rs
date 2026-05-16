use std::path::PathBuf;

use clang::Clang;
use ir::{entry::Entry, function::TypeLinkPosition, kind::Visibility, parameter::Parameter, primitives::{Primitive, Width}, ty::Type};
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

fn check(file: &str) {
	let files = vec![test_file(file)];
	let entries = parse_files(files);

	let mut settings = insta::Settings::clone_current();
	settings.add_filter(env!("CARGO_MANIFEST_DIR"), "$$CARGO_MANIFEST_DIR");
	settings.bind(|| insta::assert_debug_snapshot!(file, &entries));
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
fn test_simple04_c_metadata_and_types() {
	let entries = parse_files(vec![test_file("simple04")]);

	let point = entries
		.iter()
		.find(|entry| entry.name() == "Point" && matches!(entry, Entry::RecordType(_)))
		.unwrap();

	let Entry::RecordType(point) = point else { unreachable!() };

	assert_eq!(point.documentation.as_deref(), Some("A documented point."));
	assert_eq!(point.inner.fields.len(), 2);

	let scale = entries.iter().find(|entry| entry.name() == "scale_point").unwrap();

	let Entry::Function(scale) = scale else { unreachable!() };
	assert_eq!(scale.documentation.as_deref(), Some("Scale a point by a scalar."));

	let inputs = scale.inner.input_parameters.as_ref().unwrap();
	assert_eq!(inputs.len(), 2);

	let Parameter::Literal(first) = &inputs[0] else { unreachable!() };

	assert!(matches!(first.r#type, Some(Type::RawPointer { is_mutable: false, .. })));
	assert!(scale.inner.output_parameters.is_some());
	assert!(scale.inner.type_links.as_ref().is_some_and(|links| {
		links.iter().any(|link| {
					matches!(&link.position, TypeLinkPosition::Input { name: Some(name), .. } if name == "point")
				}) && links.iter().any(|link| matches!(link.position, TypeLinkPosition::Output { .. }))
	}));

	let scalar = entries.iter().find(|entry| entry.name() == "Scalar").unwrap();

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

	let module = entries.iter().find(|entry| entry.name() == "math").unwrap();
	assert!(matches!(module, Entry::Module(_)));

	let box_record = entries.iter().find(|entry| entry.name() == "Box").unwrap();
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

	let hidden = box_record
		.inner
		.fields
		.iter()
		.find_map(|field| match field {
			ir::record::Field::Known(field) if matches!(&field.key, ir::record::FieldKey::Ident(name) if name == "hidden") => Some(field),
			_ => None,
		})
		.unwrap();

	assert_eq!(hidden.visibility, Some(Visibility::Private));

	let static_field = box_record
		.inner
		.fields
		.iter()
		.find_map(|field| match field {
			ir::record::Field::Known(field) if matches!(&field.key, ir::record::FieldKey::Ident(name) if name == "created") => Some(field),
			_ => None,
		})
		.unwrap();

	assert!(static_field.attributes.is_static);

	let get = entries.iter().find(|entry| entry.name() == "get").unwrap();

	let Entry::Function(get) = get else { unreachable!() };

	assert_eq!(get.documentation.as_deref(), Some("Return the stored value."));
	assert!(
		matches!(&get.path, ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::Box::get"))
	);

	let reset = entries.iter().find(|entry| entry.name() == "reset").unwrap();
	let Entry::Function(reset) = reset else { unreachable!() };

	assert!(reset.inner.overloads.as_ref().is_some_and(|overloads| overloads.len() == 1));

	let add = entries.iter().find(|entry| entry.name() == "add").unwrap();
	let Entry::Function(add_symbol) = add else { unreachable!() };

	assert!(add_symbol.inner.type_links.is_none());
	assert!(
		matches!(add.path(), ir::entry::NudoxPath::Local(path) if path == std::path::Path::new("math::add"))
	);
}

#[test]
#[serial]
fn test_compile_commands_project_entrypoint() {
	let ir = ClangProject::generate_ir(test_dir("simple04")).unwrap();
	let index = ir.index().into_index();

	assert!(index.entries_by_path.values().any(|entry| entry.name() == "scale_point"));
	assert!(index.entries_by_path.values().any(|entry| entry.name() == "Point"));
}
