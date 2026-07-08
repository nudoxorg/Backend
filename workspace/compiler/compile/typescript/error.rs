//! The TypeScript backend's error taxonomy: IR lowering failures ([`Parse`])
//! and package-level entry-point/graph failures ([`Package`]).

use thiserror::Error;

/// A failure while lowering the deno-doc document set into the IR.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Parse {
	#[error("symbol not found: {0}")]
	SymbolNotFound(String),

	#[error("declaration has no usable kind for symbol `{symbol}`: {reason}")]
	NoUsableDeclaration { symbol: String, reason: String },

	#[error("type resolution failed for `{type_name}`: {reason}")]
	TypeResolution { type_name: String, reason: String },

	#[error("invalid parameter shape in `{context}`: {reason}")]
	InvalidParameter { context: String, reason: String },

	#[error("generic constraint resolution failed: {reason}")]
	GenericConstraintResolution { reason: String },

	#[error("interface method parsing failed for `{name}`: {reason}")]
	InterfaceMethodParsing { name: String, reason: String },

	#[error("unsupported declaration kind: {0}")]
	UnsupportedDeclarationKind(String),

	#[error("circular dependency detected at path: {path}")]
	CircularDependency { path: String },
}

/// A failure while resolving a package's entry point or building/parsing its
/// documentation graph (IO, manifest JSON, deno-graph).
#[derive(Debug, Error)]
pub enum Package {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("parse error: {0}")]
	Parse(#[from] Parse),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("invalid local entry point: {0}")]
	InvalidLocalEntryPoint(String),

	#[error("could not discover a TypeScript entry point under `{path}`")]
	EntryPointDiscovery { path: String },

	#[error("entry point `{path}` does not exist")]
	EntryPointMissing { path: String },

	#[error("TypeScript document graph generation failed: {0}")]
	Graph(String),
}
