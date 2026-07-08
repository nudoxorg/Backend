//! Serialization helpers mapping heart's domain types onto their column
//! representations, and back. Kept **total and lossless**: every value has
//! exactly one column encoding and every stored encoding decodes unambiguously,
//! so a round-trip through postgres is the identity.
//!
//! - discriminant enums (`ResolutionState`, `Phase`, `FailureKind`,
//!   `Language`, `Visibility`, `SinkKind`, tenant kind) ↔ short `text` tokens;
//! - `Failure` / `Toolchain` ↔ `jsonb` (via serde);
//! - `ContentHash` / `Generation` ↔ `bytea` (exactly 32 bytes);
//! - `PackageId` / `SymbolId` / `Tenant` id ↔ `uuid`.
//!
//! The token tables here are the same domains the schema's CHECK constraints
//! enforce, so the database rejects precisely what [`state_from_token`] et al.
//! would fail to decode.

use heart::{
	Failure, Phase, ResolutionState,
	content::ContentHash,
	ecosystem::Language,
	identity::{SymbolId, PackageId},
};
use uuid::Uuid;

use crate::coordination::SinkKind;

/// A decode failure — a stored column value that does not correspond to any
/// domain value. Should be impossible given the CHECK constraints, but decoding
/// is kept total so a corrupt row surfaces an error rather than a panic.
///
/// Deep refactor: sub-variants for token kinds, #[from] for Name/Version,
/// concrete sources, no lossy to_string when wrapping sqlx.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
	/// A discriminant token was not a member of its domain (fine-grained).
	#[error("unknown ecosystem discriminant")]
	UnknownEcosystemDiscriminant { token: String },

	#[error("unknown sink kind discriminant")]
	UnknownSinkKindDiscriminant { token: String },

	#[error("unknown phase discriminant")]
	UnknownPhaseDiscriminant { token: String },

	#[error("unknown state discriminant")]
	UnknownStateDiscriminant { token: String },

	/// A `bytea` content hash was not exactly 32 bytes.
	#[error("content hash must be 32 bytes, got {0}")]
	BadHashLength(usize),

	/// A `jsonb` payload failed to deserialize into its domain type.
	#[error("json payload decode failed for {domain}")]
	Json { domain: &'static str, #[source] source: serde_json::Error },

	/// A `Progressing` row was missing its non-null `phase`, or a `Stored` row
	/// its non-null `content_hash`, etc. — a state/column-set invariant broke.
	#[error("state is missing required column")]
	MissingColumn { state: &'static str, column: &'static str },

	/// A stored package name no longer re-validates under its ecosystem's rules.
	/// (Now uses explicit NameEmpty / NameTooLong / NameHasInvalidChars.)
	#[error("stored package name failed re-validation")]
	Name(#[from] heart::NameError),

	/// A stored version string no longer parses under its ecosystem's grammar.
	/// (Now carries typed source via VersionError::{Cargo,Npm,Python} wrappers.)
	#[error("stored package version failed re-validation")]
	Version(#[from] heart::identity::VersionError),

	/// A custom origin token could not be reconstituted into a base URL.
	#[error("custom origin token does not name a valid base URL")]
	Origin { token: String, #[source] source: url::ParseError },

	/// Sqlx row decode surfaced with source (for search/index wrappers).
	#[error("codec sqlx decode")]
	SqlxDecode { domain: &'static str, #[source] source: sqlx::Error },
}

// ─────────────────────────────────────────────────────────────────────────────
// uuid ↔ id
// ─────────────────────────────────────────────────────────────────────────────

/// The raw uuid backing a [`PackageId`], for a `uuid` column.
pub fn package_id_to_uuid(id: PackageId) -> Uuid { *id.as_uuid() }

/// Reconstruct a [`PackageId`] from a `uuid` column.
///
/// `PackageId` wraps a private `Id<PackageCoordinates>` with no public
/// constructor, but it derives `Deserialize` and `Id<T>` (de)serializes as its
/// bare uuid — so round-tripping the raw uuid through serde is the sanctioned,
/// panic-free way to re-brand a value that was minted deterministically before
/// it was ever stored. Postgres uuids are always well-formed, so this is
/// infallible in practice; the error path is kept total regardless.
pub fn package_id_from_uuid(uuid: Uuid) -> PackageId {
	serde_json::from_value(serde_json::Value::String(uuid.to_string()))
		.expect("a valid uuid always deserializes into a PackageId")
}

/// The raw uuid backing a [`SymbolId`].
pub fn symbol_id_to_uuid(id: SymbolId) -> Uuid { *id.as_uuid() }


// ─────────────────────────────────────────────────────────────────────────────
// bytea ↔ content hash / generation
// ─────────────────────────────────────────────────────────────────────────────

/// The 32 raw bytes of a [`ContentHash`], for a `bytea` column.
pub fn hash_to_bytes(h: ContentHash) -> Vec<u8> { h.as_bytes().to_vec() }

/// Reconstruct a [`ContentHash`] from a `bytea` column (must be 32 bytes).
pub fn hash_from_bytes(bytes: &[u8]) -> Result<ContentHash, CodecError> {
	let arr: [u8; 32] = bytes.try_into().map_err(|_| CodecError::BadHashLength(bytes.len()))?;
	Ok(ContentHash::from_bytes(arr))
}

/// The 32 raw bytes of a snapshot [`ContentHash`], for a `bytea` column.
pub fn generation_to_bytes(g: ContentHash) -> Vec<u8> { hash_to_bytes(g) }

/// Reconstruct a snapshot [`ContentHash`] from a `bytea` column.
pub fn generation_from_bytes(bytes: &[u8]) -> Result<ContentHash, CodecError> {
	hash_from_bytes(bytes)
}

// ─────────────────────────────────────────────────────────────────────────────
// ecosystem / visibility / sink kind ↔ text
// ─────────────────────────────────────────────────────────────────────────────

/// The stable lowercase token for an [`Language`].
pub fn ecosystem_token(e: Language) -> &'static str { e.as_token() }

/// Reconstruct an [`Language`] from its token.
pub fn ecosystem_from_token(token: &str) -> Result<Language, CodecError> {
	match token {
		"rust" => Ok(Language::Rust),
		"typescript" => Ok(Language::Typescript),
		"python" => Ok(Language::Python),
		"go" => Ok(Language::Go),
		"java" => Ok(Language::Java),
		other => Err(CodecError::UnknownEcosystemDiscriminant { token: other.to_owned() }),
	}
}

/// The token for a [`SinkKind`].
pub fn sink_kind_token(k: SinkKind) -> &'static str {
	match k {
		SinkKind::Vector => "vector",
		SinkKind::Graph => "graph",
		SinkKind::Text => "text",
	}
}

/// Reconstruct a [`SinkKind`] from its token.
pub fn sink_kind_from_token(token: &str) -> Result<SinkKind, CodecError> {
	match token {
		"vector" => Ok(SinkKind::Vector),
		"graph" => Ok(SinkKind::Graph),
		"text" => Ok(SinkKind::Text),
		other => Err(CodecError::UnknownSinkKindDiscriminant { token: other.to_owned() }),
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// phase ↔ text
// ─────────────────────────────────────────────────────────────────────────────

/// The token for a [`Phase`].
pub fn phase_token(p: Phase) -> &'static str {
	match p {
		Phase::Acquiring => "acquiring",
		Phase::Extracting => "extracting",
		Phase::Compiling => "compiling",
		Phase::Emitting => "emitting",
	}
}

/// Reconstruct a [`Phase`] from its token.
pub fn phase_from_token(token: &str) -> Result<Phase, CodecError> {
	match token {
		"acquiring" => Ok(Phase::Acquiring),
		"extracting" => Ok(Phase::Extracting),
		"compiling" => Ok(Phase::Compiling),
		"emitting" => Ok(Phase::Emitting),
		other => Err(CodecError::UnknownPhaseDiscriminant { token: other.to_owned() }),
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// ResolutionState ↔ (state token, phase?, content_hash?, needed, failure jsonb)
// ─────────────────────────────────────────────────────────────────────────────

/// The state discriminant token for a [`ResolutionState`].
pub fn state_token(s: &ResolutionState) -> &'static str {
	match s {
		ResolutionState::Unindexed { .. } => "unindexed",
		ResolutionState::Progressing(_) => "progressing",
		ResolutionState::Stored { .. } => "stored",
		ResolutionState::Failed(_) => "failed",
		ResolutionState::DeadLettered(_) => "deadlettered",
	}
}

/// The full column projection of a [`ResolutionState`] for the `parse_status`
/// row — everything except the package id + `updated_at`.
///
/// This is the lossless decomposition the state machine persists: the
/// discriminant plus exactly the columns that variant populates, all others
/// `None`/default.
#[derive(Debug, Clone)]
pub struct StateColumns {
	/// `state` discriminant token.
	pub state: &'static str,
	/// `phase` token — `Some` only for `Progressing`.
	pub phase: Option<&'static str>,
	/// `content_hash` bytes — `Some` only for `Stored`.
	pub content_hash: Option<Vec<u8>>,
	/// `attempts` — carried from `Failure`, else 0.
	pub attempts: i32,
	/// `failure` jsonb — `Some` for `Failed`/`DeadLettered`.
	pub failure: Option<serde_json::Value>,
	/// `needed` — carried from `Unindexed`, else false.
	pub needed: bool,
}

/// Decompose a [`ResolutionState`] into its `parse_status` columns. Total: every
/// variant maps to exactly one column set.
pub fn state_to_columns(s: &ResolutionState) -> Result<StateColumns, CodecError> {
	let (phase, content_hash, attempts, failure, needed) = match s {
		ResolutionState::Unindexed { needed } => (None, None, 0, None, *needed),
		ResolutionState::Progressing(p) => (Some(phase_token(*p)), None, 0, None, false),
		ResolutionState::Stored { hash } => (None, Some(hash_to_bytes(*hash)), 0, None, false),
		ResolutionState::Failed(f) | ResolutionState::DeadLettered(f) => {
			let json = serde_json::to_value(f)
				.map_err(|source| CodecError::Json { domain: "Failure", source })?;
			(None, None, f.attempts as i32, Some(json), false)
		}
	};
	Ok(StateColumns { state: state_token(s), phase, content_hash, attempts, failure, needed })
}

/// Reassemble a [`ResolutionState`] from its stored `parse_status` columns. The
/// inverse of [`state_to_columns`]; errors (rather than panics) if a variant's
/// required column is absent.
pub fn state_from_columns(
	state: &str,
	phase: Option<&str>,
	content_hash: Option<&[u8]>,
	needed: bool,
	failure: Option<&serde_json::Value>,
) -> Result<ResolutionState, CodecError> {
	match state {
		"unindexed" => Ok(ResolutionState::Unindexed { needed }),
		"progressing" => {
			let phase = phase
				.ok_or(CodecError::MissingColumn { state: "progressing", column: "phase" })?;
			Ok(ResolutionState::Progressing(phase_from_token(phase)?))
		}
		"stored" => {
			let bytes = content_hash
				.ok_or(CodecError::MissingColumn { state: "stored", column: "content_hash" })?;
			Ok(ResolutionState::Stored { hash: hash_from_bytes(bytes)? })
		}
		"failed" | "deadlettered" => {
			let variant = if state == "failed" { "failed" } else { "deadlettered" };
			let json = failure
				.ok_or(CodecError::MissingColumn { state: variant, column: "failure" })?;
			let f: Failure = serde_json::from_value(json.clone())
				.map_err(|source| CodecError::Json { domain: "Failure", source })?;
			Ok(if state == "failed" {
				ResolutionState::Failed(f)
			} else {
				ResolutionState::DeadLettered(f)
			})
		}
		other => Err(CodecError::UnknownStateDiscriminant { token: other.to_owned() }),
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// SearchFacets ↔ jsonb
// ─────────────────────────────────────────────────────────────────────────────

/// Serialize a [`crate::metadata::SearchFacets`] to a `jsonb` value — the
/// facets analog of the `Failure` jsonb encoding in [`state_to_columns`].
/// Malformed serialization surfaces as a codec error (never a panic).
pub fn facets_to_json(
	f: &crate::metadata::SearchFacets,
) -> Result<serde_json::Value, CodecError> {
	serde_json::to_value(f).map_err(|source| CodecError::Json { domain: "SearchFacets", source })
}

/// Deserialize an optional [`crate::metadata::SearchFacets`] from the nullable
/// `parse_status.facets` `jsonb` column. `None` (a `NULL` column) is the common
/// case — a package whose rich metadata was never extracted; a malformed
/// payload is a codec error, exactly as [`state_from_columns`] treats a
/// malformed `failure`.
pub fn facets_from_json(
	v: Option<&serde_json::Value>,
) -> Result<Option<crate::metadata::SearchFacets>, CodecError> {
	match v {
		None => Ok(None),
		Some(value) => serde_json::from_value(value.clone())
			.map(Some)
			.map_err(|source| CodecError::Json { domain: "SearchFacets", source }),
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// Toolchain ↔ jsonb
// ─────────────────────────────────────────────────────────────────────────────

/// Serialize a [`heart::ecosystem::Toolchain`] to a `jsonb` value.
pub fn toolchain_to_json(
	t: &heart::ecosystem::Toolchain,
) -> Result<serde_json::Value, CodecError> {
	serde_json::to_value(t).map_err(|source| CodecError::Json { domain: "Toolchain", source })
}

/// Deserialize a [`heart::ecosystem::Toolchain`] from a `jsonb` value.
pub fn toolchain_from_json(
	v: &serde_json::Value,
) -> Result<heart::ecosystem::Toolchain, CodecError> {
	serde_json::from_value(v.clone())
		.map_err(|source| CodecError::Json { domain: "Toolchain", source })
}

// ─────────────────────────────────────────────────────────────────────────────
// origin / coordinates ↔ columns
// ─────────────────────────────────────────────────────────────────────────────

/// Reconstruct a [`heart::RegistryOrigin`] from its stable token. The public
/// registries map exactly; anything else is a custom origin whose base URL is
/// reconstituted from the token (the token *is* the origin's stable name).
pub fn origin_from_token(token: &str) -> Result<heart::RegistryOrigin, CodecError> {
	match token {
		"crates.io" => Ok(heart::RegistryOrigin::CratesIo),
		"npm" => Ok(heart::RegistryOrigin::NpmPublic),
		"pypi" => Ok(heart::RegistryOrigin::PyPi),
		custom => {
			let url = url::Url::parse(&format!("https://{custom}"))
				.map_err(|source| CodecError::Origin { token: custom.to_owned(), source })?;
			Ok(heart::RegistryOrigin::Custom { name: custom.into(), url })
		}
	}
}

/// Rebuild validated [`crate::package::Coordinates`] from their stored column
/// decomposition — the inverse of the `packages` upsert projection.
/// Re-validation is deliberate: a row that no longer normalizes identically is
/// surfaced as an error, never trusted blindly.
pub fn coordinates_from_columns(
	language: &str,
	origin_token: &str,
	name_original: &str,
	version_canonical: &str,
) -> Result<crate::package::Coordinates, CodecError> {
	let ecosystem = ecosystem_from_token(language)?;
	let origin = origin_from_token(origin_token)?;
	let name = crate::package::PackageName::new(ecosystem, name_original)
		.map_err(CodecError::Name)?;
	let version = heart::PackageVersion::try_from((ecosystem, version_canonical))
		.map_err(CodecError::Version)?;
	Ok(crate::package::Coordinates { origin, name, version })
}
