//! The Rust backend's error taxonomy: IR lowering failures ([`Parse`]) and
//! package-level acquisition/tooling failures ([`Package`]).

use std::{io, process::ExitStatus};

use semver::Version;
use thiserror::Error;

/// Failures specific to type lowering from rustdoc JSON (covers primitives,
/// paths, generics, dyn, slices, pointers, impl-trait, qualified, etc).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TypeResolutionError {
	#[error("unknown primitive type: {0}")]
	UnknownPrimitive(String),

	#[error("invalid array length '{len}'")]
	InvalidArrayLength { len: String },

	#[error("type resolution failed for {type_name}: missing field {field}")]
	MissingTypeField { type_name: String, field: String },

	#[error("unknown item id {0} encountered during type resolution")]
	UnknownItem(u32),

	#[error("bound resolution failure for bound '{bound}' in context '{context}'")]
	BoundFailure { bound: String, context: String },

	#[error("generic argument mismatch: expected {expected}, got {actual}")]
	GenericArgMismatch { expected: usize, actual: usize },

	#[error("qualified path '{name}' could not be resolved")]
	QualifiedPathResolutionFailed { name: String },

	#[error("dyn trait had no traits or resolution failed")]
	DynTraitResolutionFailed,

	#[error("function pointer had invalid signature")]
	FunctionPointerSignatureInvalid,

	#[error("tuple contained unresolvable element type")]
	TupleElementFailed,

	#[error("slice inner type resolution failed")]
	SliceInnerFailed,

	#[error("raw pointer target type failed to resolve")]
	RawPointerTargetFailed,

	#[error("borrowed reference target failed")]
	BorrowedRefTargetFailed,

	#[error("impl trait bounds could not be resolved")]
	ImplTraitBoundsFailed,

	#[error("pattern type unsupported for resolution")]
	PatTypeUnsupported,

	#[error("unexpected infer type in resolution context")]
	UnexpectedInferType,

	#[error("generic parameter resolution issue: {reason}")]
	GenericParamResolution { reason: String },

	#[error("associated type in type position failed: {name}")]
	AssociatedTypeInTypeFailed { name: String },

	#[error("const generic expr invalid")]
	InvalidConstGeneric,

	#[error("lifetime resolution in type failed")]
	LifetimeInTypeFailed,

	#[error("type resolution failed for {type_name}: {reason}")]
	Other { type_name: String, reason: String },
}

/// Sub-enum covering item lookup, kinds, fields, reexports, paths, names,
/// modules and other rustdoc JSON structural issues (reexports, visibility,
/// generics members, impl contents, etc).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ItemError {
	#[error("item not found: {0}")]
	NotFound(u32),

	#[error("invalid item kind for ID {id}: expected {expected}, got {actual}")]
	InvalidKind { id: u32, expected: &'static str, actual: String },

	#[error("missing required field: {field} in {context}")]
	MissingField { field: &'static str, context: &'static str },

	#[error("re-export resolution failed: name={name} target={target_id:?}: {reason}")]
	ReExportFailed { name: String, target_id: Option<u32>, reason: String },

	#[error("re-export has no target id for name '{name}'")]
	ReExportNoTargetId { name: String },

	#[error("re-export target {id} missing from index")]
	ReExportTargetMissing { id: u32 },

	#[error("item {id} has no name")]
	ItemMissingName { id: u32 },

	#[error("no primary path found for id {0}")]
	NoPrimaryPath(u32),

	#[error("path parsing failed: {0}")]
	PathParsingFailed(String),

	#[error("circular dependency: {path}")]
	CircularDependency { path: String },

	#[error("target not in index during path map build: {0}")]
	TargetNotInIndex(u32),

	#[error("module child missing during traversal: {0}")]
	ModuleChildMissing(u32),

	#[error("use item (reexport) unresolved or glob without support: {name}")]
	UnresolvedGlobReexport { name: String },

	#[error("trait item {0} missing during member collection")]
	TraitMemberMissing(u32),

	#[error("visibility parse issue")]
	VisibilityIssue { details: String },

	#[error("span missing for item {id} in rustdoc JSON")]
	MissingSpan { id: u32 },

	#[error("rustdoc paths map missing entry for id {id}")]
	MissingPathsEntry { id: u32 },
}

/// Sub-enum for generics / bounds / where / param issues inferred from rustdoc lowering.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum GenericError {
	#[error("generic constraint resolution failed: {reason}")]
	ConstraintResolution { reason: String },

	#[error("trait bound resolution failed: {reason}")]
	TraitBoundResolution { reason: String },

	#[error("invalid generic argument count or structure")]
	ArgMismatch,

	#[error("where predicate on unknown param '{param}'")]
	WhereOnUnknownParam { param: String },

	#[error("assoc const without type in generics context")]
	AssocConstMissingType,

	#[error("generic param kind unsupported: {kind}")]
	UnsupportedGenericParamKind { kind: String },

	#[error("default type for generic param failed to lower")]
	GenericDefaultResolutionFailed,
}

/// Sub-enum for function signatures and associated items.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SignatureError {
	#[error("invalid function signature: {reason}")]
	Invalid { reason: String },

	#[error("associated type resolution failed: {reason}")]
	AssociatedType { reason: String },

	#[error("receiver kind could not be determined")]
	ReceiverResolutionFailed,
}

/// Sub-enum for impl/trait-impl specific rustdoc issues.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum ImplError {
	#[error("impl block parsing failed: {reason}")]
	Failed { reason: String },

	#[error("inherent impl block not supported for TraitImpl lowering")]
	InherentImplNotSupported,

	#[error("negative impl encountered for trait lowering")]
	NegativeImplNotSupported,

	#[error("blanket impl without for-type resolution")]
	BlanketImplResolutionFailed,

	#[error("impl for type failed to resolve")]
	ForTypeResolutionFailed,

	#[error("impl item {id} was not a function or assoc item")]
	UnexpectedImplItemKind { id: u32 },
}

/// A failure while lowering rustdoc JSON into the IR.
/// Uses sub-enums for explosion of former stringly-typed cases + deep coverage
/// of rustdoc JSON shapes (reexports via Use, module trees, generics, visibility,
/// primitives, impls/assoc, spans, paths, bounds, etc).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Parse {
	#[error(transparent)]
	Item(#[from] ItemError),

	#[error(transparent)]
	Type(#[from] TypeResolutionError),

	#[error(transparent)]
	Generic(#[from] GenericError),

	#[error(transparent)]
	Signature(#[from] SignatureError),

	#[error(transparent)]
	Impl(#[from] ImplError),

	#[error("unsupported item type")]
	UnsupportedItemType,

	#[error("invalid primitive type: {0}")]
	InvalidPrimitive(String),

	// retained numeric mismatch for direct use
	#[error("generic argument mismatch: expected {expected}, got {actual}")]
	GenericArgMismatch { expected: usize, actual: usize },

	// additional explicit variants for rustdoc JSON edge cases
	#[error("missing crate root in rustdoc JSON")]
	MissingCrateRoot,

	#[error("rustdoc JSON missing expected 'paths' entry for id {0}")]
	MissingPathsEntry(u32),

	#[error("span missing for source extraction on item {0}")]
	MissingSpan(u32),

	#[error("unexpected rustdoc item structure for primitives scan")]
	InvalidPrimitiveItemStructure,

	#[error("failed to resolve trait ref in generics or bounds")]
	TraitRefResolutionFailed,

	#[error("const generic default resolution failed")]
	ConstGenericDefaultFailed,

	#[error("invalid visibility format: {0}")]
	InvalidVisibility(String),

	#[error("path parsing error: {0}")]
	PathParsing(String),
}

/// A failure while resolving/documenting a package (tooling, IO, JSON).
#[derive(Debug, Error)]
pub enum Package {
	#[error(transparent)]
	Io(#[from] io::Error),

	#[error(transparent)]
	Process(#[from] ProcessFailure),

	#[error(transparent)]
	Parse(#[from] Parse),

	#[error(transparent)]
	Serialization(#[from] serde_json::Error),

	#[error(transparent)]
	Metadata(#[from] MetadataError),

	#[error("version {0} not found")]
	VersionNotFound(Version),
}

#[derive(Debug, Error)]
pub enum ProcessFailure {
	#[error("process `{command}` failed")]
	Failed {
		command: String,
		#[source]
		kind: ProcessFailureKind,
		stdout: Option<String>,
		stderr: Option<String>,
	},
}

#[derive(Debug, Error)]
pub enum ProcessFailureKind {
	#[error("exited with status {status}")]
	NonZeroExit { status: ExitStatus },

	#[error("terminated by signal")]
	Signaled,

	#[error("timed out")]
	TimedOut,
}

#[derive(Debug, Error)]
pub enum MetadataError {
	#[error("cargo metadata command failed: {stderr}")]
	CargoFailed { stderr: String },

	#[error("failed to deserialize cargo metadata JSON: {0}")]
	Json(#[from] serde_json::Error),
}
