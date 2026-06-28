use std::{collections::HashMap, path::Path};

use gix::{ObjectId, Repository};
use ir::entry::Index;
use nudox_core::Language;
use semver::Version;
use url::Url;

use crate::{
	core::{rust::RustPackage, ts::Package, ts::entry_point::{resolve_typescript_repository_entry_point, typescript_repository_entry_hint}},
	git,
	http::error::{AppError, IngestError},
};

pub trait LanguageBackend {
	type Package: Clone + Send + 'static;

	const LANGUAGE: Language;

	fn package_name(package: &Self::Package) -> &str;

	fn package_source(package: &Self::Package) -> &Url;

	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId>;

	fn prepare_workspace(
		package: Self::Package,
		workspace: &Path,
	) -> Result<Self::Package, AppError>;

	fn generate_ir(
		package: &Self::Package,
		workspace: &Path,
		version: &Version,
	) -> Result<(Index, HashMap<String, String>), AppError>;
}

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

pub struct TypeScriptBackend;

impl LanguageBackend for TypeScriptBackend {
	type Package = Package;

	const LANGUAGE: Language = Language::TypeScript;

	fn package_name(package: &Package) -> &str { &package.name }

	fn package_source(package: &Package) -> &Url { &package.source }

	fn find_commit(
		repository: &Repository,
		version: &Version,
		name: &str,
		remote_head: Option<ObjectId>,
	) -> Option<ObjectId> {
		git::find_typescript_commit_for_version(repository, version, name, remote_head)
	}

	fn prepare_workspace(
		mut package: Package,
		workspace: &Path,
	) -> Result<Package, AppError> {
		let entry_point = resolve_typescript_repository_entry_point(
			workspace,
			typescript_repository_entry_hint(&package),
		)?;
		package.entry_point = entry_point.display().to_string();
		Ok(package)
	}

	fn generate_ir(
		package: &Package,
		_workspace: &Path,
		_version: &Version,
	) -> Result<(Index, HashMap<String, String>), AppError> {
		let ir = package.generate_ir().map_err(|source| {
			AppError::Ingest(IngestError::TsIrGeneration { package: package.name.clone(), source })
		})?;
		Ok((ir.index().into_index(), HashMap::new()))
	}
}
