pub mod error;
pub mod rust;
pub mod ts;

pub use error::Error;

use std::{collections::HashMap, path::Path};

use gix::{ObjectId, Repository};
use ir::entry::Index;
use nudox_core::Language;
use semver::Version;
use url::Url;

use crate::backends::{
	rust::RustPackage,
	ts::{Package, entry_point::{resolve_typescript_repository_entry_point, typescript_repository_entry_hint}},
};
use crate::vcs;

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

	fn prepare_workspace(package: Self::Package, workspace: &Path)
	-> Result<Self::Package, Error>;

	fn generate_ir(
		package: &Self::Package,
		workspace: &Path,
		version: &Version,
	) -> Result<(Index, HashMap<String, String>), Error>;
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
		vcs::find_commit_for_version(repository, version, name, remote_head)
	}

	fn prepare_workspace(package: RustPackage, _workspace: &Path) -> Result<RustPackage, Error> {
		Ok(package)
	}

	fn generate_ir(
		package: &RustPackage,
		workspace: &Path,
		version: &Version,
	) -> Result<(Index, HashMap<String, String>), Error> {
		let (ir, source_map) =
			package.generate_ir_with_sources(&workspace.to_path_buf(), version).map_err(|source| {
				Error::IrGeneration { package: package.name.clone(), source }
			})?;
		Ok((ir.index().into_index(), source_map.into_iter().collect()))
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
		vcs::find_typescript_commit_for_version(repository, version, name, remote_head)
	}

	fn prepare_workspace(mut package: Package, workspace: &Path) -> Result<Package, Error> {
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
	) -> Result<(Index, HashMap<String, String>), Error> {
		let ir = package.generate_ir().map_err(|source| {
			Error::TsIrGeneration { package: package.name.clone(), source }
		})?;
		Ok((ir.index().into_index(), HashMap::new()))
	}
}
