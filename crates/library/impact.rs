//! Cross-language "what is affected if I change this" contract.
//!
//! One request and one reply shape serve every language. A language plugs into
//! the owner-side framework with an adapter; this module only holds the typed
//! vocabulary shared by the CLI, MCP, and the owner. A reply never turns "no
//! path found" into "no impact": it carries per-domain basis, coverage gaps and
//! freshness next to the rows, and a candidate path is never presented as exact.
use crate::{
    NativeOwnerStatement, PackageReference, ProductAdmissionError, seal_native_owner_statement,
    verify_native_owner_statement,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// Largest page of impact rows one request may ask for.
pub const IMPACT_MAX_PAGE_ROWS: u16 = 200;
/// Rows returned when the caller states no limit.
pub const IMPACT_DEFAULT_PAGE_ROWS: u16 = 40;
/// Deepest causal chain a request may ask for.
pub const IMPACT_MAX_DEPTH: u8 = 6;
/// Depth used when the caller states none.
pub const IMPACT_DEFAULT_DEPTH: u8 = 3;
const MAX_NAME_BYTES: usize = 512;
const MAX_PATH_BYTES: usize = 1024;
const MAX_TOKEN_BYTES: usize = 512;

/// Language domain which owns a seed or a node. SQL registers an adapter for
/// [`ImpactDomain::Sql`]; until it does, a SQL seed reports the domain as not
/// installed rather than an empty impact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDomain {
    /// Stylesheets analyzed by the native CSS engine.
    Css,
    /// Application code (TypeScript, TSX, JSX) which uses other domains.
    Code,
    /// SQL schema objects and queries.
    Sql,
}
impl ImpactDomain {
    /// Stable lowercase label used in requests and rows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Css => "css",
            Self::Code => "code",
            Self::Sql => "sql",
        }
    }
    /// Parse the lowercase label.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "css" => Some(Self::Css),
            "code" => Some(Self::Code),
            "sql" => Some(Self::Sql),
            _ => None,
        }
    }
}

/// Proof strength of a path. A path is as strong as its weakest edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactStrength {
    /// Every edge on the path is a proven relation.
    Exact,
    /// At least one edge is a possible relation whose binding is unproven.
    Candidate,
}

/// Which way to follow relations from the seed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDirection {
    /// What is affected when the seed changes (default).
    Dependents,
    /// What the seed itself relies on.
    Dependencies,
}

/// Row filter. An exact-only filter still reports candidate and unknown counts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactStrengthFilter {
    /// Show exact and candidate rows.
    Any,
    /// Show exact rows; candidates are only counted.
    Exact,
}

/// Seed named by domain, kind and written name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactSeedSpec {
    /// Domain which resolves the name.
    pub domain: ImpactDomain,
    /// Domain-scoped kind. `css`: `class`, `id`, `selector`, `custom-property`,
    /// `at-rule`, `file`. `code`: `component`, `file`. `sql`: `table`, `column`,
    /// `view`, `index`.
    pub kind: String,
    /// Written name, for example `card`, `--brand`, `.card > .title`, `Button`.
    pub name: String,
    /// Optional project-relative file which narrows an ambiguous name.
    pub path: Option<String>,
}

/// Opaque owner-sealed handle copied from a previous impact row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImpactHandle(String);

/// Exact claims behind a handle: one node at one admitted basis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactHandleClaims {
    /// Selected workspace root the node was read from.
    pub manifest_root: [u8; 32],
    /// Project the node belongs to.
    pub project: [u8; 32],
    /// Owning domain.
    pub domain: ImpactDomain,
    /// Adapter-defined stable node key.
    pub node: [u8; 32],
}
impl NativeOwnerStatement for ImpactHandleClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.handle.v1\0";
}
fn domain_byte(domain: ImpactDomain) -> u8 {
    match domain {
        ImpactDomain::Css => 1,
        ImpactDomain::Code => 2,
        ImpactDomain::Sql => 3,
    }
}
fn domain_from_byte(value: u8) -> Result<ImpactDomain, ProductAdmissionError> {
    match value {
        1 => Ok(ImpactDomain::Css),
        2 => Ok(ImpactDomain::Code),
        3 => Ok(ImpactDomain::Sql),
        _ => Err(ProductAdmissionError::SemanticVersionShape),
    }
}
fn decode_token(value: &str) -> Result<Vec<u8>, ProductAdmissionError> {
    if value.len() > MAX_TOKEN_BYTES {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ProductAdmissionError::SemanticVersionShape)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != value {
        return Err(ProductAdmissionError::SemanticVersionShape);
    }
    Ok(bytes)
}
fn array32(bytes: &[u8], at: usize) -> Result<[u8; 32], ProductAdmissionError> {
    bytes
        .get(at..at + 32)
        .and_then(|slice| <[u8; 32]>::try_from(slice).ok())
        .ok_or(ProductAdmissionError::SemanticVersionShape)
}
impl ImpactHandle {
    /// Parse canonical shape without authenticating the seal.
    pub fn parse(value: String) -> Result<Self, ProductAdmissionError> {
        let handle = Self(value);
        handle.decode()?;
        Ok(handle)
    }
    /// Validate shape only.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        self.decode().map(|_| ())
    }
    /// The canonical token text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Seal one node claim.
    pub fn issue(claims: ImpactHandleClaims, secret: &[u8; 32]) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut out = vec![1, 1, domain_byte(claims.domain)];
        for value in [&claims.manifest_root, &claims.project, &claims.node, &seal] {
            out.extend_from_slice(value);
        }
        Ok(Self(URL_SAFE_NO_PAD.encode(out)))
    }
    fn decode(&self) -> Result<(ImpactHandleClaims, [u8; 32]), ProductAdmissionError> {
        let bytes = decode_token(&self.0)?;
        if bytes.len() != 3 + 32 * 4 || bytes.get(..2) != Some(&[1u8, 1][..]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let domain = domain_from_byte(bytes[2])?;
        Ok((
            ImpactHandleClaims {
                manifest_root: array32(&bytes, 3)?,
                project: array32(&bytes, 35)?,
                domain,
                node: array32(&bytes, 67)?,
            },
            array32(&bytes, 99)?,
        ))
    }
    /// Authenticate the claim.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<ImpactHandleClaims, ProductAdmissionError> {
        let (claims, seal) = self.decode()?;
        verify_native_owner_statement(secret, &claims, &seal)?;
        Ok(claims)
    }
}

/// What to start from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum ImpactSeed {
    /// A written name in a domain.
    Named(ImpactSeedSpec),
    /// A handle copied from a previous reply.
    Handle {
        /// Sealed node handle.
        handle: ImpactHandle,
    },
}

/// Owner-sealed continuation: the exact request recipe and a row offset.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImpactCursor(String);
/// Claims sealed into a continuation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactCursorClaims {
    /// Selected workspace root the first page was read from.
    pub manifest_root: [u8; 32],
    /// Digest of the complete request recipe except the cursor itself.
    pub recipe: [u8; 32],
    /// Rows already returned.
    pub offset: u32,
}
impl NativeOwnerStatement for ImpactCursorClaims {
    const DOMAIN: &'static [u8] = b"backend.impact.cursor.v1\0";
}
impl ImpactCursor {
    /// Parse canonical shape without authenticating the seal.
    pub fn parse(value: String) -> Result<Self, ProductAdmissionError> {
        let cursor = Self(value);
        cursor.decode()?;
        Ok(cursor)
    }
    /// Validate shape only.
    pub fn admit_shape(&self) -> Result<(), ProductAdmissionError> {
        self.decode().map(|_| ())
    }
    /// The canonical token text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Seal one continuation.
    pub fn issue(claims: ImpactCursorClaims, secret: &[u8; 32]) -> Result<Self, ProductAdmissionError> {
        let seal = seal_native_owner_statement(secret, &claims)?;
        let mut out = vec![1, 2];
        out.extend_from_slice(&claims.manifest_root);
        out.extend_from_slice(&claims.recipe);
        out.extend_from_slice(&claims.offset.to_be_bytes());
        out.extend_from_slice(&seal);
        Ok(Self(URL_SAFE_NO_PAD.encode(out)))
    }
    fn decode(&self) -> Result<(ImpactCursorClaims, [u8; 32]), ProductAdmissionError> {
        let bytes = decode_token(&self.0)?;
        if bytes.len() != 2 + 32 + 32 + 4 + 32 || bytes.get(..2) != Some(&[1u8, 2][..]) {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        let offset = u32::from_be_bytes(
            bytes
                .get(66..70)
                .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
                .ok_or(ProductAdmissionError::SemanticVersionShape)?,
        );
        Ok((
            ImpactCursorClaims {
                manifest_root: array32(&bytes, 2)?,
                recipe: array32(&bytes, 34)?,
                offset,
            },
            array32(&bytes, 70)?,
        ))
    }
    /// Authenticate the claim.
    pub fn verify(&self, secret: &[u8; 32]) -> Result<ImpactCursorClaims, ProductAdmissionError> {
        let (claims, seal) = self.decode()?;
        verify_native_owner_statement(secret, &claims, &seal)?;
        Ok(claims)
    }
}

/// One bounded impact question.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactRequest {
    /// Selected local project label.
    pub package: PackageReference,
    /// The thing being changed.
    pub seed: ImpactSeed,
    /// Follow dependents (default) or dependencies.
    pub direction: ImpactDirection,
    /// Longest causal chain to follow.
    pub depth: u8,
    /// Row filter.
    pub strength: ImpactStrengthFilter,
    /// Rows in this page.
    pub page_rows: u16,
    /// Continuation from the preceding page.
    pub continuation: Option<ImpactCursor>,
}
fn bounded_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.contains('\0')
}
impl ImpactRequest {
    /// Build a request with the product defaults.
    #[must_use]
    pub fn new(package: PackageReference, seed: ImpactSeed) -> Self {
        Self {
            package,
            seed,
            direction: ImpactDirection::Dependents,
            depth: IMPACT_DEFAULT_DEPTH,
            strength: ImpactStrengthFilter::Any,
            page_rows: IMPACT_DEFAULT_PAGE_ROWS,
            continuation: None,
        }
    }
    /// Check request bounds. This does not authenticate a handle or cursor.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if !matches!(self.package, PackageReference::Local(_))
            || self.depth == 0
            || self.depth > IMPACT_MAX_DEPTH
            || self.page_rows == 0
            || self.page_rows > IMPACT_MAX_PAGE_ROWS
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        match &self.seed {
            ImpactSeed::Named(spec) => {
                if !bounded_text(&spec.kind, 64)
                    || !bounded_text(&spec.name, MAX_NAME_BYTES)
                    || spec.path.as_deref().is_some_and(|path| !bounded_text(path, MAX_PATH_BYTES))
                {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
            }
            ImpactSeed::Handle { handle } => handle.admit_shape()?,
        }
        if let Some(cursor) = &self.continuation {
            cursor.admit_shape()?;
        }
        Ok(())
    }
    /// Digest of everything which determines the ordered row list. A cursor
    /// from another recipe is refused instead of silently skipping rows.
    #[must_use]
    pub fn recipe(&self) -> [u8; 32] {
        let mut recipe = self.clone();
        recipe.continuation = None;
        recipe.page_rows = 0;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.impact.recipe.v1\0");
        hasher.update(&serde_json::to_vec(&recipe).unwrap_or_default());
        *hasher.finalize().as_bytes()
    }
}

/// A resolved or candidate node as shown to the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactNodeView {
    /// Owning domain.
    pub domain: ImpactDomain,
    /// Domain-scoped kind, for example `rule`, `class`, `custom-property`.
    pub kind: String,
    /// Human-readable name.
    pub name: String,
    /// Project-relative file, where the node has one.
    pub path: Option<String>,
    /// One-based line, where known.
    pub line: Option<u32>,
    /// Enclosing component, function or at-rule context, when lifted.
    pub within: Option<String>,
    /// Sealed handle for a follow-up question about this node.
    pub handle: Option<ImpactHandle>,
}

/// How a seed name resolved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImpactSeedState {
    /// The seed names one or more definitions; all were followed.
    Resolved,
    /// The name matches several kinds or files; none was followed. Repeat with a handle or path.
    Ambiguous,
    /// The analyzed scope holds no such name. Not evidence that the name is absent from unanalyzed sources.
    NotFound {
        /// Why this is not a proof of absence.
        why: String,
    },
    /// The domain which owns the seed cannot answer, so nothing was searched.
    Unavailable {
        /// Why the domain is unavailable.
        why: String,
    },
}

/// One affected node with the shortest explanation the framework found.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactRow {
    /// One-based row position in the full ordered result, stable across pages.
    pub n: u32,
    /// Edges from the seed.
    pub depth: u8,
    /// Weakest proof along the shown path.
    pub strength: ImpactStrength,
    /// The affected node.
    pub node: ImpactNodeView,
    /// Relation on the last edge, for example `css.reads-property`.
    pub relation: String,
    /// Short human reason naming the last edge's evidence.
    pub why: String,
    /// `n` of the row this one was reached from; zero means the seed.
    pub via: u32,
    /// Executable follow-up which reads the exact source, when one exists.
    pub read: Option<String>,
}

/// Domain-level availability, separate from row counts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImpactDomainState {
    /// The selected analysis was read. Read `coverage` for what it cannot see.
    Analyzed,
    /// Part of the domain was read; the listed reasons say which part is missing.
    Partial {
        /// What is missing.
        reasons: Vec<String>,
    },
    /// Nothing from this domain could be read.
    Unavailable {
        /// What is missing.
        reason: String,
    },
    /// No adapter for this domain is installed in this build.
    NotInstalled,
}

/// Basis of one domain, read at the selected workspace root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDomainBasis {
    /// Domain.
    pub domain: ImpactDomain,
    /// Availability.
    pub state: ImpactDomainState,
    /// Files this domain contributed.
    pub files: u32,
}

/// One named blind spot. Every gap that could hide an affected node is listed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactGap {
    /// Stable facet, for example `css.cascade`, `code.dynamic-class`.
    pub facet: String,
    /// Plain statement of what cannot be seen.
    pub note: String,
    /// How many sites or files the gap touches, when counted.
    pub count: Option<u32>,
}

/// Whether the analyzed files still match the disk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImpactFreshness {
    /// Every checked file matches the bytes which were indexed.
    Current {
        /// Files compared.
        checked: u32,
    },
    /// Some files changed or vanished after indexing. Rows from them may be wrong.
    Stale {
        /// Files compared.
        checked: u32,
        /// Changed or missing project-relative paths, at most a handful.
        changed: Vec<String>,
        /// Concrete next action.
        action: String,
    },
    /// Freshness could not be checked.
    Unknown {
        /// Why.
        why: String,
    },
}

/// Counts which stay true under any row filter or paging.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactTotals {
    /// Affected nodes reached by an exact path.
    pub exact: u32,
    /// Affected nodes reached only by candidate paths.
    pub candidate: u32,
    /// Distinct files among all affected nodes.
    pub files: u32,
    /// Counts by node kind, sorted by kind.
    pub by_kind: Vec<(String, u32)>,
}

/// Why enumeration ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ImpactTerminal {
    /// Every reachable node within the requested depth was enumerated.
    Complete,
    /// More rows follow.
    More {
        /// Continuation for the same recipe and workspace root.
        cursor: ImpactCursor,
    },
    /// A computation budget stopped traversal. The row list is a lower bound.
    BudgetLimited {
        /// Which budget.
        budget: String,
    },
}

/// The answer to one impact question.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactReport {
    /// Short identity of the selected workspace root.
    pub basis: String,
    /// Domain availability.
    pub domains: Vec<ImpactDomainBasis>,
    /// How the seed resolved.
    pub seed_state: ImpactSeedState,
    /// Resolved seed definitions, or the candidates when ambiguous.
    pub seeds: Vec<ImpactNodeView>,
    /// Requested direction, echoed.
    pub direction: ImpactDirection,
    /// Requested depth, echoed.
    pub depth: u8,
    /// Requested filter, echoed.
    pub strength_filter: ImpactStrengthFilter,
    /// Totals over every affected node, before the row filter and paging.
    pub totals: ImpactTotals,
    /// The page of rows.
    pub rows: Vec<ImpactRow>,
    /// Blind spots which could hide affected nodes.
    pub gaps: Vec<ImpactGap>,
    /// Freshness of the analyzed files.
    pub freshness: ImpactFreshness,
    /// Enumeration state.
    pub terminal: ImpactTerminal,
    /// Concrete next actions.
    pub next: Vec<String>,
}
impl ImpactReport {
    /// Whether this answer proves anything about absence of impact. It never
    /// does while a domain is unavailable, a gap is listed or traversal stopped.
    #[must_use]
    pub fn proves_no_impact(&self) -> bool {
        matches!(self.seed_state, ImpactSeedState::Resolved)
            && self.totals.exact == 0
            && self.totals.candidate == 0
            && self.gaps.is_empty()
            && matches!(self.terminal, ImpactTerminal::Complete)
            && matches!(self.freshness, ImpactFreshness::Current { .. })
            && self.domains.iter().all(|d| matches!(d.state, ImpactDomainState::Analyzed))
    }
    /// Check reply self-consistency before it leaves the owner.
    pub fn admit(&self) -> Result<usize, ProductAdmissionError> {
        let mut last = 0;
        for row in &self.rows {
            if row.n <= last || row.depth == 0 || row.depth > IMPACT_MAX_DEPTH || row.via >= row.n {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            if self.strength_filter == ImpactStrengthFilter::Exact && row.strength != ImpactStrength::Exact {
                return Err(ProductAdmissionError::SemanticVersionShape);
            }
            if let Some(handle) = &row.node.handle {
                handle.admit_shape()?;
            }
            last = row.n;
        }
        if self.rows.len() > usize::from(IMPACT_MAX_PAGE_ROWS) {
            return Err(ProductAdmissionError::RowBound);
        }
        if let ImpactTerminal::More { cursor } = &self.terminal {
            cursor.admit_shape()?;
        }
        for seed in &self.seeds {
            if let Some(handle) = &seed.handle {
                handle.admit_shape()?;
            }
        }
        // A "no such name" or "unavailable" answer cannot carry impact rows.
        if !matches!(self.seed_state, ImpactSeedState::Resolved)
            && (!self.rows.is_empty() || self.totals.exact + self.totals.candidate != 0)
        {
            return Err(ProductAdmissionError::SemanticVersionShape);
        }
        Ok(self.rows.len())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn package() -> PackageReference {
        PackageReference::parse("app".to_owned()).expect("local package")
    }

    #[test]
    fn handle_and_cursor_authenticate_their_claims() {
        let secret = [3; 32];
        let claims = ImpactHandleClaims {
            manifest_root: [1; 32],
            project: [2; 32],
            domain: ImpactDomain::Css,
            node: [4; 32],
        };
        let handle = ImpactHandle::issue(claims.clone(), &secret).expect("issue");
        assert_eq!(handle.verify(&secret).expect("verify"), claims);
        assert!(handle.verify(&[9; 32]).is_err());
        let cursor = ImpactCursor::issue(
            ImpactCursorClaims { manifest_root: [1; 32], recipe: [5; 32], offset: 40 },
            &secret,
        )
        .expect("issue cursor");
        assert_eq!(cursor.verify(&secret).expect("verify cursor").offset, 40);
        assert!(ImpactCursor::parse("not-a-token".into()).is_err());
        assert!(ImpactHandle::parse(cursor.as_str().to_owned()).is_err());
    }

    #[test]
    fn recipe_ignores_paging_but_not_filters() {
        let seed = ImpactSeed::Named(ImpactSeedSpec {
            domain: ImpactDomain::Css,
            kind: "class".into(),
            name: "card".into(),
            path: None,
        });
        let base = ImpactRequest::new(package(), seed);
        let mut paged = base.clone();
        paged.page_rows = 7;
        assert_eq!(base.recipe(), paged.recipe());
        let mut deeper = base.clone();
        deeper.depth = 5;
        assert_ne!(base.recipe(), deeper.recipe());
        let mut exact = base.clone();
        exact.strength = ImpactStrengthFilter::Exact;
        assert_ne!(base.recipe(), exact.recipe());
    }

    #[test]
    fn admission_bounds_every_request_field() {
        let seed = ImpactSeed::Named(ImpactSeedSpec {
            domain: ImpactDomain::Css,
            kind: "class".into(),
            name: "card".into(),
            path: None,
        });
        let ok = ImpactRequest::new(package(), seed.clone());
        assert!(ok.admit().is_ok());
        for mutate in [
            |r: &mut ImpactRequest| r.depth = 0,
            |r: &mut ImpactRequest| r.depth = IMPACT_MAX_DEPTH + 1,
            |r: &mut ImpactRequest| r.page_rows = 0,
            |r: &mut ImpactRequest| r.page_rows = IMPACT_MAX_PAGE_ROWS + 1,
        ] {
            let mut request = ok.clone();
            mutate(&mut request);
            assert!(request.admit().is_err());
        }
        let mut blank = ok.clone();
        blank.seed = ImpactSeed::Named(ImpactSeedSpec {
            domain: ImpactDomain::Css,
            kind: "class".into(),
            name: String::new(),
            path: None,
        });
        assert!(blank.admit().is_err());
    }

    #[test]
    fn an_unresolved_seed_cannot_carry_rows_or_prove_absence() {
        let mut report = ImpactReport {
            basis: "abc".into(),
            domains: vec![ImpactDomainBasis {
                domain: ImpactDomain::Css,
                state: ImpactDomainState::Unavailable { reason: "no CSS worker".into() },
                files: 0,
            }],
            seed_state: ImpactSeedState::Unavailable { why: "no CSS worker".into() },
            seeds: Vec::new(),
            direction: ImpactDirection::Dependents,
            depth: 3,
            strength_filter: ImpactStrengthFilter::Any,
            totals: ImpactTotals { exact: 0, candidate: 0, files: 0, by_kind: Vec::new() },
            rows: Vec::new(),
            gaps: Vec::new(),
            freshness: ImpactFreshness::Unknown { why: "no analysis".into() },
            terminal: ImpactTerminal::Complete,
            next: Vec::new(),
        };
        assert!(report.admit().is_ok());
        assert!(!report.proves_no_impact());
        report.totals.exact = 1;
        assert!(report.admit().is_err());
    }
}
