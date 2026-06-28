use std::path::PathBuf;

use lang_types::Language;
use url::Url;

use crate::{
	core::{rust::{Crates, RustPackage}, ts::TsPackage},
	http::error::{AppError, RegistryLookupError},
};

use super::{NewPackageRequest, PackageHandle, TYPESCRIPT_REPOSITORY_ENTRY_PREFIX};
use crate::core::ts::entry_point::typescript_slug;

pub(crate) async fn resolve_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	match request.language {
		Language::Rust => {
			if request.source.is_some() {
				return resolve_explicit_rust_package_handle(request);
			}
			let registry = Crates::new();
			let packages = registry.get_packages_by_name(&request.name).await.map_err(|err| {
				let parsers::rust::RegistryError::CratesIo(source) = err;
				AppError::RegistryLookup(RegistryLookupError::CratesIo {
					language: request.language,
					package:  request.name.clone(),
					source,
				})
			})?;
			resolve_rust_package_handle(request, packages)
		}
		Language::TypeScript => resolve_typescript_package_handle(request).await,
		other => Err(AppError::UnsupportedLanguage { language: other }),
	}
}

fn resolve_explicit_rust_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	let source = request
		.source
		.as_deref()
		.ok_or_else(|| AppError::MissingSource { kind: "Rust" })?;
	let source = parse_explicit_repository_source(request, source)?;

	Ok(PackageHandle::Rust(RustPackage {
		slug: request.name.to_ascii_lowercase(),
		name: request.name.clone(),
		language: Language::Rust,
		uuid: 0,
		source,
		direct_repo: true,
		description: None,
	}))
}

fn resolve_rust_package_handle(
	request: &NewPackageRequest,
	packages: Vec<RustPackage>,
) -> Result<PackageHandle, AppError> {
	let package = packages.into_iter().next().ok_or_else(|| {
		AppError::RegistryLookup(RegistryLookupError::NotFound {
			language: request.language,
			package:  request.name.clone(),
		})
	})?;
	Ok(PackageHandle::Rust(package))
}

async fn resolve_typescript_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	if request.source.is_some() {
		return resolve_explicit_typescript_package_handle(request);
	}

	let registry = crate::core::ts::Npm::default();
	let package =
		registry.resolve_package_version(&request.name, &request.version).await.map_err(|source| {
			AppError::RegistryLookup(RegistryLookupError::Npm {
				language: request.language,
				package:  request.name.clone(),
				source,
			})
		})?;
	Ok(PackageHandle::TypeScript(package))
}

fn resolve_explicit_typescript_package_handle(
	request: &NewPackageRequest,
) -> Result<PackageHandle, AppError> {
	let source =
		request.source.as_deref().ok_or_else(|| AppError::MissingSource { kind: "TypeScript" })?;
	let source = parse_explicit_repository_source(request, source)?;
	let entry_point = format!(
		"{TYPESCRIPT_REPOSITORY_ENTRY_PREFIX}{}",
		request.entry_point.as_deref().unwrap_or_default()
	);

	Ok(PackageHandle::TypeScript(TsPackage {
		slug: typescript_slug(&request.name),
		name: request.name.clone(),
		uuid: 0,
		source,
		description: None,
		entry_point,
	}))
}

fn parse_explicit_repository_source(
	request: &NewPackageRequest,
	source: &str,
) -> Result<Url, AppError> {
	if let Ok(url) = Url::parse(source) {
		return Ok(url);
	}

	let path = PathBuf::from(source);
	let canonical = path.canonicalize().map_err(|error| {
		AppError::RegistryLookup(RegistryLookupError::PathResolution {
			language: request.language,
			package:  request.name.clone(),
			path:     source.to_owned(),
			source:   error,
		})
	})?;

	Url::from_directory_path(&canonical)
		.or_else(|()| Url::from_file_path(&canonical))
		.map_err(|()| {
			AppError::RegistryLookup(RegistryLookupError::InvalidRepositoryPath {
				language: request.language,
				package:  request.name.clone(),
				path:     canonical.display().to_string(),
			})
		})
}
