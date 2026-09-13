/// Invalid workspace manifest or transition descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceError {
    /// Base and target use different manifest schema versions.
    SchemaMismatch,
    /// Base and target select different relation schema sets.
    RelationSetMismatch,
    /// Relation bindings are unordered or duplicated.
    NonCanonicalRelations,
    /// Basis bindings are unordered or duplicated.
    NonCanonicalBasis,
    /// A transition binding is duplicated or out of order.
    DuplicateRelation,
    /// A changed relation has no transition descriptor.
    MissingTransition,
    /// An unchanged relation was accompanied by a transition descriptor.
    UnexpectedTransition,
    /// A transition does not match the manifests.
    TransitionMismatch,
    /// Basis roots changed without a transition lane.
    BasisMismatch,
    /// Authority identity changed without a transition lane.
    AuthorityMismatch,
    /// Coverage changed without a transition lane.
    CoverageMismatch,
    /// The coverage label was reconstructed from an untrusted descriptor.
    UnverifiedCoverage,
    /// A raw wire descriptor was used where a typed manifest closure was
    /// required.
    UnverifiedManifest,
    /// A relation root was not derived from a checked typed relation state.
    UnverifiedRelation,
    /// A relation state was canonical but did not carry complete authority
    /// coverage, so it cannot close an authoritative workspace commit.
    IncompleteRelation,
    /// A basis root was not derived from a checked typed object version.
    UnverifiedBasis,
    /// The authority root was not derived from a checked typed object version.
    UnverifiedAuthority,
    /// A type-erased transition failed its canonical delta identity check.
    InvalidTransition,
    /// A relation transition was decoded but never admitted against its
    /// exact typed delta.
    UnverifiedTransition,
    /// Provenance bytes did not match the supplied typed closure objects.
    ProvenanceMismatch,
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid workspace descriptor: {self:?}")
    }
}

impl std::error::Error for WorkspaceError {}
