use std::collections::HashMap;

use clang::{Clang, CompilationDatabase, Entity, EntityKind, Index, TranslationUnit, Type as ClangType};
use ir::{entry::Entry, function::Function, kind::Kind, parameter::Parameter, record::{Field, Record, RecordKind}, ty::Type};

use super::ClangError;
use crate::error::PackageError;

pub(crate) struct ClangParser {
	inner: ClangParserInner,
	files: Vec<std::path::PathBuf>,
	ir:    IRBuilder,
}

impl ClangParser {
	pub fn new(clang: Clang, db: CompilationDatabase) -> Self {
		let inner = ClangParserInner::new(clang, |c| Index::new(c, false, false));

		let files: Vec<_> = db
			.get_all_compile_commands()
			.get_commands()
			.iter()
			.map(|cmd| cmd.get_directory().join(cmd.get_filename()))
			.collect();

		ClangParser { inner, files, ir: IRBuilder::new() }
	}

	#[cfg(test)]
	pub fn from_files(clang: Clang, files: Vec<std::path::PathBuf>) -> Self {
		let inner = ClangParserInner::new(clang, |c| Index::new(c, false, false));
		ClangParser { inner, files, ir: IRBuilder::new() }
	}

	pub fn parse(mut self) -> Result<Vec<Entry>, PackageError> {
		for file_entrypoint in self.files {
			let parser = self.inner.borrow_index().parser(&file_entrypoint);

			let tu = parser.parse().map_err(|e| ClangError::SourceError(file_entrypoint, e))?;

			self.ir.handle_translation_unit(tu)?;
		}

		Ok(self.ir.finish())
	}
}

struct IRBuilder {
	entries: Vec<Entry>,
	next_id: i64,
}

impl IRBuilder {
	fn new() -> Self { IRBuilder { entries: Vec::new(), next_id: 0 } }

	fn handle_translation_unit(&mut self, unit: TranslationUnit<'_>) -> Result<(), PackageError> {
		let new_entries = TUHandler::new(self, &unit).handle()?;

		self.entries.extend(new_entries);

		Ok(())
	}

	fn finish(self) -> Vec<Entry> { self.entries }

	fn new_id(&mut self) -> i64 {
		let new_id = self.next_id;
		self.next_id += 1;
		new_id
	}
}

struct TUHandler<'ir, 'tu> {
	ir:      &'ir mut IRBuilder,
	tu:      &'tu TranslationUnit<'tu>,
	entries: Vec<Entry>,
	map:     HashMap<Entity<'tu>, i64>,
}

impl<'ir, 'tu> TUHandler<'ir, 'tu> {
	fn new(ir: &'ir mut IRBuilder, tu: &'tu TranslationUnit<'tu>) -> Self {
		TUHandler { ir, tu, entries: Vec::new(), map: HashMap::new() }
	}

	fn handle(mut self) -> Result<Vec<Entry>, PackageError> {
		self.handle_root(self.tu.get_entity())?;

		Ok(self.entries)
	}

	fn handle_root(&mut self, root: Entity<'tu>) -> Result<(), PackageError> {
		for child in root.get_children() {
			self.handle_entity(child)?;
		}

		Ok(())
	}

	fn handle_entity(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		let kind = entity.get_kind();
		match kind {
			EntityKind::StructDecl => self.handle_struct(entity)?,

			EntityKind::ClassDecl => self.handle_class(entity)?,
			EntityKind::EnumDecl => self.handle_enum(entity)?,
			EntityKind::FunctionDecl => self.handle_function(entity)?,
			EntityKind::TypedefDecl => self.handle_typedef(entity)?,
			_ => {}
		}

		Ok(())
	}

	fn handle_function(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::FunctionDecl);

		let (id, name) = self.basic_info(entity);

		let (input_parameters, output_parameters) =
			self.handle_function_params(entity.get_children())?;

		let generics = None;
		let attributes = None;
		let implemented = true;

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::Function(Function {
				input_parameters,
				output_parameters,
				type_links: None,
				attributes,
				generics,
				name,
				implemented,
				visibility: None,
			}),
			visibility: None,
			documentation: None,
			members: None,
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_struct(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::StructDecl);

		let (id, name) = self.basic_info(entity);

		let fields = self.handle_struct_fields(entity.get_children())?;

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::RecordType(Record {
				name: Some(name),
				generics: None,
				kind: RecordKind::Named,
				fields,
				visibility: None,
			}),
			visibility: None,
			documentation: None,
			members: None,
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_class(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::ClassDecl);

		let (id, name) = self.basic_info(entity);

		let fields = self.handle_struct_fields(entity.get_children())?;

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::RecordType(Record {
				name: Some(name),
				generics: None,
				kind: RecordKind::Named,
				fields,
				visibility: None,
			}),
			visibility: None,
			documentation: None,
			members: None,
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_enum(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::EnumDecl);

		let (id, name) = self.basic_info(entity);

		let variants = self.handle_enum_variants(entity.get_children())?;

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::SumType(variants),
			visibility: None,
			documentation: None,
			members: None,
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_typedef(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::TypedefDecl);

		// TODO: check things using entity child
		// to avoid double-declaring structs

		let (id, name) = self.basic_info(entity);

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::TypeAlias(Type::Infer), // TODO
			visibility: None,
			documentation: None,
			members: None,
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_struct_fields(
		&mut self,
		children: Vec<Entity<'tu>>,
	) -> Result<Option<Vec<Field>>, PackageError> {
		let mut fields = vec![];

		for child in children {
			match child.get_kind() {
				EntityKind::FieldDecl => {
					let field_name = child.get_name().unwrap_or_default();
					let field_type = child.get_type().map(|t| self.resolve_type(t));
					fields.push(Field {
						name:          Some(field_name),
						type_entry_id: None,
						ty:            field_type.map(Box::new),
						default_value: None,
						attributes:    None,
						visibility:    None,
					});
				}
				EntityKind::CompoundStmt => {}
				_ => {}
			}
		}

		Ok(if fields.is_empty() { None } else { Some(fields) })
	}

	fn handle_enum_variants(
		&mut self,
		children: Vec<Entity<'tu>>,
	) -> Result<Vec<ir::record::SumVariant>, PackageError> {
		let mut variants = vec![];

		for child in children {
			match child.get_kind() {
				EntityKind::EnumConstantDecl => {
					let variant_name = child.get_name().unwrap_or_default();
					variants.push(ir::record::SumVariant { name: variant_name, types: None });
				}
				_ => {}
			}
		}

		Ok(variants)
	}

	fn resolve_type(&self, ty: ClangType) -> Type {
		let kind = ty.get_kind();

		match kind {
			clang::TypeKind::Void => return Type::Primitive(ir::primitives::Primitive::Null),
			clang::TypeKind::Bool => return Type::Primitive(ir::primitives::Primitive::Bool(None)),
			clang::TypeKind::CharS
			| clang::TypeKind::CharU
			| clang::TypeKind::SChar
			| clang::TypeKind::UChar
			| clang::TypeKind::WChar => return Type::Primitive(ir::primitives::Primitive::Char(None)),
			clang::TypeKind::Short
			| clang::TypeKind::UShort
			| clang::TypeKind::Int
			| clang::TypeKind::UInt
			| clang::TypeKind::Long
			| clang::TypeKind::ULong
			| clang::TypeKind::LongLong
			| clang::TypeKind::ULongLong
			| clang::TypeKind::Int128
			| clang::TypeKind::UInt128 => return Type::Primitive(ir::primitives::Primitive::Int(None)),
			clang::TypeKind::Half | clang::TypeKind::Float | clang::TypeKind::Float128 => {
				return Type::Primitive(ir::primitives::Primitive::Float(None));
			}
			clang::TypeKind::Double | clang::TypeKind::LongDouble => {
				return Type::Primitive(ir::primitives::Primitive::Double(None));
			}
			clang::TypeKind::Pointer => {
				if let Some(pointee) = ty.get_pointee_type() {
					return Type::RawPointer {
						is_mutable: false,
						ty:         Box::new(self.resolve_type(pointee)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::LValueReference => {
				if let Some(referent) = ty.get_element_type() {
					return Type::BorrowedRef {
						lifetime:   None,
						is_mutable: false,
						ty:         Box::new(self.resolve_type(referent)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::RValueReference => {
				if let Some(referent) = ty.get_element_type() {
					return Type::BorrowedRef {
						lifetime:   None,
						is_mutable: true,
						ty:         Box::new(self.resolve_type(referent)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::ConstantArray => {
				if let Some(element) = ty.get_element_type() {
					return Type::Array { ty: Box::new(self.resolve_type(element)), length: 0 };
				}
				return Type::Infer;
			}
			clang::TypeKind::FunctionNoPrototype | clang::TypeKind::FunctionPrototype => {
				return Type::Primitive(ir::primitives::Primitive::Data(None));
			}
			_ => return Type::Infer,
		}
	}

	fn handle_function_params(
		&mut self,
		children: Vec<Entity<'tu>>,
	) -> Result<(Option<Vec<Parameter>>, Option<Vec<Parameter>>), PackageError> {
		let mut input_params = vec![];

		for child in children {
			match child.get_kind() {
				EntityKind::ParmDecl => {
					input_params.push(Parameter {
						name:          child.get_name().unwrap_or_default(),
						ty:            None,
						attributes:    None,
						default_value: None,
						description:   None,
					});
				}
				EntityKind::CompoundStmt => {}  // Ignore function bodies
				EntityKind::UnexposedAttr => {} // Ignore attributes
				_ => {}
			}
		}

		let input_params = (!input_params.is_empty()).then_some(input_params);

		Ok((input_params, None))
	}

	fn basic_info(&mut self, entity: Entity<'tu>) -> (i64, String) {
		(self.ir.new_id(), entity.get_name().unwrap_or_default())
	}

	fn insert_entry(&mut self, entry: Entry, entity: Entity<'tu>) {
		self.map.insert(entity, entry.id);
		self.entries.push(entry);
	}
}

#[ouroboros::self_referencing]
struct ClangParserInner {
	clang: Clang,
	#[borrows(clang)]
	#[covariant]
	index: Index<'this>,
}
