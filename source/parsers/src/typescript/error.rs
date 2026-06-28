use std::process::ExitStatus;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
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

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TsPackageError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("process `{command}` failed with {status}{details}")]
	Process { command: String, status: ExitStatus, details: String },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("UTF-8 decode error: {0}")]
	Utf8(#[from] std::string::FromUtf8Error),

	#[error("network error: {0}")]
	Network(#[from] reqwest::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("package not found: {0}")]
	NotFound(String),

	#[error("invalid URL: {0}")]
	InvalidUrl(String),

	#[error("invalid local entry point: {0}")]
	InvalidLocalEntryPoint(String),

	#[error("TypeScript document graph generation failed: {0}")]
	Graph(String),
}
