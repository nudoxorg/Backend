//! The one query algebra (INDEX-PLAN ID-6, §9).
//!
//! Every search surface — server HTTP, GUI client, embedded engine — speaks
//! this domain type directly. There is no separate wire DTO: the wire *is*
//! the domain. Pagination happens only via [`PageSpecification`] inside the
//! engine, after ranking.
//!
//! # Layering
//!
//! `heart` sits below the vector plane, so the vector-routing knobs a caller
//! states ([`QueryMode`], [`QualityMode`], [`QueryReach`]) are owned *here* as
//! plain wire enums. The `vector_core` and `server` crates map from these into
//! their internal routing vocabulary — the dependency arrow never points back
//! into `heart`.

use serde::{Deserialize, Serialize};

use crate::ecosystem::Language;
use crate::identity::PackageId;
use crate::search::Page;

/// Milliseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnixMilliseconds(pub i64);

/// A DoltLite catalog commit hash (hex), pinning a metadata snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CatalogCommitHash(pub String);

/// A point-in-history selector for catalog reads (INDEX-PLAN §9, §12).
///
/// Prefer [`AsOf::Time`] externally: commit pins are only guaranteed stable
/// for ninety days on `main` (INDEX-PLAN §12 commit-pin policy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsOf {
    /// Pin to an exact catalog commit.
    Commit(CatalogCommitHash),
    /// Resolve to the newest commit at or before this instant.
    Time(UnixMilliseconds),
}

/// A foreign symbol reference in the frozen F1 grammar `F:<eco>/<pkg>#<hex>`.
///
/// The `<pkg>` field may itself contain `/` (registryless repo-slug stems,
/// REGISTRYLESS-PLAN RL-10); `#` is the sole terminator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StableReference {
    ecosystem: String,
    package: String,
    intro_hex: String,
}

/// Why a [`StableReference`] failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StableReferenceError {
    #[error("stable reference must start with `F:`")]
    MissingPrefix,
    #[error("stable reference has no `#` terminator")]
    MissingTerminator,
    #[error("stable reference has no `/` between ecosystem and package")]
    MissingEcosystemSeparator,
    #[error("stable reference component is empty")]
    EmptyComponent,
    #[error("stable reference intro id is not lowercase hex")]
    MalformedIntro,
}

impl StableReference {
    /// Parse the frozen grammar `F:<eco>/<pkg>#<hex>`.
    pub fn parse(raw: &str) -> Result<Self, StableReferenceError> {
        let body = raw.strip_prefix("F:").ok_or(StableReferenceError::MissingPrefix)?;
        let (coordinate, intro_hex) = body
            .rsplit_once('#')
            .ok_or(StableReferenceError::MissingTerminator)?;
        let (ecosystem, package) = coordinate
            .split_once('/')
            .ok_or(StableReferenceError::MissingEcosystemSeparator)?;
        if ecosystem.is_empty() || package.is_empty() || intro_hex.is_empty() {
            return Err(StableReferenceError::EmptyComponent);
        }
        if !intro_hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(StableReferenceError::MalformedIntro);
        }
        Ok(Self {
            ecosystem: ecosystem.to_owned(),
            package: package.to_owned(),
            intro_hex: intro_hex.to_owned(),
        })
    }

    /// The ecosystem component (`rust`, `cpp`, …).
    pub fn ecosystem(&self) -> &str {
        &self.ecosystem
    }

    /// The package stem, which may itself contain `/` (RL-10).
    pub fn package(&self) -> &str {
        &self.package
    }

    /// The lowercase-hex intro id.
    pub fn intro_hex(&self) -> &str {
        &self.intro_hex
    }
}

impl std::fmt::Display for StableReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "F:{}/{}#{}", self.ecosystem, self.package, self.intro_hex)
    }
}

impl TryFrom<String> for StableReference {
    type Error = StableReferenceError;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl From<StableReference> for String {
    fn from(reference: StableReference) -> Self {
        reference.to_string()
    }
}

/// What a [`Query`] is asking for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Rank packages by the fused relevance formula (INDEX-PLAN ID-8).
    Packages,
    /// Rank symbols (moniker + kind projections of the IR plane).
    Symbols,
    /// Every recorded use of one symbol, from the reverse `occ` index.
    Usages { of: StableReference },
}

/// Which slice of the universe a [`Query`] may see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Restrict to these ecosystems; empty means all.
    pub ecosystems: Vec<Language>,
    /// Restrict to these package stems (canonical names); empty means all.
    pub packages: Vec<String>,
    /// Include versions withdrawn or yanked upstream.
    pub include_withdrawn: bool,
}

/// Ranking directives. Fusion weights are frozen (INDEX-PLAN ID-8); callers
/// only choose coarse posture, never weights.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RankSpecification {
    /// The frozen within-ecosystem fusion formula.
    #[default]
    Fused,
    /// Pure lexical relevance (debugging / eval harness only).
    TextOnly,
}

/// How a [`Query`] wants its text interpreted (09-vector §20).
///
/// This is the single semantic opt-in: `Precise` is the default lexical
/// (tantivy) surface; `Semantic` requests the embed-and-match path, which the
/// engine's planner still gates against its own semantic budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    /// Lexical, keyset-precise search — the default surface.
    #[default]
    Precise,
    /// Natural-language / snippet semantic search (planner-gated).
    Semantic,
}

/// The quality tier a client requests for the dense stage (09-vector §20.5).
///
/// Owned in `heart` so the wire contract stays decoupled from the vector-core
/// vocabulary it lowers into; `vector::routing::QualityMode` maps `From`
/// this. Absent on the wire → [`QualityMode::Parity`] (a serving request is
/// online by definition; `Local` is the client-side default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QualityMode {
    /// Client-local dense stage only; the server runs no dense stage.
    Local,
    /// The parity (compiled-in) embedding collection.
    Parity,
    /// The premium embedding collection.
    Premium,
    /// Premium (parity fallback) plus the deep cross-encoder rerank.
    Deep,
}

impl Default for QualityMode {
    fn default() -> Self {
        Self::Parity
    }
}

/// What slice of the world a dense query addresses (09-vector §20.5).
///
/// Named `QueryReach` in the wire algebra to avoid colliding with [`Scope`]
/// (the ecosystem/package narrowing); `vector::routing::QueryScope` maps
/// `From` this. Absent on the wire → [`QueryReach::Org`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryReach {
    /// The caller's own project.
    Project,
    /// The caller's dependency closure.
    Deps,
    /// The whole organization index.
    Org,
}

impl Default for QueryReach {
    fn default() -> Self {
        Self::Org
    }
}

/// The vector-plane routing knobs (09-vector §20.5).
///
/// A flat sub-record on [`Query`]; every field defaults, so a minimal
/// `{ "target": …, "text": … }` body deserializes with serving defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    /// The requested quality tier for the dense stage.
    #[serde(default)]
    pub quality: QualityMode,
    /// What slice of the world the dense query addresses.
    #[serde(default)]
    pub reach: QueryReach,
    /// Dep packages the client **claims** it serves from local baked shards
    /// (§20.5); the server excludes them from its dense Stage-1 rather than
    /// double-answering. Claimed, never verified — a false claim only narrows
    /// that client's own recall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hot_packages: Vec<PackageId>,
}

/// Page shape. The engine applies this after ranking; no other layer paginates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSpecification {
    /// Maximum hits to return; engines clamp to their own ceiling.
    pub limit: u32,
    /// Opaque resume token from [`Page::next`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl Default for PageSpecification {
    fn default() -> Self {
        Self { limit: 30, cursor: None }
    }
}

/// The single search request shape (wire = domain).
///
/// A HTTP handler deserializes this directly; there is no `SearchRequestDto`
/// wrapper. Every optional field defaults, so the minimal wire body is
/// `{ "target": …, "text": … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    /// What kind of result the caller wants.
    pub target: Target,
    /// The raw query text (package/symbol terms, or the seed for a usage walk).
    pub text: String,
    /// Ecosystem / package narrowing and withdrawn-inclusion.
    #[serde(default)]
    pub scope: Scope,
    /// Coarse ranking posture (weights are frozen, ID-8).
    #[serde(default)]
    pub rank: RankSpecification,
    /// How the text is interpreted: lexical (default) or semantic.
    #[serde(default)]
    pub mode: QueryMode,
    /// Vector-plane routing knobs (quality tier, reach, claimed hot-set).
    #[serde(default)]
    pub routing: Routing,
    /// The caller's exploration session, when results should accumulate into a
    /// session graph (the expand surface).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::Guid>,
    /// Prefer [`AsOf::Time`] externally (INDEX-PLAN §12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<AsOf>,
    /// Page shape; the engine paginates after ranking.
    #[serde(default)]
    pub page: PageSpecification,
}

/// The engine-side entry point every deployment shape implements.
pub trait QueryEngine: Send + Sync {
    /// The hit type this engine pages out.
    type Hit;
    /// The engine's failure type.
    type Error;

    /// Run one query, returning a ranked, engine-paginated page of hits.
    fn search(&self, query: &Query) -> Result<Page<Self::Hit>, Self::Error>;
}
