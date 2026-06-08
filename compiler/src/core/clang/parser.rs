use std::{collections::{HashMap, HashSet}, path::PathBuf};

use clang::{Accessibility, Clang, CompilationDatabase, Entity, EntityKind, Index, StorageClass, TranslationUnit, Type as ClangType, TypeKind};
use ir::{entry::{Entry, NudoxPath}, function::{Attribute as FnAttribute, Function}, generics::{Generics, Kind, TypeExpr, Variance}, kind::{Symbol, Visibility}, module::Module, parameter::{ConstParam, LiteralParameter, Parameter, ParameterAttribute, TypeParam, TypeParamOrigin}, primitives::{Primitive, Width}, protocols::ReceiverKind, record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant}, ty::{FunctionPointer, Type, TypeReference}};

use super::ClangError;
use crate::error::PackageError;

pub(crate) struct ClangParser {
	inner:    ClangParserInner,
	commands: Vec<CompileCommand>,
	ir:       IRBuilder,
}

struct CompileCommand {
	file:      std::path::PathBuf,
	arguments: Vec<String>,
}

impl ClangParser {
	pub fn new(clang: Clang, db: CompilationDatabase, db_dir: std::path::PathBuf) -> Self {
		let inner = ClangParserInner::new(clang, |c| Index::new(c, false, false));

		let commands: Vec<_> = db
			.get_all_compile_commands()
			.get_commands()
			.iter()
			.map(|cmd| {
				let directory = normalize_compile_path(&db_dir, cmd.get_directory());
				let file = normalize_compile_path(&directory, cmd.get_filename());
				let arguments = parser_arguments(cmd.get_arguments(), &file);
				CompileCommand { file, arguments }
			})
			.collect();

		ClangParser { inner, commands, ir: IRBuilder::new() }
	}

	#[cfg(test)]
	pub fn from_files(clang: Clang, files: Vec<std::path::PathBuf>) -> Self {
		let inner = ClangParserInner::new(clang, |c| Index::new(c, false, false));
		let commands =
			files.into_iter().map(|file| CompileCommand { file, arguments: vec![] }).collect();

		ClangParser { inner, commands, ir: IRBuilder::new() }
	}

	pub fn parse(mut self) -> Result<Vec<Entry>, PackageError> {
		for command in self.commands {
			let mut parser = self.inner.borrow_index().parser(&command.file);

			let tu = parser
				.arguments(&command.arguments)
				.parse()
				.map_err(|e| ClangError::SourceError(command.file, e))?;

			self.ir.handle_translation_unit(tu)?;
		}

		Ok(self.ir.finish())
	}
}

fn normalize_compile_path(base: &std::path::Path, path: std::path::PathBuf) -> std::path::PathBuf {
	if path.is_absolute() { path } else { base.join(path) }
}

fn parser_arguments(args: Vec<String>, file: &std::path::Path) -> Vec<String> {
	let file_name = file.file_name().and_then(|name| name.to_str());
	let file_str = file.to_str();

	args
		.into_iter()
		.enumerate()
		.filter_map(|(idx, arg)| {
			if idx == 0 {
				return None;
			}

			let is_source = file_str.is_some_and(|path| arg == path)
				|| file_name.is_some_and(|name| arg == name)
				|| arg.ends_with(".c")
				|| arg.ends_with(".cc")
				|| arg.ends_with(".cpp")
				|| arg.ends_with(".cxx");

			(!is_source).then_some(arg)
		})
		.collect()
}

struct IRBuilder {
	entries: Vec<Entry>,
}

impl IRBuilder {
	fn new() -> Self { IRBuilder { entries: Vec::new() } }

	fn handle_translation_unit(&mut self, unit: TranslationUnit<'_>) -> Result<(), PackageError> {
		let new_entries = TUHandler::new(&unit).handle()?;

		self.entries.extend(new_entries);

		Ok(())
	}

	fn finish(mut self) -> Vec<Entry> {
		let type_name_to_id = build_type_name_index(&self.entries);

		for entry in &mut self.entries {
			match entry {
				Entry::Function(symbol) => {
					symbol.inner.type_links = build_type_links_for_function(&symbol.inner, &type_name_to_id);
				}
				Entry::RecordType(symbol) => {
					if let Some(methods) = &mut symbol.inner.methods {
						for method in methods {
							method.type_links = build_type_links_for_function(&method, &type_name_to_id);
						}
					}
					if let Some(constructors) = &mut symbol.inner.constructors {
						for constructor in constructors {
							constructor.type_links =
								build_type_links_for_function(&constructor, &type_name_to_id);
						}
					}
				}
				_ => {}
			}
		}

		self.entries.dedup_by_key(|entry| entry.path().clone());

		self.entries
	}
}

fn build_type_name_index(entries: &[Entry]) -> HashMap<String, NudoxPath> {
	let mut index = HashMap::new();

	for entry in entries {
		let path = entry.path().clone();
		index.entry(entry.name().to_owned()).or_insert(path.clone());

		// TODO: FQN + NudoxPath rework
		if let NudoxPath::Local(path) = entry.path() {
			let path_string = path.to_string_lossy().replace('/', "::");
			index.entry(path_string).or_insert(entry.path().clone());
		}
	}

	index
}

fn build_type_links_for_function(
	function: &Function,
	type_name_to_id: &HashMap<String, NudoxPath>,
) -> Option<HashMap<String, i64>> {
	let mut links = HashMap::new();

	collect_parameter_type_links(
		true,
		function.input_parameters.as_deref(),
		type_name_to_id,
		&mut links,
	);
	collect_parameter_type_links(
		false,
		function.output_parameters.as_deref(),
		type_name_to_id,
		&mut links,
	);

	if !links.is_empty() { Some(links) } else { None }
}

// TODO: re-implement type link support
fn collect_parameter_type_links(
	_is_input: bool,
	parameters: Option<&[Parameter]>,
	type_name_to_id: &HashMap<String, NudoxPath>,
	_links: &mut HashMap<String, i64>,
) {
	let Some(parameters) = parameters else { return };

	for (_idx, parameter) in parameters.iter().enumerate() {
		let Parameter::Literal(literal) = parameter else { continue };

		let Some(ty) = &literal.r#type else { continue };

		if let Some(_target) = resolve_ir_type_to_entry_path(ty, type_name_to_id) {
			let _name = if !literal.name.is_empty() { Some(literal.name.clone()) } else { None };

			// let position = if is_input {
			// 	TypeLinkPosition::Input { index: idx, name }
			// } else {
			// 	TypeLinkPosition::Output { index: idx, name }
			// };

			// links.push(TypeLink { position, target, source: None });
		}
	}
}

fn resolve_ir_type_to_entry_path(
	ty: &Type,
	type_name_to_id: &HashMap<String, NudoxPath>,
) -> Option<NudoxPath> {
	match ty {
		Type::TypeReference(reference) => {
			resolve_type_reference(&reference.identifier, type_name_to_id)
		}
		Type::RawPointer { r#type, .. }
		| Type::BorrowedRef { r#type, .. }
		| Type::Array { r#type, .. }
		| Type::Slice(r#type)
		| Type::Variadic(r#type) => resolve_ir_type_to_entry_path(r#type, type_name_to_id),
		Type::FunctionPointer(function) => function
			.inputs
			.as_deref()
			.into_iter()
			.flatten()
			.chain(function.outputs.as_deref().into_iter().flatten())
			.find_map(|parameter| match parameter {
				Parameter::Literal(literal) => {
					literal.r#type.as_ref().and_then(|ty| resolve_ir_type_to_entry_path(ty, type_name_to_id))
				}
				_ => None,
			}),
		_ => None,
	}
}

fn resolve_type_reference(
	identifier: &str,
	type_name_to_id: &HashMap<String, NudoxPath>,
) -> Option<NudoxPath> {
	let normalized = normalize_type_identifier(identifier);

	if let Some(id) = type_name_to_id.get(normalized.as_str()) {
		return Some(id.clone());
	}

	type_name_to_id.iter().find_map(|(name, id)| {
		if name.ends_with(normalized.as_str()) || normalized.ends_with(name.as_str()) {
			Some(id.clone())
		} else {
			None
		}
	})
}

fn normalize_type_identifier(identifier: &str) -> String {
	let without_template_args = identifier.split('<').next().unwrap_or(identifier).trim();
	without_template_args.trim_start_matches("const ").replace('/', "::")
}

struct TUHandler<'tu> {
	tu:      &'tu TranslationUnit<'tu>,
	entries: Vec<Entry>,
	map:     HashMap<Entity<'tu>, NudoxPath>,
	scopes:  Vec<String>,
}

impl<'tu> TUHandler<'tu> {
	fn new(tu: &'tu TranslationUnit<'tu>) -> Self {
		TUHandler { tu, entries: Vec::new(), map: HashMap::new(), scopes: Vec::new() }
	}

	fn handle(mut self) -> Result<Vec<Entry>, PackageError> {
		self.handle_root(self.tu.get_entity())?;

		Ok(self.entries)
	}

	fn handle_root(&mut self, root: Entity<'tu>) -> Result<(), PackageError> {
		let children = root.get_children();
		let mut seen_functions = HashSet::new();

		for child in &children {
			if Self::is_free_function_kind(child.get_kind()) {
				let name = child.get_name().unwrap_or_default();

				if seen_functions.insert(name.clone()) {
					let overloads = children
						.iter()
						.copied()
						.filter(|candidate| {
							Self::is_free_function_kind(candidate.get_kind())
								&& candidate.get_name().is_some_and(|c| c == name)
						})
						.collect::<Vec<_>>();

					self.handle_function_group(name, &overloads)?;
				}
			} else {
				self.handle_entity(*child)?;
			}
		}

		Ok(())
	}

	fn is_free_function_kind(kind: EntityKind) -> bool {
		matches!(kind, EntityKind::FunctionDecl | EntityKind::FunctionTemplate)
	}

	fn handle_entity(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		if !entity.is_in_main_file() || entity.is_invalid_declaration() {
			return Ok(());
		}

		match entity.get_kind() {
			EntityKind::StructDecl => self.handle_struct(entity)?,
			EntityKind::UnionDecl => self.handle_union(entity)?,
			EntityKind::ClassDecl => self.handle_class(entity)?,
			EntityKind::ClassTemplate => self.handle_class(entity)?,
			EntityKind::EnumDecl => self.handle_enum(entity)?,
			EntityKind::FunctionDecl => self.handle_function(entity)?,
			EntityKind::FunctionTemplate => self.handle_function(entity)?,
			EntityKind::TypedefDecl => self.handle_typedef(entity)?,
			EntityKind::TypeAliasDecl => self.handle_typedef(entity)?,
			EntityKind::Namespace => self.handle_namespace(entity)?,
			EntityKind::LinkageSpec => self.handle_root(entity)?,
			EntityKind::VarDecl => self.handle_variable(entity)?,
			EntityKind::MacroDefinition => self.handle_macro(entity)?,
			_ => {}
		}

		Ok(())
	}

	fn handle_function(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		let name = self.entity_name(entity);
		let entry = self.free_function_symbol(name, &[entity]).map(Entry::Function)?;

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_function_group(
		&mut self,
		name: String,
		overloads: &[Entity<'tu>],
	) -> Result<(), PackageError> {
		if let Some(primary) = overloads.first().copied() {
			let entry = self.free_function_symbol(name, overloads).map(Entry::Function)?;
			self.insert_entry(entry, primary);
		}

		Ok(())
	}

	fn free_function_symbol(
		&self,
		name: String,
		overloads: &[Entity<'tu>],
	) -> Result<Symbol<Function>, PackageError> {
		let path = self.path_for_name(&name);

		let (primary_idx, primary) = overloads
			.iter()
			.copied()
			.enumerate()
			.rfind(|(_, candidate)| candidate.is_definition())
			.unwrap_or_else(|| {
				let idx = overloads.len().saturating_sub(1);
				(idx, overloads[idx])
			});

		let mut function = self.function_from_entity(primary, None)?;

		let extra_overloads = overloads
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, overload)| {
				let overload_name =
					overload.get_name().or_else(|| overload.get_display_name()).unwrap_or_default();

				self.free_function_symbol(overload_name, &[*overload]).map(|sym| sym.inner)
			})
			.collect::<Result<Vec<_>, _>>()?;

		function.overloads = if !extra_overloads.is_empty() { Some(extra_overloads) } else { None };

		Ok(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(primary),
			documentation: self.documentation(primary),
			inner: function,
		})
	}

	fn handle_struct(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::StructDecl);

		self.handle_record(entity)
	}

	fn handle_union(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::UnionDecl);

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);

		let fields = self.handle_struct_fields(entity.get_children())?;

		let types = fields
			.into_iter()
			.filter_map(|field| match field {
				Field::Known(known) => known.r#type.map(|ty| *ty),
				_ => None,
			})
			.collect();

		let entry = Entry::UnionType(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: types,
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_record(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert!(matches!(
			entity.get_kind(),
			EntityKind::StructDecl | EntityKind::ClassDecl | EntityKind::ClassTemplate
		));

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);
		let children = entity.get_children();

		let fields = self.handle_struct_fields(children.clone())?;
		let super_types = self.handle_base_specifiers(children.clone());
		let generics = self.handle_template_params(children.clone());

		let (methods, constructors, member_entries) =
			self.in_scope(name.clone(), |this| -> Result<_, PackageError> {
				let methods = this.handle_methods(children.clone())?;
				let constructors = this.handle_constructors(children.clone())?;
				let member_entries = this.handle_record_member_entries(children)?;

				Ok((methods, constructors, member_entries))
			})?;

		let members = if !member_entries.is_empty() {
			Some(member_entries.iter().map(|entry| entry.path().clone()).collect())
		} else {
			None
		};

		self.entries.extend(member_entries);

		let entry = Entry::RecordType(Symbol {
			name: name.clone(),
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: Record {
				name: Some(name.clone()),
				generics,
				fields,
				call_signatures: None,
				methods,
				constructors,
				index_signatures: None,
				super_types,
				members,
				implemented_protocols: None,
			},
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_class(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert!(matches!(entity.get_kind(), EntityKind::ClassDecl | EntityKind::ClassTemplate));

		self.handle_record(entity)
	}

	fn handle_enum(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::EnumDecl);

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);

		let variants = self.handle_enum_variants(entity.get_children())?;

		let entry = Entry::SumType(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: variants,
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_typedef(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert!(matches!(entity.get_kind(), EntityKind::TypedefDecl | EntityKind::TypeAliasDecl));

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);

		let inner = entity
			.get_typedef_underlying_type()
			.or_else(|| entity.get_type())
			.map(|ty| self.resolve_type(ty))
			.unwrap_or(Type::Infer);

		let entry = Entry::TypeAlias(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner,
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_namespace(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::Namespace);

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);
		let start = self.entries.len();

		self.in_scope(name.clone(), |this| this.handle_root(entity))?;

		let members = self.entries[start..].iter().map(|entry| entry.path().clone()).collect();
		let entry = Entry::Module(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: Module { members: Some(members) },
		});

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_variable(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::VarDecl);

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);

		// let binding = ValueBinding {
		// 	r#type:        entity.get_type().map(|ty| self.resolve_type(ty)),
		// 	default_value: None,
		// 	attributes:    ValueBindingAttributes {
		// 		is_mutable: !entity.get_type().is_some_and(|ty| ty.is_const_qualified()),
		// 		is_static: matches!(entity.get_storage_class(),
		// Some(StorageClass::Static)), 		storage:
		// entity.get_storage_class().map(|storage| format!("{storage:?}")),
		// 		..ValueBindingAttributes::default()
		// 	},
		// };

		let symbol = Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: (),
		};

		let entry = if entity.get_type().is_some_and(|ty| ty.is_const_qualified()) {
			Entry::Constant(symbol)
		} else {
			Entry::Variable(symbol)
		};

		self.insert_entry(entry, entity);

		Ok(())
	}

	fn handle_macro(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::MacroDefinition);

		if entity.is_builtin_macro() {
			return Ok(());
		}

		let name = self.entity_name(entity);
		let path = self.path_for_name(&name);

		let entry = Entry::Macro(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(entity),
			documentation: self.documentation(entity),
			inner: (),
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
				EntityKind::FieldDecl | EntityKind::VarDecl => {
					let field_name = child.get_name().unwrap_or_default();

					let field_type = child.get_type().map(|t| self.resolve_type(t));

					let is_static = child.get_kind() == EntityKind::VarDecl;

					fields.push(Field::Known(KnownField {
						key:           FieldKey::Ident(field_name),
						r#type:        field_type.map(Box::new),
						default_value: None,
						attributes:    FieldAttributes {
							decorators: vec![],
							is_mutable: child.is_mutable()
								|| child.get_type().is_some_and(|ty| !ty.is_const_qualified()),
							is_optional: false,
							is_static,
						},
						visibility:    Some(self.visibility(child)),
						documentation: self.documentation(child),
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
		let mut seen_docs = HashSet::new();

		for child in children {
			match child.get_kind() {
				EntityKind::EnumConstantDecl => {
					let variant_name = child.get_name().unwrap_or_default();
					let documentation = self.documentation(child).filter(|doc| seen_docs.insert(doc.clone()));

					variants.push(SumVariant { name: variant_name, data: None, documentation });
				}
				_ => {}
			}
		}

		Ok(variants)
	}

	fn resolve_type(&self, ty: ClangType) -> Type {
		let kind = ty.get_kind();

		match kind {
			TypeKind::Void => Type::Void,
			TypeKind::Bool => Type::Primitive(Primitive::Bool),
			TypeKind::CharS
			| TypeKind::CharU
			| TypeKind::SChar
			| TypeKind::UChar
			| TypeKind::WChar
			| TypeKind::Char16
			| TypeKind::Char32 => Type::Primitive(Primitive::Char),

			TypeKind::Short | TypeKind::Int | TypeKind::Long | TypeKind::LongLong | TypeKind::Int128 => {
				Type::Primitive(Primitive::Int(clang_type_to_width(ty)))
			}

			TypeKind::UShort
			| TypeKind::UInt
			| TypeKind::ULong
			| TypeKind::ULongLong
			| TypeKind::UInt128 => Type::Primitive(Primitive::UInt(clang_type_to_width(ty))),

			TypeKind::Half
			| TypeKind::Float16
			| TypeKind::Float
			| TypeKind::Float128
			| TypeKind::Double
			| TypeKind::LongDouble => Type::Primitive(Primitive::Float(clang_type_to_width(ty))),

			TypeKind::Pointer => {
				if let Some(pointee) = ty.get_pointee_type() {
					Type::RawPointer {
						is_mutable: !pointee.is_const_qualified(),
						r#type:     Box::new(self.resolve_type(pointee)),
					}
				} else {
					Type::Infer
				}
			}
			TypeKind::LValueReference => {
				if let Some(referent) = ty.get_pointee_type().or_else(|| ty.get_element_type()) {
					Type::BorrowedRef {
						lifetime:   None,
						is_mutable: !referent.is_const_qualified(),
						r#type:     Box::new(self.resolve_type(referent)),
					}
				} else {
					Type::Infer
				}
			}
			TypeKind::RValueReference => {
				if let Some(referent) = ty.get_pointee_type().or_else(|| ty.get_element_type()) {
					Type::BorrowedRef {
						lifetime:   None,
						is_mutable: true,
						r#type:     Box::new(self.resolve_type(referent)),
					}
				} else {
					Type::Infer
				}
			}
			TypeKind::ConstantArray | TypeKind::VariableArray | TypeKind::DependentSizedArray => {
				if let Some(element) = ty.get_element_type() {
					Type::Array {
						r#type: Box::new(self.resolve_type(element)),
						length: ty.get_size().unwrap_or_default(),
					}
				} else {
					Type::Infer
				}
			}
			TypeKind::IncompleteArray => {
				if let Some(element) = ty.get_element_type() {
					Type::Slice(Box::new(self.resolve_type(element)))
				} else {
					Type::Infer
				}
			}
			TypeKind::FunctionNoPrototype | TypeKind::FunctionPrototype => {
				let inputs = ty.get_argument_types().and_then(|args| self.parameters_from_types(args));
				let outputs = ty
					.get_result_type()
					.map(|result| self.output_parameters_from_type(self.resolve_type(result)));

				Type::FunctionPointer(FunctionPointer { inputs, outputs, attributes: None })
			}
			TypeKind::Elaborated => {
				ty.get_elaborated_type().map(|inner| self.resolve_type(inner)).unwrap_or(Type::Infer)
			}
			TypeKind::Typedef => {
				let identifier = ty.get_typedef_name().unwrap_or_else(|| ty.get_display_name());

				Type::TypeReference(self.type_reference(identifier, None))
			}
			TypeKind::Record | TypeKind::Enum => {
				let identifier = ty
					.get_declaration()
					.and_then(|entity| entity.get_display_name())
					.unwrap_or_else(|| ty.get_display_name());

				Type::TypeReference(self.type_reference(identifier, self.generic_args_from_type(ty)))
			}
			TypeKind::Auto | TypeKind::Dependent => Type::Infer,
			TypeKind::Nullptr => Type::TypeReference(TypeReference {
				identifier:   "nullptr_t".to_owned(),
				generic_args: None,
			}),
			TypeKind::Unexposed
				if let canonical = ty.get_canonical_type()
					&& canonical != ty =>
			{
				self.resolve_type(canonical)
			}
			_ => {
				let name = ty.get_display_name();
				if !name.is_empty() {
					Type::TypeReference(self.type_reference(name, None))
				} else {
					Type::Infer
				}
			}
		}
	}

	fn type_reference(
		&self,
		identifier: String,
		generic_args: Option<Vec<ir::generics::GenericArg>>,
	) -> TypeReference {
		TypeReference { identifier, generic_args }
	}

	fn handle_function_signature(
		&self,
		entity: Entity<'tu>,
	) -> Result<(Option<Vec<Parameter>>, Option<Vec<Parameter>>), PackageError> {
		let mut input_params = vec![];

		if let Some(args) = entity.get_arguments() {
			for arg in args {
				let ty = arg.get_type().map(|ty| self.resolve_type(ty));
				let attributes = ty.as_ref().and_then(Self::parameter_attributes);
				input_params.push(Parameter::Literal(LiteralParameter {
					name: arg.get_name().unwrap_or_default(),
					r#type: ty,
					attributes,
					default_value: None,
					description: self.documentation(arg),
				}));
			}
		}

		if entity.is_variadic() {
			input_params.push(Parameter::Literal(LiteralParameter {
				name:          "...".to_owned(),
				r#type:        Some(Type::Variadic(Box::new(Type::Any))),
				attributes:    Some(vec![ParameterAttribute::Variadic]),
				default_value: None,
				description:   None,
			}));
		}

		let input_params = (!input_params.is_empty()).then_some(input_params);
		let output_params =
			entity.get_result_type().map(|ty| self.output_parameters_from_type(self.resolve_type(ty)));

		Ok((input_params, output_params))
	}

	fn handle_methods(
		&self,
		children: Vec<Entity<'tu>>,
	) -> Result<Option<Vec<Function>>, PackageError> {
		let methods: Vec<_> = children
			.into_iter()
			.filter(|child| {
				matches!(
					child.get_kind(),
					EntityKind::Method | EntityKind::ConversionFunction | EntityKind::FunctionTemplate
				)
			})
			.map(|method| {
				self
					.member_function_symbol(self.member_function_name(method), &[method])
					.map(|sym| sym.inner)
			})
			.collect::<Result<_, _>>()?;

		if !methods.is_empty() { Ok(Some(methods)) } else { Ok(None) }
	}

	fn handle_record_member_entries(
		&self,
		children: Vec<Entity<'tu>>,
	) -> Result<Vec<Entry>, PackageError> {
		let mut entries = Vec::new();
		let mut seen_methods = HashSet::new();

		for child in &children {
			match child.get_kind() {
				EntityKind::Method | EntityKind::ConversionFunction | EntityKind::FunctionTemplate => {
					let name = self.member_function_name(*child);

					if seen_methods.insert(name.clone()) {
						let overloads = children
							.iter()
							.copied()
							.filter(|candidate| {
								matches!(
									candidate.get_kind(),
									EntityKind::Method
										| EntityKind::ConversionFunction
										| EntityKind::FunctionTemplate
								) && self.member_function_name(*candidate) == name
							})
							.collect::<Vec<_>>();

						entries.push(self.member_function_entry(name, &overloads)?);
					}
				}
				EntityKind::Constructor => {
					if seen_methods.insert("constructor".to_owned()) {
						let constructors = children
							.iter()
							.copied()
							.filter(|candidate| candidate.get_kind() == EntityKind::Constructor)
							.collect::<Vec<_>>();

						entries.push(self.member_function_entry("constructor".to_owned(), &constructors)?);
					}
				}
				EntityKind::Destructor => {
					if seen_methods.insert("destructor".to_owned()) {
						let destructors = children
							.iter()
							.copied()
							.filter(|candidate| candidate.get_kind() == EntityKind::Destructor)
							.collect::<Vec<_>>();

						entries.push(self.member_function_entry("destructor".to_owned(), &destructors)?);
					}
				}
				_ => {}
			}
		}

		Ok(entries)
	}

	fn member_function_symbol(
		&self,
		name: String,
		overloads: &[Entity<'tu>],
	) -> Result<Symbol<Function>, PackageError> {
		let path = self.path_for_name(&name);

		let primary_idx = overloads
			.iter()
			.rposition(|candidate| candidate.is_definition())
			.unwrap_or(overloads.len().saturating_sub(1));

		let primary = overloads[primary_idx];
		let receiver = self.receiver_for_member(primary);

		let mut function = self.function_from_entity(primary, receiver)?;

		let extra_overloads = overloads
			.iter()
			.enumerate()
			.filter(|(idx, _)| *idx != primary_idx)
			.map(|(_, overload)| {
				let overload_name = self.member_function_name(*overload);
				self.member_function_symbol(overload_name, &[*overload]).map(|sym| sym.inner)
			})
			.collect::<Result<Vec<_>, _>>()?;

		function.overloads = if !extra_overloads.is_empty() { Some(extra_overloads) } else { None };

		Ok(Symbol {
			name,
			path,
			aliases: None,
			visibility: self.visibility(primary),
			documentation: self.documentation(primary),
			inner: function,
		})
	}

	fn member_function_entry(
		&self,
		name: String,
		overloads: &[Entity<'tu>],
	) -> Result<Entry, PackageError> {
		self.member_function_symbol(name, overloads).map(Entry::Function)
	}

	fn handle_constructors(
		&self,
		children: Vec<Entity<'tu>>,
	) -> Result<Option<Vec<Function>>, PackageError> {
		let constructors: Vec<_> = children
			.into_iter()
			.filter(|child| matches!(child.get_kind(), EntityKind::Constructor | EntityKind::Destructor))
			.map(|ctor| {
				self.member_function_symbol(self.member_function_name(ctor), &[ctor]).map(|sym| sym.inner)
			})
			.collect::<Result<_, _>>()?;

		Ok((!constructors.is_empty()).then_some(constructors))
	}

	fn member_function_name(&self, entity: Entity<'tu>) -> String {
		match entity.get_kind() {
			EntityKind::Constructor => "constructor".to_owned(),
			EntityKind::Destructor => "destructor".to_owned(),
			_ => entity.get_name().or_else(|| entity.get_display_name()).unwrap_or_default(),
		}
	}

	fn receiver_for_member(&self, entity: Entity<'tu>) -> Option<ReceiverKind> {
		match entity.get_kind() {
			EntityKind::Constructor => Some(ReceiverKind::Static),
			EntityKind::Destructor => Some(ReceiverKind::Owned),
			_ if entity.is_static_method() => Some(ReceiverKind::Static),
			_ if entity.is_const_method() => Some(ReceiverKind::SharedRef),
			_ => Some(ReceiverKind::MutRef),
		}
	}

	fn function_from_entity(
		&self,
		entity: Entity<'tu>,
		receiver: Option<ReceiverKind>,
	) -> Result<Function, PackageError> {
		let (input_parameters, output_parameters) = self.handle_function_signature(entity)?;

		Ok(Function {
			input_parameters,
			output_parameters,
			type_links: None,
			attributes: self.handle_function_attributes(entity),
			generics: self.handle_template_params(entity.get_children()),
			receiver,
			implemented: entity.is_definition(),
			overloads: None,
			members: None,
			implemented_protocols: None,
			body: None, // TODO
		})
	}

	fn handle_base_specifiers(&self, children: Vec<Entity<'tu>>) -> Option<Vec<Type>> {
		let bases: Vec<_> = children
			.into_iter()
			.filter(|child| child.get_kind() == EntityKind::BaseSpecifier)
			.filter_map(|child| child.get_type().map(|ty| self.resolve_type(ty)))
			.collect();

		if !bases.is_empty() { Some(bases) } else { None }
	}

	fn handle_template_params(&self, children: Vec<Entity<'tu>>) -> Option<Generics> {
		let mut params = vec![];
		for child in children {
			match child.get_kind() {
				EntityKind::TemplateTypeParameter => {
					params.push(Parameter::Type(TypeParam {
						name:         child.get_name(),
						kind:         Kind::Type,
						variance:     Variance::Invariant,
						default_type: None,
						params:       None,
						origin:       TypeParamOrigin::Free,
					}));
				}
				EntityKind::NonTypeTemplateParameter => {
					let name = child.get_name().unwrap_or_default();

					let ty =
						child.get_type().map(|ty| ty.get_display_name()).unwrap_or_else(|| "auto".to_owned());

					params.push(Parameter::Const(ConstParam {
						name,
						r#type: TypeExpr { name: ty, args: vec![] },
						default_value: None,
					}));
				}
				EntityKind::TemplateTemplateParameter => {
					params.push(Parameter::Type(TypeParam {
						name:         child.get_name(),
						kind:         Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Type)),
						variance:     Variance::Invariant,
						default_type: None,
						params:       None,
						origin:       TypeParamOrigin::Free,
					}));
				}
				_ => {}
			}
		}

		if !params.is_empty() { Some(Generics { params, constraints: vec![] }) } else { None }
	}

	fn handle_function_attributes(&self, entity: Entity<'tu>) -> Option<Vec<FnAttribute>> {
		let mut attrs = vec![];

		if entity.is_variadic() {
			attrs.push(FnAttribute::Variadic);
		}

		for child in entity.get_children() {
			match child.get_kind() {
				EntityKind::PureAttr => attrs.push(FnAttribute::Pure),
				EntityKind::ConstAttr => attrs.push(FnAttribute::Const),
				_ => {}
			}
		}

		if !attrs.is_empty() { Some(attrs) } else { None }
	}

	fn output_parameters_from_type(&self, ty: Type) -> Vec<Parameter> {
		vec![Parameter::Literal(LiteralParameter {
			name:          String::new(),
			r#type:        Some(ty),
			attributes:    None,
			default_value: None,
			description:   None,
		})]
	}

	fn parameters_from_types(&self, types: Vec<ClangType<'tu>>) -> Option<Vec<Parameter>> {
		let params: Vec<_> = types
			.into_iter()
			.enumerate()
			.map(|(idx, ty)| {
				Parameter::Literal(LiteralParameter {
					name:          format!("arg{idx}"),
					r#type:        Some(self.resolve_type(ty)),
					attributes:    None,
					default_value: None,
					description:   None,
				})
			})
			.collect();

		if !params.is_empty() { Some(params) } else { None }
	}

	fn generic_args_from_type(&self, ty: ClangType<'tu>) -> Option<Vec<ir::generics::GenericArg>> {
		let args: Vec<_> = ty
			.get_template_argument_types()?
			.into_iter()
			.flatten()
			.map(|arg| ir::generics::GenericArg::Type(self.resolve_type(arg)))
			.collect();

		if !args.is_empty() { Some(args) } else { None }
	}

	fn parameter_attributes(ty: &Type) -> Option<Vec<ParameterAttribute>> {
		let mut attrs = vec![];

		match ty {
			Type::BorrowedRef { is_mutable: true, .. } => attrs.push(ParameterAttribute::Inout),
			Type::BorrowedRef { is_mutable: false, .. } => attrs.push(ParameterAttribute::Borrowing),
			Type::Variadic(_) => attrs.push(ParameterAttribute::Variadic),
			_ => {}
		}

		if !attrs.is_empty() { Some(attrs) } else { None }
	}

	fn visibility(&self, entity: Entity<'tu>) -> Visibility {
		match entity.get_accessibility() {
			Some(Accessibility::Private) => Visibility::Private,
			Some(Accessibility::Protected) => Visibility::Protected,
			Some(Accessibility::Public) => Visibility::Public,
			None => match entity.get_storage_class() {
				Some(StorageClass::Static) => Visibility::Internal,
				_ => Visibility::Public,
			},
		}
	}

	fn documentation(&self, entity: Entity<'tu>) -> Option<String> {
		entity
			.get_comment()
			.map(clean_comment)
			.or_else(|| entity.get_comment_brief().map(|comment| comment.trim().to_owned()))
			.filter(|comment| !comment.is_empty())
	}

	fn path_for_name(&self, name: &str) -> NudoxPath {
		let mut segments = self.scopes.clone();
		segments.push(name.to_owned());

		NudoxPath::Local(PathBuf::from(segments.join("::")))
	}

	fn entity_name(&mut self, entity: Entity<'tu>) -> String { entity.get_name().unwrap_or_default() }

	fn insert_entry(&mut self, entry: Entry, entity: Entity<'tu>) {
		self.map.insert(entity, entry.path().clone());
		self.entries.push(entry);
	}

	fn in_scope<R>(&mut self, scope: String, f: impl FnOnce(&mut Self) -> R) -> R {
		self.scopes.push(scope);
		let res = f(self);
		self.scopes.pop();
		res
	}
}

fn clean_comment(comment: String) -> String {
	let trimmed = comment.trim();

	let inner = trimmed
		.strip_prefix("/**")
		.and_then(|text| text.strip_suffix("*/"))
		.or_else(|| trimmed.strip_prefix("/*").and_then(|text| text.strip_suffix("*/")))
		.unwrap_or(trimmed);

	inner
		.lines()
		.map(|line| {
			line
				.trim()
				.strip_prefix("///")
				.or_else(|| line.trim().strip_prefix("//"))
				.unwrap_or_else(|| line.trim().strip_prefix('*').unwrap_or(line.trim()))
				.trim()
		})
		.filter(|line| !line.is_empty())
		.collect::<Vec<_>>()
		.join("\n")
}

fn clang_type_to_width(ty: ClangType) -> Width {
	match ty.get_sizeof().map(|it| it * 8) {
		Ok(8) => Width::W8,
		Ok(16) => Width::W16,
		Ok(32) => Width::W32,
		Ok(64) => Width::W64,
		Ok(80) => Width::W80,
		Ok(128) => Width::W128,
		Ok(_) => Width::Arch,
		Err(_) => unreachable!(),
	}
}

#[ouroboros::self_referencing]
struct ClangParserInner {
	clang: Clang,
	#[borrows(clang)]
	#[covariant]
	index: Index<'this>,
}
