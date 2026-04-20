/// TypeScript/JavaScript package and registry types.
///
/// Mirrors the architecture of `compiler/src/core/rust.rs` for the Rust
/// pipeline.  `TsPackage` implements the `Package` trait by shelling out to
/// Deno (the same way `RustPackage` shells out to `cargo rustdoc`), capturing
/// the `jsr:@deno/doc` JSON output, and feeding it through `TsDocParser`.
use std::{collections::HashMap, process::Command, process::ExitStatus};

use semver::Version;
use thiserror::Error;
use url::Url;

use crate::{core::ts_parser::{ParseError, TsDocParser}, pipeline::{Collected, Ir}, traits::{package::Package, registry::Registry}};

// ============================================================================
// Error types
// ============================================================================

/// Parse/conversion errors from the TypeScript pipeline.
/// Mirrors `crate::core::rust::ParseError`.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TsParseError {
	#[error("symbol not found: {0}")]
	SymbolNotFound(String),

	#[error("type resolution failed for `{type_name}`: {reason}")]
	TypeResolution { type_name: String, reason: String },

	#[error("invalid declaration: {0}")]
	InvalidDeclaration(String),

	#[error("unsupported declaration kind: {0}")]
	UnsupportedDeclarationKind(String),

	#[error("circular dependency at path: {path}")]
	CircularDependency { path: String },

	#[error("generic constraint resolution failed: {reason}")]
	GenericConstraintResolution { reason: String },
}

/// Errors arising from TypeScript package operations.
/// Mirrors `crate::error::PackageError`.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TsPackageError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("process `{command}` failed with {status}")]
	Process { command: String, status: ExitStatus },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("UTF-8 decode error: {0}")]
	Utf8(#[from] std::string::FromUtf8Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("package not found: {0}")]
	NotFound(String),

	#[error("invalid URL: {0}")]
	InvalidUrl(String),
}

// ============================================================================
// TsPackage — implements the Package trait
// ============================================================================

/// A TypeScript/JavaScript package that can be documented via `jsr:@deno/doc`.
///
/// Mirrors `RustPackage` in `rust.rs`.
pub struct TsPackage {
	pub slug:        String,
	pub name:        String,
	pub uuid:        u64,
	pub source:      Url,
	pub description: Option<String>,
	/// The primary entry-point specifier passed to `deno doc`.
	/// Can be a URL, local path, or `npm:`/`jsr:` specifier.
	pub entry_point: String,
}

impl TsPackage {
	/// Absolute path to the embedded Deno runner script.
	///
	/// The script lives in the same directory as this source file and is
	/// referenced at compile time so the path is always valid regardless of
	/// the working directory at runtime.
	const RUNNER_SCRIPT: &'static str =
		concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/ts_doc_runner.ts");

	/// Shell out to Deno, run `ts_doc_runner.ts` against `entry_point`, and
	/// convert the resulting JSON into an `Ir<Collected>`.
	///
	/// Mirrors `RustPackage::generate_ir` in `rust.rs`.
	fn generate_ir(&self) -> Result<Ir<Collected>, TsPackageError> {
		let output = Command::new("deno")
			.arg("run")
			.arg("--allow-read")
			.arg("--allow-net")
			.arg("--allow-env")
			.arg(Self::RUNNER_SCRIPT)
			.arg(&self.entry_point)
			.output()?;

		if !output.status.success() {
			return Err(TsPackageError::Process {
				command: format!("deno run ts_doc_runner {}", self.entry_point),
				status:  output.status,
			});
		}

		let json = String::from_utf8(output.stdout)?;
		let documents: HashMap<String, deno_doc::Document> = serde_json::from_str(&json)?;

		let mut parser = TsDocParser::new(documents)?;
		let entries = parser.parse_documents()?;

		Ok(Ir::from_entries(entries))
	}
}

impl Package for TsPackage {
	type Error = TsPackageError;

	fn get_available_versions(&self) -> Result<Vec<Version>, Self::Error> {
		Err(TsPackageError::NotImplemented)
	}

	fn flags(&self) -> Result<Option<Vec<String>>, Self::Error> { Ok(None) }

	fn description(&self) -> Result<Option<String>, Self::Error> { Ok(self.description.clone()) }

	fn retrieve(
		&self,
		_version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, Self::Error> {
		self.generate_ir()
	}

	fn dependencies(&self) -> Result<Vec<Self>, Self::Error> { Err(TsPackageError::NotImplemented) }

	fn dependents(&self) -> Result<Vec<Self>, Self::Error> { Err(TsPackageError::NotImplemented) }
}

// ============================================================================
// Npm — implements the Registry trait
// ============================================================================

/// npm registry client for discovering and fetching TypeScript packages.
///
/// Mirrors `Crates` in `rust.rs`.
pub struct Npm {
	/// Base URL for the npm registry API.
	pub registry_url: Url,
}

impl Default for Npm {
	fn default() -> Self { Self { registry_url: Url::parse("https://registry.npmjs.org").unwrap() } }
}

impl Registry for Npm {
	type Error = TsPackageError;
	type Pkg = TsPackage;

	async fn search_packages(&self, query: &str) -> Result<Vec<TsPackage>, TsPackageError> {
		// npm search via the registry API (`/-/v1/search?text=<query>`).
		// Full HTTP client integration would use `reqwest` or similar; for
		// now return NotImplemented until the HTTP layer is wired in.
		let _ = query;
		Err(TsPackageError::NotImplemented)
	}

	async fn get_package_by_id(&self, id: u64) -> Result<TsPackage, TsPackageError> {
		let _ = id;
		Err(TsPackageError::NotImplemented)
	}

	async fn get_packages_by_name(&self, name: &str) -> Result<Vec<TsPackage>, TsPackageError> {
		// Construct an `npm:` specifier and return a stub package.
		let slug = name.to_lowercase().replace('/', "__");
		Ok(vec![TsPackage {
			slug:        slug.clone(),
			name:        name.to_string(),
			uuid:        0,
			source:      self.registry_url.join(name).unwrap_or_else(|_| self.registry_url.clone()),
			description: None,
			entry_point: format!("npm:{}", name),
		}])
	}

	async fn get_reference(&self) -> TsPackage {
		TsPackage {
			slug:        "typescript-reference".into(),
			name:        "TypeScript Reference".into(),
			uuid:        0,
			source:      Url::parse("https://www.typescriptlang.org/docs/").unwrap(),
			description: Some("The official TypeScript language reference documentation".into()),
			entry_point: "npm:typescript".into(),
		}
	}
}
