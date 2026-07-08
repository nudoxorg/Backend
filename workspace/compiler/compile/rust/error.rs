//! The Rust backend's error taxonomy: IR lowering failures ([`Parse`]) and
//! package-level acquisition/tooling failures ([`Package`]).

use std::{io, process::ExitStatus};

use semver::Version;
use thiserror::Error;

/// A failure while lowering rustdoc JSON into the IR.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Parse {
	#[error("Item not found: {0}")]
	ItemNotFound(u32),

	#[error("Invalid item kind for ID {id}: expected {expected}, got {actual}")]
	InvalidItemKind { id: String, expected: String, actual: String },

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

	#[error("Unsupported item type")]
	UnsupportedItemType,

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

/// A failure while resolving/documenting a package (tooling, IO, JSON).
#[derive(Debug, Error)]
pub enum Package {
	#[error("IO error: {0}")]
	Io(#[from] io::Error),

	#[error("process `{command}` failed with {status}{details}")]
	Process { command: String, status: ExitStatus, details: String },

	#[error("parse error: {0}")]
	Parse(#[from] Parse),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("metadata error: {0}")]
	Metadata(String),

	#[error("version {0} not found")]
	VersionNotFound(Version),
}
