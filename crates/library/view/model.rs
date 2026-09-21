//! Public immutable view values and query result records.

use crate::canonical::{
    BranchKey, LogKey, PackageKey, SemanticObject, SymbolKey, ViewStateRoot, encode_id,
};
use crate::surface::SemanticLinkKind;
use backend_compile::{DeclarationKind, SourceExcerpt, SourceLocation};
use backend_version::{AuthorizedCompleteCoverage, CoverageWitness, ScopeRoot};
use core::fmt;
use std::sync::Arc;

/// Maximum evidence retained on one local coverage capability and exposed in
/// a producer certificate.
pub const MAX_COVERAGE_EVIDENCE: usize = 64 * 1024;

/// Producer-owned evidence that one exact source scope was observed
/// completely. The library accepts this capability at transition time; it
/// never derives complete authority from a view's own root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageCapability {
    authorized: AuthorizedCompleteCoverage,
    evidence: Arc<[u8]>,
}

impl CoverageCapability {
    /// Wraps coverage that has already crossed the admitted producer
    /// boundary. This is used by trusted protocol adapters after they have
    /// validated a producer certificate.
    /// # Errors
    ///
    /// Returns an error when the retained evidence is oversized or its digest
    /// does not match the evidence checked by the authority.
    pub fn from_authorized_with_evidence(
        coverage: AuthorizedCompleteCoverage,
        evidence: Vec<u8>,
    ) -> Result<Self, String> {
        if evidence.len() > MAX_COVERAGE_EVIDENCE {
            return Err("coverage evidence exceeds the bounded certificate limit".to_owned());
        }
        if *blake3::hash(&evidence).as_bytes() != coverage.evidence_digest() {
            return Err("coverage evidence does not match its admitted digest".to_owned());
        }
        Ok(Self {
            authorized: coverage,
            evidence: Arc::from(evidence),
        })
    }

    /// Returns the exact scope admitted by the producer.
    #[must_use]
    pub fn scope_root(&self) -> ScopeRoot {
        self.authorized.scope_root()
    }

    /// Returns the stable identity of the admitted producer.
    #[must_use]
    pub fn producer_identity(&self) -> [u8; 32] {
        self.authorized.producer_identity()
    }

    /// Returns the exact producer observation context.
    #[must_use]
    pub fn context(&self) -> [u8; 32] {
        self.authorized.context()
    }

    /// Returns the digest of the evidence admitted by the authority.
    #[must_use]
    pub fn evidence_digest(&self) -> [u8; 32] {
        self.authorized.evidence_digest()
    }

    /// Returns the bounded evidence checked by the authority.
    #[must_use]
    pub fn evidence(&self) -> &[u8] {
        &self.evidence
    }

    pub(crate) fn admit_root_claim<R: backend_version::Relation>(
        &self,
        claim: backend_version::UntrustedId<R>,
    ) -> Result<backend_version::StateRoot<R>, backend_version::IdAdmissionError> {
        backend_version::StateRoot::admit_from_producer(claim, &self.authorized)
    }

    pub(crate) fn admit_key_claim<T: backend_version::Schema>(
        &self,
        claim: backend_version::UntrustedId<T>,
    ) -> Result<backend_version::ObjectKey<T>, backend_version::IdAdmissionError> {
        backend_version::ObjectKey::admit_from_producer(claim, &self.authorized)
    }

    pub(super) fn witness(self) -> CoverageWitness {
        CoverageWitness::Complete(self.authorized)
    }
}

/// Source basis claimed by a view and each of its rows.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Basis {
    /// Exact source relation root observed by the view.
    pub root: ViewStateRoot,
    /// Immutable source object/facet version.
    pub object: SemanticObject,
    /// Branch carrying the source root.
    pub branch: BranchKey,
    /// Log carrying the source sequence.
    pub log: LogKey,
    /// Schema version used to interpret source facts.
    pub schema: u16,
}

impl Basis {
    /// Creates a basis with the default product branch and log names.
    #[must_use]
    pub fn new(root: ViewStateRoot, object: SemanticObject) -> Self {
        Self::with_context(
            root,
            object,
            crate::canonical::branch_key("main"),
            crate::canonical::log_key("library"),
            crate::canonical::PROTOCOL_SCHEMA,
        )
    }

    /// Creates a basis with explicit branch, log, and schema bindings.
    #[must_use]
    pub const fn with_context(
        root: ViewStateRoot,
        object: SemanticObject,
        branch: BranchKey,
        log: LogKey,
        schema: u16,
    ) -> Self {
        Self {
            root,
            object,
            branch,
            log,
            schema,
        }
    }
}

/// Whether a view's source is current, stale, or unknown relative to a query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// The view is based on the requested source root.
    Current,
    /// The view is intentionally pinned to an older source root.
    Stale {
        /// Source root observed by the stale view.
        observed: ViewStateRoot,
    },
    /// No source comparison was possible.
    Unknown,
}

/// Retrieval lane identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Lane {
    /// Exact key/address lookup.
    Exact,
    /// Name and outline arrangement.
    Names,
    /// Graph relationship arrangement.
    Graph,
    /// Semantic or embedding arrangement.
    Semantic,
}

/// Why a lane could not produce authoritative rows.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Reason {
    /// No local materialization is available.
    NoIndex,
    /// The deployment did not configure this lane.
    Unconfigured,
    /// Remote work is unavailable while offline.
    Offline,
    /// Work was cancelled before a complete result.
    Cancelled,
    /// The source facts were not complete for this lane.
    Incomplete,
}

/// Honest lane coverage. Unavailable never means an empty complete result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    /// The lane covers its declared source scope.
    Complete,
    /// The lane completed only a bounded portion of its scope.
    Partial {
        /// Lane whose bounded result is represented.
        lane: Lane,
        /// Number of completed shards/lanes.
        completed: u16,
        /// Number of declared shards/lanes.
        total: u16,
    },
    /// The lane could not produce an authoritative result.
    Unavailable {
        /// Lane whose result is unavailable.
        lane: Lane,
        /// Typed reason for unavailability.
        reason: Reason,
    },
}

impl Reason {
    /// Returns whether this lane was never part of the declared scope.
    ///
    /// `Unconfigured` is the one reason that is a fact about the *deployment*
    /// rather than about this revision's work: no timer, retry, or further
    /// indexing will change it, and nothing was left undone. Every other
    /// reason describes work that could have produced rows for this revision
    /// and did not.
    ///
    /// A surface needs this distinction to answer "is this project ready" with
    /// one word. Treating an unconfigured lane as a failure makes an owner
    /// that has completed every lane it has report a fault forever, which is
    /// the same fabricated state as reporting a partial fraction that can
    /// never advance — it just fails in the other direction. The lane itself
    /// must still be *shown* as unavailable with its reason; it simply must
    /// not hold the summary word hostage.
    #[must_use]
    pub const fn is_outside_declared_scope(self) -> bool {
        match self {
            Self::Unconfigured => true,
            Self::NoIndex | Self::Offline | Self::Cancelled | Self::Incomplete => false,
        }
    }
}

impl Coverage {
    /// Returns whether this coverage can claim completeness.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Returns whether this lane failed work it was asked to do.
    ///
    /// A lane the deployment never configured is unavailable but not failed;
    /// see [`Reason::is_outside_declared_scope`].
    #[must_use]
    pub const fn is_failed_lane(self) -> bool {
        match self {
            Self::Unavailable { reason, .. } => !reason.is_outside_declared_scope(),
            Self::Complete | Self::Partial { .. } => false,
        }
    }

    pub(super) fn is_valid(self) -> bool {
        match self {
            Self::Complete | Self::Unavailable { .. } => true,
            Self::Partial {
                completed, total, ..
            } => completed <= total,
        }
    }
}

/// One immutable document fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fragment {
    /// Human-readable prose.
    Text(String),
    /// Source or signature code.
    Code(String),
    /// Link to another stable declaration identity.
    Link {
        /// Display label.
        label: String,
        /// Stable declaration target.
        target: SymbolKey,
    },
    /// Explicit line break.
    Break,
}

/// One declaration document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    /// Stable declaration identity.
    pub symbol: SymbolKey,
    /// Exact source root used to answer the document query.
    pub basis: ViewStateRoot,
    /// Complete source basis when the producer can attest the object, branch,
    /// log, and schema that produced this document.  The root-only field is
    /// retained for compatibility with older in-process constructors; wire
    /// producers should populate this field so a reply cannot silently move
    /// between source streams that happen to share a root digest.
    pub source: Option<Basis>,
    /// Ordered document fragments.
    pub fragments: Box<[Fragment]>,
    /// Optional canonical signature text.
    pub signature: Option<String>,
    /// Availability of the declaration's exact source coordinate.
    pub location: SourceAvailability,
    /// Bounded declaration source text with explicit availability and extent.
    pub excerpt: SourceExcerpt,
}

impl Document {
    /// Creates a document from owned fragments.
    #[must_use]
    pub fn new(
        symbol: SymbolKey,
        basis: ViewStateRoot,
        fragments: impl Into<Box<[Fragment]>>,
    ) -> Self {
        Self {
            symbol,
            basis,
            source: None,
            fragments: fragments.into(),
            signature: None,
            location: SourceAvailability::NotCaptured,
            excerpt: SourceExcerpt::NotCaptured,
        }
    }

    /// Binds this document to the complete producer source basis.
    #[must_use]
    pub fn with_source_basis(mut self, source: Basis) -> Self {
        self.source = Some(source);
        self
    }

    /// Returns a document bound to the exact source root used to answer it.
    #[must_use]
    pub const fn basis(&self) -> ViewStateRoot {
        self.basis
    }

    /// Returns the complete producer source basis, when one was supplied.
    #[must_use]
    pub const fn source_basis(&self) -> Option<Basis> {
        self.source
    }

    /// Attaches the source availability copied from the projected row.
    #[must_use]
    pub fn with_location(mut self, location: SourceAvailability) -> Self {
        self.location = location;
        self
    }

    /// Attaches bounded source text copied from the projected row.
    #[must_use]
    pub fn with_excerpt(mut self, excerpt: SourceExcerpt) -> Self {
        self.excerpt = excerpt;
        self
    }

    /// Returns the deterministic document text projection.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        if let Some(signature) = &self.signature {
            out.push_str(signature);
            out.push('\n');
        }
        for fragment in &self.fragments {
            match fragment {
                Fragment::Text(text) | Fragment::Code(text) => out.push_str(text),
                Fragment::Link { label, .. } => out.push_str(label),
                Fragment::Break => out.push('\n'),
            }
        }
        out
    }
}

/// One declaration in an outline tree.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OutlineNode {
    /// Stable declaration identity.
    pub symbol: SymbolKey,
    /// Ordered child declarations.
    pub children: Box<[OutlineNode]>,
}

/// A package outline.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Outline {
    /// Stable package identity.
    pub package: PackageKey,
    /// Exact source root used to answer the outline query.
    pub basis: ViewStateRoot,
    /// Complete source basis when the producer can attest the source stream.
    pub source: Option<Basis>,
    /// Root outline node.
    pub root: OutlineNode,
    /// Additional top-level declarations in packages whose outline is a forest.
    pub additional_roots: Box<[OutlineNode]>,
    /// Whether this bounded response contains the complete package outline.
    pub extent: OutlineExtent,
}

/// Completeness of one bounded outline response.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum OutlineExtent {
    /// Every retained node was included.
    Complete,
    /// At least one node was omitted by the node or depth bound.
    Truncated,
}

impl Outline {
    /// Creates an outline bound to the exact source root used to answer it.
    #[must_use]
    pub fn new(package: PackageKey, basis: ViewStateRoot, root: OutlineNode) -> Self {
        Self {
            package,
            basis,
            source: None,
            root,
            additional_roots: Box::new([]),
            extent: OutlineExtent::Complete,
        }
    }

    /// Attaches the remaining top-level declarations in canonical order.
    #[must_use]
    pub fn with_additional_roots(mut self, roots: impl Into<Box<[OutlineNode]>>) -> Self {
        self.additional_roots = roots.into();
        self
    }

    /// Marks whether the bounded response is complete.
    #[must_use]
    pub const fn with_extent(mut self, extent: OutlineExtent) -> Self {
        self.extent = extent;
        self
    }

    /// Iterates every top-level declaration without inventing a synthetic root.
    pub fn roots(&self) -> impl Iterator<Item = &OutlineNode> {
        core::iter::once(&self.root).chain(self.additional_roots.iter())
    }

    /// Binds this outline to the complete producer source basis.
    #[must_use]
    pub const fn with_source_basis(mut self, source: Basis) -> Self {
        self.source = Some(source);
        self
    }

    /// Returns the exact source root used to answer this outline query.
    #[must_use]
    pub const fn basis(&self) -> ViewStateRoot {
        self.basis
    }

    /// Returns the complete producer source basis, when one was supplied.
    #[must_use]
    pub const fn source_basis(&self) -> Option<Basis> {
        self.source
    }
}

/// A name arrangement row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NameRecord {
    /// Stable declaration identity.
    pub symbol: SymbolKey,
    /// Stable package identity.
    pub package: PackageKey,
    /// Canonical display name.
    pub name: String,
    /// Declaration kind used by outline/name clients.
    pub kind: String,
}

/// One stable row identity. Position and score are projections, never keys.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RowId {
    /// Package shelf row.
    Package(PackageKey),
    /// Declaration row.
    Symbol(SymbolKey),
    /// Generic immutable object row.
    Object(SemanticObject),
}

impl RowId {
    /// Returns a deterministic relation key used to compute the view root.
    #[must_use]
    pub fn stable_key(self) -> String {
        match self {
            Self::Package(id) => format!("package:{}", encode_id(id.as_bytes())),
            Self::Symbol(id) => format!("symbol:{}", encode_id(id.as_bytes())),
            Self::Object(id) => format!("object:{}", encode_id(id.as_bytes())),
        }
    }
}

/// One typed edge in a compiler-owned graph neighborhood.
///
/// Graph neighborhoods are transported as a row snapshot for compatibility
/// with the existing view protocol. This sidecar keeps the edge authority
/// alongside that snapshot without changing canonical row identity or the
/// view-root commitment. The `from`/`to` endpoints are always row identities
/// admitted by the same snapshot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GraphRelation {
    /// Source declaration of the directed relation.
    pub from: RowId,
    /// Target declaration of the directed relation.
    pub to: RowId,
    /// Compiler-defined semantic relation kind.
    pub relation: SemanticLinkKind,
}

impl GraphRelation {
    /// Creates one typed graph relation.
    #[must_use]
    pub const fn new(from: RowId, to: RowId, relation: SemanticLinkKind) -> Self {
        Self { from, to, relation }
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.stable_key())
    }
}

/// Stable row lifecycle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RowState {
    /// Row content is available.
    Ready,
    /// Row is waiting for a demanded lane.
    Loading,
    /// Row failed with a typed lane result.
    Failed,
}

/// Per-row source evidence. Missing hydration is an ordinary availability state; malformed
/// source identity remains a projection error and cannot be encoded here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceAvailability {
    /// The producer captured an exact source location.
    Captured(SourceLocation),
    /// The producer retained no source span for this declaration.
    NotCaptured,
    /// A source span exists, but its source bytes are not resident locally.
    NotHydrated,
    /// This deployment has no source provider for the row's origin.
    Unconfigured,
}

impl SourceAvailability {
    /// Returns the exact captured location, when locally usable.
    #[must_use]
    pub const fn captured(&self) -> Option<&SourceLocation> {
        match self {
            Self::Captured(location) => Some(location),
            Self::NotCaptured | Self::NotHydrated | Self::Unconfigured => None,
        }
    }
}

/// Compact row projection, with content materialized separately when needed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    /// Stable identity independent of display spelling or rank.
    pub id: RowId,
    /// Exact source basis used for this row.
    pub basis: Basis,
    /// Lifecycle state.
    pub state: RowState,
    /// Display label.
    pub label: String,
    /// Optional score. Score changes do not change row identity.
    pub score: Option<u32>,
    /// Package arrangement owner, when this declaration is attached to a
    /// package.  This relation metadata is part of the immutable row value so
    /// package outlines can be scoped without scanning every symbol.
    pub package: Option<PackageKey>,
    /// Parent declaration in the package outline, when one exists.
    pub parent: Option<SymbolKey>,
    /// Complete immutable document fragments owned by this row.
    pub document: Box<[Fragment]>,
    /// Optional canonical declaration signature.
    pub signature: Option<String>,
    /// Typed declaration kind. Package/object rows intentionally have no kind.
    pub kind: Option<DeclarationKind>,
    /// Exact source path and one-based start line for this declaration.
    pub source: SourceAvailability,
    /// Bounded declaration source text with explicit availability and extent.
    pub excerpt: SourceExcerpt,
}

impl Row {
    /// Creates a ready row with a stable identity and source basis.
    #[must_use]
    pub fn new(id: RowId, basis: Basis, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            id,
            basis,
            state: RowState::Ready,
            label: label.clone(),
            score: None,
            package: None,
            parent: None,
            document: vec![Fragment::Text(label)].into_boxed_slice(),
            signature: None,
            kind: None,
            source: SourceAvailability::NotCaptured,
            excerpt: SourceExcerpt::NotCaptured,
        }
    }

    /// Creates a ready symbol row attached to one package.
    #[must_use]
    pub fn in_package(
        id: RowId,
        basis: Basis,
        package: PackageKey,
        label: impl Into<String>,
    ) -> Self {
        let mut row = Self::new(id, basis, label);
        row.package = Some(package);
        row
    }

    /// Adds a parent declaration to this row's outline relation.
    #[must_use]
    pub const fn with_parent(mut self, parent: SymbolKey) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Replaces the complete immutable document projection for this row.
    #[must_use]
    pub fn with_document(mut self, fragments: impl Into<Box<[Fragment]>>) -> Self {
        self.document = fragments.into();
        self
    }

    /// Attaches the canonical declaration signature for this row.
    #[must_use]
    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }

    /// Attaches a typed declaration kind to this row.
    #[must_use]
    pub const fn with_kind(mut self, kind: DeclarationKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Attaches the exact source location for this row.
    #[must_use]
    pub fn with_source(mut self, source: SourceLocation) -> Self {
        self.source = SourceAvailability::Captured(source);
        self
    }

    /// Attaches bounded declaration source text.
    #[must_use]
    pub fn with_excerpt(mut self, excerpt: SourceExcerpt) -> Self {
        self.excerpt = excerpt;
        self
    }
}
