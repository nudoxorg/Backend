//! Per-language ingestion strategy.
//!
//! Everything language-specific about turning a tracked package into IR lives
//! behind [`LanguageBackend`]: the git tag scheme that maps a semver version to
//! a commit, the post-checkout workspace preparation, and IR generation itself.
//! The sync engine ([`crate::local_registry`]) and the ingest pipeline
//! ([`crate::ingest`]) are otherwise language-agnostic — they are generic over
//! `B: LanguageBackend` and never match on a concrete language.

use std::{collections::HashMap, path::Path};

use gix::{ObjectId, Repository};
use ir::entry::Index;
use nudox_core::Language;
use semver::Version;
use url::Url;

use crate::{core::{rust::RustPackage, ts::TsPackage}, error::{AppError, IngestError}, git, local_registry::ts_entry_point::{resolve_typescript_repository_entry_point, typescript_repository_entry_hint}};

/// The complete language-specific surface of ingestion.
///
/// Implementors are zero-sized strategy markers (`RustBackend`,
/// `TypeScriptBackend`); callers dispatch statically via the type parameter.
pub trait LanguageBackend {
	/// The concrete package handle this backend operates on.
	type Package: Clone + Send + 'static;

	/// The neutral language tag stamped onto every projected symbol.
	const LANGUAGE: Language;

	/// The package's display name (used for logging and the registry lookup).
	fn package_name(package: &Self::Package) -> &str;

	/// The package's upstream source URL (used to clone/open the repository).
	fn package_source(package: &Self::Package) -> &Url;

	/// Resolve `version` to a commit using this language's tag conventions.
	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId>;

	/// Hook run after a git workspace is materialized, before IR generation.
	///
	/// Rust is a no-op; TypeScript resolves and records the source entry point so
	/// the doc frontend knows where to start.
	fn prepare_workspace(
		package: Self::Package,
		workspace: &Path,
	) -> Result<Self::Package, AppError>;

	/// Generate the IR index and the raw-source map for a materialized package.
	///
	/// `workspace` is the checked-out source root and `version` the tracked
	/// version; a frontend that resolves its inputs from the package handle (e.g.
	/// TypeScript via its entry point) may ignore them. The source map keys
	/// fully-qualified names to raw source for tree-sitter; it is empty when the
	/// frontend does not surface spans.
	///
	/// Blocking (rustdoc / deno-doc are CPU-bound); callers run it under
	/// `spawn_blocking`.
	fn generate_ir(
		package: &Self::Package,
		workspace: &Path,
		version: &Version,
	) -> Result<(Index, HashMap<String, String>), AppError>;
}

/// Rust packages: crates.io / cargo conventions, IR via rustdoc JSON.
pub struct RustBackend;

impl LanguageBackend for RustBackend {
	type Package = RustPackage;

	const LANGUAGE: Language = Language::Rust;

	fn package_name(package: &RustPackage) -> &str { &package.name }

	fn package_source(package: &RustPackage) -> &Url { &package.source }

	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId> {
		git::find_commit_for_version(repository, version, name, remote_head)
	}

	fn prepare_workspace(package: RustPackage, _workspace: &Path) -> Result<RustPackage, AppError> {
		Ok(package)
	}

	fn generate_ir(
		package: &RustPackage,
		workspace: &Path,
		version: &Version,
	) -> Result<(Index, HashMap<String, String>), AppError> {
		let (ir, source_map) =
			package.generate_ir_with_sources(&workspace.to_path_buf(), version).map_err(|source| {
				AppError::Ingest(IngestError::IrGeneration { package: package.name.clone(), source })
			})?;
		Ok((ir.index().into_index(), source_map))
	}
}

/// TypeScript packages: npm / JSR conventions, IR via deno-doc.
pub struct TypeScriptBackend;

impl LanguageBackend for TypeScriptBackend {
	type Package = TsPackage;

	const LANGUAGE: Language = Language::TypeScript;

	fn package_name(package: &TsPackage) -> &str { &package.name }

	fn package_source(package: &TsPackage) -> &Url { &package.source }

	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId> {
		git::find_typescript_commit_for_version(repository, version, name, remote_head)
	}

	fn prepare_workspace(
		mut package: TsPackage,
		workspace: &Path,
	) -> Result<TsPackage, AppError> {
		let entry_point = resolve_typescript_repository_entry_point(
			workspace,
			typescript_repository_entry_hint(&package),
		)?;
		package.entry_point = entry_point.display().to_string();
		Ok(package)
	}

	fn generate_ir(
		package: &TsPackage,
		_workspace: &Path,
		_version: &Version,
	) -> Result<(Index, HashMap<String, String>), AppError> {
		let ir = package.generate_ir().map_err(|source| {
			AppError::Ingest(IngestError::TsIrGeneration { package: package.name.clone(), source })
		})?;
		Ok((ir.index().into_index(), HashMap::new()))
	}
}
