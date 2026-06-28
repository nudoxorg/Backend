use std::sync::Arc;
use serde::{Deserialize, Deserializer, Serialize};
use crate::RepoId;

/// Identifies an external library by name and version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LibRef {
	/// The library's package name.
	pub name:    String,
	/// The library's version string.
	pub version: String,
}

/// Describes where a symbol originates — either from an indexed repo or an
/// external library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolOrigin {
	/// The symbol originates from an indexed repository.
	Repo {
		/// The repository identifier.
		repo_id: RepoId,
	},
	/// The symbol originates from an external library dependency.
	ExternalLib {
		/// The external library reference.
		lib: LibRef,
	},
}

/// Opaque tree-sitter representation of a source chunk as raw bytes.
///
/// The concrete format is decided by the pipeline implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreesitterRepr(pub Vec<u8>);

/// A byte range within a source file or buffer.
///
/// Invariant: `start <= end`. Enforced on construction and during deserialization.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ByteSpan {
	start: usize,
	end:   usize,
}

impl ByteSpan {
	/// Returns `None` when `start > end`.
	pub fn new(start: usize, end: usize) -> Option<Self> {
		if start <= end { Some(Self { start, end }) } else { None }
	}

	/// Construct from a range known to satisfy `start <= end`. Panics otherwise.
	#[inline]
	pub fn covering(start: usize, end: usize) -> Self {
		assert!(start <= end, "ByteSpan: start ({start}) > end ({end})");
		Self { start, end }
	}

	/// Inclusive start byte offset.
	#[inline]
	pub fn start(self) -> usize { self.start }

	/// Exclusive end byte offset.
	#[inline]
	pub fn end(self) -> usize { self.end }

	/// Length in bytes.
	#[inline]
	pub fn len(self) -> usize { self.end - self.start }

	/// True when `start == end`.
	#[inline]
	pub fn is_empty(self) -> bool { self.start == self.end }
}

impl<'de> Deserialize<'de> for ByteSpan {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		#[derive(Deserialize)]
		struct Raw {
			/// Inclusive start byte offset.
			start: usize,
			/// Exclusive end byte offset.
			end:   usize,
		}
		let Raw { start, end } = Raw::deserialize(d)?;
		Self::new(start, end).ok_or_else(|| {
			serde::de::Error::custom(format!("ByteSpan: start ({start}) > end ({end})"))
		})
	}
}

/// A chunk of source code together with its tree-sitter representation and the
/// span of the primary symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceChunk {
	/// The raw source code text of this chunk.
	pub raw_code:        Arc<str>,
	/// The tree-sitter representation of this chunk.
	pub treesitter_repr: Option<TreesitterRepr>,
	/// The byte span of the primary symbol within `raw_code`.
	pub symbol_span:     ByteSpan,
}

/// Describes the semantic purpose for which an embedding was generated.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmbeddingPurpose {
	/// Embedding captures the code structure and semantics.
	Code,
	/// Embedding captures a docstring attached to the symbol.
	Docstring,
	/// Embedding captures an inline or block comment.
	Comment,
	/// Embedding captures some other user-defined purpose.
	Other(String),
}

/// Identifies the kind of embedding backend that produced a vector.
///
/// Carried with every [`EmbeddingRecord`] so that downstream consumers
/// (indexes, reranking, debugging) can distinguish vectors from different
/// providers even when their model names happen to collide.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelType {
	/// Deterministic placeholder embedder used for development.
	Placeholder,
	/// Deterministic constant-vector mock embedder used in tests.
	Mock,
	/// In-process model (e.g. candle, ort).
	InProcess,
	/// Remote HTTP embedding service hosted by OpenAI or similar.
	Openai,
	/// Self-hosted remote embedding service.
	SelfHosted,
	/// Any other model type identified by a free-form string.
	Other(String),
}

/// A non-empty embedding vector guaranteed to contain at least one element.
///
/// Serializes transparently as a JSON array; deserialization accepts any
/// `Vec<f32>` for wire compatibility with blobs written before this invariant
/// was added.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Embedding(Vec<f32>);

impl Embedding {
	/// Returns `None` if `v` is empty.
	pub fn new(v: Vec<f32>) -> Option<Self> {
		(!v.is_empty()).then_some(Self(v))
	}

	/// The number of dimensions (always ≥ 1 for well-formed instances).
	pub fn len(&self) -> usize { self.0.len() }

	/// Borrow as a float slice.
	pub fn as_slice(&self) -> &[f32] { &self.0 }

	/// Consume into the underlying `Vec<f32>`.
	pub fn into_vec(self) -> Vec<f32> { self.0 }
}

impl std::ops::Deref for Embedding {
	type Target = [f32];
	fn deref(&self) -> &[f32] { &self.0 }
}

impl PartialEq<Vec<f32>> for Embedding {
	fn eq(&self, other: &Vec<f32>) -> bool { self.0 == *other }
}

/// Canonical identifier for an embedding model (e.g. `"text-embedding-3-small"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

impl ModelId {
	/// Wrap any string as a `ModelId`.
	pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }

	/// Borrow the inner string.
	pub fn as_str(&self) -> &str { &self.0 }
}

impl From<&str> for ModelId {
	fn from(s: &str) -> Self { Self(s.to_owned()) }
}

impl From<String> for ModelId {
	fn from(s: String) -> Self { Self(s) }
}

impl From<ModelId> for String {
	fn from(m: ModelId) -> Self { m.0 }
}

impl PartialEq<str> for ModelId {
	fn eq(&self, other: &str) -> bool { self.0 == other }
}

impl std::fmt::Display for ModelId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

/// A single embedding vector produced by a specific model for a specific
/// purpose.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRecord {
	/// Family / provider of the model that produced this vector.
	pub model_type: ModelType,
	/// The model's identifier (e.g. `"text-embedding-3-small"`).
	pub model:      ModelId,
	/// The semantic purpose for which this embedding was generated.
	pub purpose:    EmbeddingPurpose,
	/// The raw embedding vector.
	pub vector:     Embedding,
}

/// Programming language of a source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
	/// The Rust programming language.
	Rust,
	/// The TypeScript programming language.
	TypeScript,
}

impl Language {
	/// The canonical lower-case identifier string (e.g. `"rust"`, `"typescript"`).
	pub fn as_str(&self) -> &'static str {
		match self {
			Language::Rust => "rust",
			Language::TypeScript => "typescript",
		}
	}
}

impl std::fmt::Display for Language {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl std::str::FromStr for Language {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.trim().to_ascii_lowercase().as_str() {
			"rust" | "rs" => Ok(Language::Rust),
			"typescript" | "ts" => Ok(Language::TypeScript),
			other => Err(format!("unknown language: {other}")),
		}
	}
}

/// Metadata that describes the context in which a source chunk was extracted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkMetadata {
	/// The repository from which this chunk was extracted.
	pub repo_id:             RepoId,
	/// Path to the source file within the repository.
	pub file_path:           std::path::PathBuf,
	/// Byte span of this chunk within the source file.
	pub file_span:           ByteSpan,
	/// Timestamp at which this chunk was parsed.
	pub parsed_at:           chrono::DateTime<chrono::Utc>,
	/// The programming language of this chunk.
	pub lang:                Language,
	/// Optional toolchain / language version string (e.g. `"1.78.0"` for Rust).
	pub lang_version:        Option<String>,
	/// Schema version of the [`BlobInfo`] format.
	pub blob_schema_version: u32,
}

/// The syntactic kind of a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SymbolKind {
	/// A free function or associated function.
	#[default]
	Function,
	/// A struct definition.
	Struct,
	/// An enum definition.
	Enum,
	/// A trait definition.
	Trait,
	/// A method on an impl block.
	Method,
	/// A closure expression.
	Closure,
	/// A type alias.
	TypeAlias,
	/// A constant or static item.
	Const,
	/// Any other symbol kind not enumerated above.
	Other,
}

impl SymbolKind {
	/// The canonical PascalCase name, matching the serde representation.
	pub fn as_str(&self) -> &'static str {
		match self {
			SymbolKind::Function => "Function",
			SymbolKind::Struct => "Struct",
			SymbolKind::Enum => "Enum",
			SymbolKind::Trait => "Trait",
			SymbolKind::Method => "Method",
			SymbolKind::Closure => "Closure",
			SymbolKind::TypeAlias => "TypeAlias",
			SymbolKind::Const => "Const",
			SymbolKind::Other => "Other",
		}
	}

	/// Lower-case label for payloads and embedding text (e.g. `"function"`).
	pub fn label(&self) -> &'static str {
		match self {
			SymbolKind::Function => "function",
			SymbolKind::Struct => "struct",
			SymbolKind::Enum => "enum",
			SymbolKind::Trait => "trait",
			SymbolKind::Method => "method",
			SymbolKind::Closure => "closure",
			SymbolKind::TypeAlias => "typealias",
			SymbolKind::Const => "const",
			SymbolKind::Other => "other",
		}
	}
}

impl std::fmt::Display for SymbolKind {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

/// Error returned when a string cannot be parsed into a [`SymbolKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSymbolKindError(pub String);

impl std::fmt::Display for ParseSymbolKindError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "unknown symbol kind: {}", self.0)
	}
}

impl std::error::Error for ParseSymbolKindError {}

impl std::str::FromStr for SymbolKind {
	type Err = ParseSymbolKindError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s.trim().to_ascii_lowercase().as_str() {
			"function" | "fn" => SymbolKind::Function,
			"struct" => SymbolKind::Struct,
			"enum" => SymbolKind::Enum,
			"trait" => SymbolKind::Trait,
			"method" => SymbolKind::Method,
			"closure" => SymbolKind::Closure,
			"typealias" | "type" | "type_alias" => SymbolKind::TypeAlias,
			"const" | "static" => SymbolKind::Const,
			"other" => SymbolKind::Other,
			_ => return Err(ParseSymbolKindError(s.to_owned())),
		})
	}
}
