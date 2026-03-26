use std::collections::HashMap;

use clang::{Clang, CompilationDatabase, Entity, EntityKind, Index, TranslationUnit};
use ir::{entry::Entry, function::Function, kind::Kind, parameter::Parameter};

use super::ClangError;
use crate::error::PackageError;

pub(crate) struct ClangParser {
	inner: ClangParserInner,
	db:    CompilationDatabase,
	ir:    IRBuilder,
}

impl ClangParser {
	pub fn new(clang: Clang, db: CompilationDatabase) -> Self {
		let inner = ClangParserInner::new(clang, |c| Index::new(c, false, false));
		ClangParser { inner, db, ir: IRBuilder::new() }
	}

	pub fn parse(mut self) -> Result<Vec<Entry>, PackageError> {
		let compile_commands = self.db.get_all_compile_commands();

		for command in compile_commands.get_commands() {
			let file_entrypoint = command.get_directory().join(command.get_filename());

			let parser = self.inner.borrow_index().parser(&file_entrypoint);

			// TODO: handle command arguments
			// &parser.arguments(&command.get_arguments());

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
		match entity.get_kind() {
			EntityKind::StructDecl => todo!(),
			EntityKind::ClassDecl => todo!(),
			EntityKind::EnumDecl => todo!(),
			EntityKind::FunctionDecl => self.handle_function(entity)?,
			EntityKind::TypedefDecl => todo!(),
			_ => return Err(PackageError::NotImplemented),
		}

		Ok(())
	}

	fn handle_function(&mut self, entity: Entity<'tu>) -> Result<(), PackageError> {
		debug_assert_eq!(entity.get_kind(), EntityKind::FunctionDecl);

		let (id, name) = self.basic_info(entity);

		let (input_parameters, output_parameters) =
			self.handle_function_params(entity.get_children())?;

		let generics = None; // TODO
		let attributes = None; // TODO
		let implemented = true; // TODO

		let entry = Entry {
			name: name.clone(),
			id,
			path: vec![],
			aliases: None,
			kind: Kind::Function(Function {
				input_parameters,
				output_parameters,
				type_links: None, // TODO?
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
						ty:            None, // TODO
						attributes:    None, // TOOD
						default_value: None, // TODO
						description:   None,
					});
				}
				EntityKind::CompoundStmt => {} // Ignore function bodies
				kind => {
					eprintln!("unexpected child kind {kind:?} as child of function");
					return Err(PackageError::NotImplemented);
				}
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

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	const TEST_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/clang/tests/");

	#[test]
	fn test_parse_file() {
		let dir = Path::new(TEST_DIR).join("simple01");

		dbg!(&dir);

		let clang = Clang::new().unwrap();
		let db = CompilationDatabase::from_directory(dir).unwrap();

		let parser = ClangParser::new(clang, db);

		let entries = parser.parse().expect("failed to parse file");

		assert!(entries.len() > 0);

		dbg!(entries);
	}
}
