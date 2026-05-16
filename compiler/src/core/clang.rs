mod parser;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use clang::{Clang, CompilationDatabase, SourceError};
use thiserror::Error;

use self::parser::ClangParser;
use crate::{error::PackageError, pipeline::{Collected, Ir}};

#[derive(Debug, Error)]
pub enum ClangError {
	#[error("an instance of `Clang` already exists")]
	MultipleInstances,

	#[error("failed to load compilation database at {0}")]
	CompilationDatabaseLoadError(PathBuf),

	#[error("failed to parse translation unit at {0}: {1}")]
	SourceError(PathBuf, SourceError),
}

pub struct ClangProject;

impl ClangProject {
	pub fn generate_ir(dir: impl AsRef<Path>) -> Result<Ir<Collected>, PackageError> {
		// TODO: maybe sanity check dir for nicer errors instead of passing straight
		// into clang?

		let dir = dir.as_ref();

		assert!(dir.exists());

		// TODO: handle this more cleanly?
		let clang = Clang::new().map_err(|_| ClangError::MultipleInstances)?;

		let db = CompilationDatabase::from_directory(dir)
			.map_err(|_| ClangError::CompilationDatabaseLoadError(dir.to_path_buf()))?;

		let parser = ClangParser::new(clang, db, dir.to_path_buf());

		parser.parse().map(Ir::from_entries)
	}
}
