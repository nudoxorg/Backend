//! The TypeScript backend's error taxonomy: IR lowering failures ([`Parse`])
//! and package-level entry-point/graph failures ([`Package`]).

use std::path::PathBuf;

use anyhow::Error as AnyhowError;
use deno_doc::DocError;
use thiserror::Error;

/// Failures during lowering of TypeScript *type expressions* (TsTypeDef, mapped,
/// indexed, generics, literals, refs, etc). Each resolution failure mode has an
/// explicit variant (no generic reason strings).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TsTypeError {
	#[error("missing key parameter in index signature")]
	MissingIndexSignatureKeyParameter,

	#[error("missing key type in index signature")]
	MissingIndexSignatureKeyType,

	#[error("missing value type in index signature")]
	MissingIndexSignatureValueType,

	#[error("missing source constraint for mapped type (type param `{param}`)")]
	MissingMappedTypeConstraint { param: String },

	#[error("type reference resolution failed for `{identifier}`")]
	TypeReferenceResolutionFailed { identifier: String },

	#[error("unsupported TS type definition kind `{kind}` while lowering `{context}`")]
	UnsupportedTypeDefinitionKind { kind: String, context: String },

	#[error("generic type parameter parse failed for `{name}`")]
	GenericTypeParameterFailed { name: String },

	#[error("type literal could not be lowered to record shape")]
	TypeLiteralLoweringFailed,
}

/// Failures at the declaration / symbol level (lookup, primary decl selection,
/// namespaces, unsupported kinds).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TsDeclarationError {
	#[error("symbol not found: {module}::{symbol}")]
	SymbolNotFound { module: String, symbol: String },

	#[error("declaration has no usable kind for symbol `{symbol}`")]
	NoUsableDeclaration { symbol: String },

	#[error("unsupported declaration kind: {kind}")]
	UnsupportedDeclarationKind { kind: String },

	#[error("namespace element `{element}` could not be resolved under `{parent}`")]
	NamespaceElementResolutionFailed { parent: String, element: String },
}

/// Failures specific to interfaces (TraitDef), their methods, call/index
/// signatures, and generic constraints on them.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TsInterfaceError {
	#[error("interface method parsing failed for `{name}`")]
	InterfaceMethodParsingFailed { name: String },

	#[error("call signature parsing failed at position {index}")]
	CallSignatureParsingFailed { index: usize },

	#[error("index signature (as method) parsing failed at position {index}")]
	IndexSignatureAsMethodFailed { index: usize },

	#[error("generic constraint resolution failed for parameter `{name}`")]
	GenericConstraintResolutionFailed { name: String },
}

/// A failure while lowering the deno-doc document set into the IR.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Parse {
	#[error(transparent)]
	Type(#[from] TsTypeError),

	#[error(transparent)]
	Declaration(#[from] TsDeclarationError),

	#[error(transparent)]
	Interface(#[from] TsInterfaceError),

	#[error("invalid parameter shape in `{context}`: {detail}")]
	InvalidParameter { context: String, detail: String },

	#[error("circular dependency detected at path: {path:?}")]
	CircularDependency { path: PathBuf },

	#[error("parameter parsing failed: {detail}")]
	ParameterParsingFailed { detail: String },

	#[error("deno-doc symbol lowering issue: {detail}")]
	DenoDocSymbolIssue { detail: String },
}

/// A failure while resolving a package's entry point or building/parsing its
/// documentation graph (IO, manifest JSON, deno-graph).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Package {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	Parse(#[from] Parse),

	#[error(transparent)]
	Serialization(#[from] serde_json::Error),

	#[error("invalid local entry point `{path:?}`")]
	InvalidLocalEntryPoint { path: PathBuf },

	#[error("could not discover a TypeScript entry point under `{path:?}`")]
	EntryPointDiscoveryFailed { path: PathBuf },

	#[error("entry point `{path:?}` does not exist")]
	EntryPointMissing { path: PathBuf },

	#[error("could not convert entry point `{path:?}` into module specifier")]
	InvalidModuleSpecifier { path: PathBuf },

	#[error("entry point `{candidate:?}` does not exist under root `{root:?}`")]
	EntryPointDoesNotExist { candidate: PathBuf, root: PathBuf },

	#[error("TypeScript document graph generation failed for `{entry:?}`")]
	GraphGenerationFailed { entry: PathBuf, #[source] source: AnyhowError },

	#[error("deno-doc parser creation failed under `{entry:?}`")]
	DocParserCreationFailed { entry: PathBuf, #[source] source: AnyhowError },

	#[error("deno-doc parse failed under `{entry:?}`")]
	DocParseFailed { entry: PathBuf, #[source] source: DocError },
}
