use crate::core::rust_parser::RustdocParser;
use crate::error::NewDocsError;
use crate::traits::{package::Package, registry::Registry};
use crates_io_api::{AsyncClient, Crate, CratesQuery, SyncClient};
use lang_types::Language;
use url::Url;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Item not found: {0}")]
    ItemNotFound(u32),

    #[error("Invalid item kind for ID {id}: expected {expected}, got {actual}")]
    InvalidItemKind {
        id: String,
        expected: String,
        actual: String,
    },

    #[error("Missing required field: {field} in {context}")]
    MissingField { field: String, context: String },

    #[error("Re-export resolution failed for {path}: {reason}")]
    ReExportResolution { path: String, reason: String },

    #[error("Generic argument mismatch: expected {expected}, got {actual}")]
    GenericArgMismatch { expected: usize, actual: usize },

    #[error("Type resolution failed for {type_name}: {reason}")]
    TypeResolution { type_name: String, reason: String },

    #[error("Invalid visibility format: {0}")]
    InvalidVisibility(String),

    #[error("Path parsing error: {0}")]
    PathParsing(String),

    #[error("Unsupported item type: {0}")]
    UnsupportedItemType(String),

    #[error("Circular dependency detected: {path}")]
    CircularDependency { path: String },

    #[error("Generic constraint resolution failed: {reason}")]
    GenericConstraintResolution { reason: String },

    #[error("Trait bound resolution failed: {reason}")]
    TraitBoundResolution { reason: String },

    #[error("Invalid function signature: {reason}")]
    InvalidFunctionSignature { reason: String },

    #[error("Associated type resolution failed: {reason}")]
    AssociatedTypeResolution { reason: String },

    #[error("Impl block parsing failed: {reason}")]
    ImplBlockParsing { reason: String },

    #[error("Invalid primitive type: {0}")]
    InvalidPrimitive(String),
}

use crate::ir::entry::Entry;
use semver::Version;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Crates {
    pub client: AsyncClient,
}

pub struct RPackage {
    pub slug: String,
    pub name: String,
    pub language: Language,
    pub uuid: i64,
    pub source: Url,
    pub description: Option<String>,
}

impl RPackage {
    /// Internal helper to run cargo rustdoc and return the parsed Entry IR
    fn generate_ir(&self, version: &Version) -> Result<Vec<Entry>, NewDocsError> {
        let target_dir = std::env::current_dir()
            .map_err(|e| NewDocsError::IoError(e))?
            .join("target")
            .join("doc_json");

        if !target_dir.exists() {
            fs::create_dir_all(&target_dir).map_err(|e| NewDocsError::IoError(e))?;
        }

        // Execute: cargo rustdoc --output-format json -Z unstable-options
        // Note: This requires a nightly toolchain
        let status = Command::new("cargo")
            .arg("rustdoc")
            .arg("--package")
            .arg(&self.name)
            .arg("--")
            .arg("-Z")
            .arg("unstable-options")
            .arg("--output-format")
            .arg("json")
            .status()
            .map_err(|e| NewDocsError::ProcessError(format!("Failed to run cargo: {}", e)))?;

        if !status.success() {
            return Err(NewDocsError::ProcessError(
                "Cargo rustdoc failed".to_string(),
            ));
        }

        // Location follows Cargo's standard: target/doc/package_name.json
        let json_path =
            PathBuf::from("target/doc").join(format!("{}.json", self.name.replace('-', "_")));

        let json_content = fs::read_to_string(&json_path).map_err(|e| NewDocsError::IoError(e))?;

        let rustdoc_crate: rustdoc_types::Crate = serde_json::from_str(&json_content)
            .map_err(|e| NewDocsError::ParsingError(format!("JSON fail: {}", e)))?;

        let mut parser = RustdocParser::new(rustdoc_crate)?;
        Ok(parser.parse_crate().unwrap())
    }
}

impl From<Crate> for RPackage {
    fn from(c: Crate) -> Self {
        RPackage {
            slug: c.name.to_lowercase(),
            name: c.name.clone(),
            language: Language::Rust,
            uuid: c.id.parse::<i64>().unwrap_or(0),
            source: Url::parse(
                &c.repository
                    .unwrap_or_else(|| format!("https://crates.io/crates/{}", c.name)),
            )
            .unwrap_or_else(|_| Url::parse("https://crates.io").unwrap()),
            description: c.description,
        }
    }
}

impl Package for RPackage {
    fn get_available_versions(&self) -> Result<Vec<Version>, NewDocsError> {
        // In a real implementation, you'd use the crates_io_api client here
        // For brevity, assuming the versions are fetched via the registry client
        Err(NewDocsError::NotImplemented)
    }

    fn flags(&self) -> Result<Option<Vec<String>>, NewDocsError> {
        // Mocking common Rust features as per the Swift prototype
        Ok(Some(vec!["default".into(), "std".into(), "alloc".into()]))
    }

    fn description(&self) -> Result<Option<String>, NewDocsError> {
        Ok(self.description.clone())
    }

    fn retrieve(
        &self,
        version: Version,
        _flags: Option<Vec<String>>,
    ) -> Result<String, NewDocsError> {
        let entries = self.generate_ir(&version)?;
        serde_json::to_string(&entries).map_err(|e| NewDocsError::ParsingError(e.to_string()))
    }

    fn dependencies(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        // Logic to fetch dependencies via crates.io API
        todo!("Implement dependency resolution")
    }

    fn dependents(&self) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        todo!("Implement reverse dependency resolution")
    }
}

impl Registry for Crates {
    type Error = NewDocsError;

    async fn search_packages(&self, query: &str) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        let q = CratesQuery::builder().search(query).build();
        let result = self
            .client
            .crates(q)
            .await
            .map_err(|e| NewDocsError::NetworkError(e.to_string()))?;

        Ok(result
            .crates
            .into_iter()
            .map(|c| Box::new(RPackage::from(c)) as Box<dyn Package>)
            .collect())
    }

    async fn get_package_by_uuid(&self, uuid: u64) -> Result<Box<dyn Package>, NewDocsError> {
        let c = self
            .client
            .get_crate(&uuid.to_string())
            .await
            .map_err(|e| NewDocsError::NetworkError(e.to_string()))?;
        Ok(Box::new(RPackage::from(c.crate_data)))
    }

    async fn get_packages_by_name(
        &self,
        name: &str,
    ) -> Result<Vec<Box<dyn Package>>, NewDocsError> {
        let c = self
            .client
            .get_crate(name)
            .await
            .map_err(|e| NewDocsError::NetworkError(e.to_string()))?;
        Ok(vec![Box::new(RPackage::from(c.crate_data))])
    }

    async fn get_reference(&self) -> Box<dyn Package> {
        Box::new(RPackage {
            slug: "rust-reference".into(),
            name: "The Rust Reference".into(),
            language: Language::Rust,
            uuid: 0,
            source: Url::parse("https://doc.rust-lang.org/reference/").unwrap(),
            description: Some("The official reference manual for the Rust language".into()),
        })
    }
}
