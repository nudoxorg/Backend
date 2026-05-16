use std::collections::HashMap;

use clang::{Clang, CompilationDatabase, Entity, EntityKind, Index, TranslationUnit, Type as ClangType};
use ir::{entry::{Entry, NudoxPath}, function::Function, kind::{Symbol, Visibility}, parameter::{LiteralParameter, Parameter}, primitives::{Primitive, Width}, record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant}, ty::Type};

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
}

impl IRBuilder {
	fn new() -> Self { IRBuilder { entries: Vec::new() } }

	fn handle_translation_unit(&mut self, unit: TranslationUnit<'_>) -> Result<(), PackageError> {
		let new_entries = TUHandler::new(self, &unit).handle()?;

		self.entries.extend(new_entries);

		Ok(())
	}

	fn finish(self) -> Vec<Entry> { self.entries }
}

struct TUHandler<'ir, 'tu> {
	_ir:     &'ir mut IRBuilder,
	tu:      &'tu TranslationUnit<'tu>,
	entries: Vec<Entry>,
	map:     HashMap<Entity<'tu>, NudoxPath>,
}

impl<'ir, 'tu> TUHandler<'ir, 'tu> {
	fn new(_ir: &'ir mut IRBuilder, tu: &'tu TranslationUnit<'tu>) -> Self {
		TUHandler { _ir, tu, entries: Vec::new(), map: HashMap::new() }
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

		let name = self.entity_name(entity);

		let (input_parameters, output_parameters) =
			self.handle_function_params(entity.get_children())?;

		let generics = None;
		let attributes = None;
		let implemented = true;

		let entry = Entry::Function(Symbol {
			name:          name.clone(),
			path:          NudoxPath::Local(name.clone().into()),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         Function {
				input_parameters,
				output_parameters,
				type_links: None,
				attributes,
				generics,
				receiver: None,
				implemented,
				overloads: None,
				members: None,
				implemented_protocols: None,
			},
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_struct(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::StructDecl);

		let name = self.entity_name(entity);

		let fields = self.handle_struct_fields(entity.get_children())?;

		let entry = Entry::RecordType(Symbol {
			name:          name.clone(),
			path:          NudoxPath::Local(name.clone().into()),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         Record {
				name: Some(name),
				generics: None,
				fields,
				call_signatures: None,
				methods: None,
				constructors: None,
				index_signatures: None,
				super_types: None,
				members: None,
				implemented_protocols: None,
			},
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_class(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::ClassDecl);

		let name = self.entity_name(entity);

		let fields = self.handle_struct_fields(entity.get_children())?;

		let entry = Entry::RecordType(Symbol {
			name:          name.clone(),
			path:          NudoxPath::Local(name.clone().into()),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         Record {
				name: Some(name),
				generics: None,
				fields,
				call_signatures: None,
				constructors: None,
				methods: None,
				index_signatures: None,
				super_types: None,
				members: None,
				implemented_protocols: None,
			},
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_enum(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::EnumDecl);

		let name = self.entity_name(entity);

		let variants = self.handle_enum_variants(entity.get_children())?;

		let entry = Entry::SumType(Symbol {
			name:          name.clone(),
			path:          NudoxPath::Local(name.into()),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         variants,
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_typedef(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::TypedefDecl);

		// TODO: check things using entity child
		// to avoid double-declaring structs

		let name = self.entity_name(entity);

		let entry = Entry::TypeAlias(Symbol {
			name:          name.clone(),
			path:          NudoxPath::Local(name.into()),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: None,
			inner:         Type::Infer,
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_struct_fields(
		&mut self,
		children: Vec<Entity<'tu>>,
	) -> Result<Vec<Field>, PackageError> {
		let mut fields = vec![];

		for child in children {
			match child.get_kind() {
				EntityKind::FieldDecl => {
					let field_name = child.get_name().unwrap_or_default();
					let field_type = child.get_type().map(|t| self.resolve_type(t));
					fields.push(Field::Known(KnownField {
						key:           FieldKey::Ident(field_name),
						r#type:        field_type.map(Box::new),
						default_value: None,
						attributes:    FieldAttributes {
							decorators:  vec![],
							is_mutable:  true,
							is_optional: false,
							is_static:   false,
						},
						visibility:    None,
						documentation: None,
					}));
				}
				EntityKind::CompoundStmt => {}
				_ => {}
			}
		}

		Ok(fields)
	}

	fn handle_enum_variants(
		&mut self,
		children: Vec<Entity<'tu>>,
	) -> Result<Vec<SumVariant>, PackageError> {
		let mut variants = vec![];

		for child in children {
			match child.get_kind() {
				EntityKind::EnumConstantDecl => {
					let variant_name = child.get_name().unwrap_or_default();
					variants.push(SumVariant {
						name:          variant_name,
						data:          None,
						documentation: None,
					});
				}
				_ => {}
			}
		}

		Ok(variants)
	}

	fn resolve_type(&self, ty: ClangType) -> Type {
		let kind = ty.get_kind();

		match kind {
			clang::TypeKind::Void => return Type::Void,
			clang::TypeKind::Bool => return Type::Primitive(Primitive::Bool),
			clang::TypeKind::CharS
			| clang::TypeKind::CharU
			| clang::TypeKind::SChar
			| clang::TypeKind::UChar
			| clang::TypeKind::WChar => return Type::Primitive(Primitive::Char),

			clang::TypeKind::Short => return Type::Primitive(Primitive::Int(Width::W16)),

			clang::TypeKind::UShort => return Type::Primitive(Primitive::UInt(Width::W16)),

			clang::TypeKind::Int => return Type::Primitive(Primitive::Int(Width::W32)),

			clang::TypeKind::UInt => return Type::Primitive(Primitive::UInt(Width::W32)),

			clang::TypeKind::Long => return Type::Primitive(Primitive::Int(Width::W64)),

			clang::TypeKind::ULong => return Type::Primitive(Primitive::UInt(Width::W64)),

			clang::TypeKind::LongLong => return Type::Primitive(Primitive::Int(Width::W64)),

			clang::TypeKind::ULongLong => return Type::Primitive(Primitive::UInt(Width::W64)),

			clang::TypeKind::Int128 => return Type::Primitive(Primitive::Int(Width::W128)),

			clang::TypeKind::UInt128 => return Type::Primitive(Primitive::UInt(Width::W128)),

			clang::TypeKind::Half => return Type::Primitive(Primitive::Float(Width::W16)),
			clang::TypeKind::Float => return Type::Primitive(Primitive::Float(Width::W32)),

			clang::TypeKind::Float128 => return Type::Primitive(Primitive::Float(Width::W128)),

			clang::TypeKind::Double => return Type::Primitive(Primitive::Float(Width::W64)),

			clang::TypeKind::LongDouble => return Type::Primitive(Primitive::Float(Width::W80)),

			clang::TypeKind::Pointer => {
				if let Some(pointee) = ty.get_pointee_type() {
					return Type::RawPointer {
						is_mutable: false,
						r#type:     Box::new(self.resolve_type(pointee)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::LValueReference => {
				if let Some(referent) = ty.get_element_type() {
					return Type::BorrowedRef {
						lifetime:   None,
						is_mutable: false,
						r#type:     Box::new(self.resolve_type(referent)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::RValueReference => {
				if let Some(referent) = ty.get_element_type() {
					return Type::BorrowedRef {
						lifetime:   None,
						is_mutable: true,
						r#type:     Box::new(self.resolve_type(referent)),
					};
				}
				return Type::Infer;
			}
			clang::TypeKind::ConstantArray => {
				if let Some(element) = ty.get_element_type() {
					return Type::Array { r#type: Box::new(self.resolve_type(element)), length: 0 };
				}
				return Type::Infer;
			}
			clang::TypeKind::FunctionNoPrototype | clang::TypeKind::FunctionPrototype => {
				unimplemented!() // TODO
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
					input_params.push(Parameter::Literal(LiteralParameter {
						name:          child.get_name().unwrap_or_default(),
						r#type:        None,
						attributes:    None,
						default_value: None,
						description:   None,
					}));
				}
				EntityKind::CompoundStmt => {}  // Ignore function bodies
				EntityKind::UnexposedAttr => {} // Ignore attributes
				_ => {}
			}
		}

		let input_params = (!input_params.is_empty()).then_some(input_params);

		Ok((input_params, None))
	}

	fn entity_name(&mut self, entity: Entity<'tu>) -> String { entity.get_name().unwrap_or_default() }

	fn insert_entry(&mut self, entry: Entry, entity: Entity<'tu>) {
		self.map.insert(entity, entry.path().clone());
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
