//! Exact request roots and reusable output proof.

use super::{coverage, version};
use crate::{
    AuthorityScope, AuthorityScopeVersion, RecipeVersion, ScopedReadManifest,
    ScopedReadManifestVersion, SemanticError,
};
use backend_version::{
    AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion, Relation, StateRoot,
};
use std::fmt;

/// Canonical schema for a checked reuse request.
#[derive(Debug)]
pub struct ReuseRequestSchema;

impl backend_version::Schema for ReuseRequestSchema {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 0x0032;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Version of a checked reuse request.
pub type ReuseRequestVersion = ObjectVersion<ReuseRequestSchema>;

/// A complete version-bound request for one reusable output.
pub struct ReuseRequest<R: Relation> {
    recipe: RecipeVersion,
    input: StateRoot<R>,
    read_manifest: ScopedReadManifestVersion,
    authority: AuthorityScopeVersion,
    coverage: AuthorizedCompleteCoverage,
}

impl<R: Relation> fmt::Debug for ReuseRequest<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReuseRequest")
            .field("recipe", &self.recipe)
            .field("input", &self.input)
            .field("read_manifest", &self.read_manifest)
            .field("authority", &self.authority)
            .field("coverage", &self.coverage)
            .finish()
    }
}

impl<R: Relation> Copy for ReuseRequest<R> {}

impl<R: Relation> Clone for ReuseRequest<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> PartialEq for ReuseRequest<R> {
    fn eq(&self, other: &Self) -> bool {
        self.recipe == other.recipe
            && self.input == other.input
            && self.read_manifest == other.read_manifest
            && self.authority == other.authority
            && self.coverage == other.coverage
    }
}

impl<R: Relation> Eq for ReuseRequest<R> {}

impl<R: Relation> ReuseRequest<R> {
    /// Creates a request after validating every read and the authority scope.
    ///
    /// A complete positive read must retain its observed value version.  A
    /// complete negative read must retain its complete coverage witness.  A
    /// request with incomplete observations cannot enter the proof path.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ScopeMismatch`] when coverage names another
    /// authority scope or [`SemanticError::IncompleteCoverage`] when a read
    /// lacks complete coverage or a positive value version.
    pub fn new(
        recipe: RecipeVersion,
        input: StateRoot<R>,
        reads: &ScopedReadManifest,
        authority: &AuthorityScope,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if coverage.scope_root() != authority.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        for observation in reads.observations() {
            if observation.read().scope_root() != authority.scope_root()
                || observation.coverage().scope_root() != authority.scope_root()
            {
                return Err(SemanticError::ScopeMismatch);
            }
            if !matches!(observation.coverage(), CoverageWitness::Complete(_))
                || (!observation.read().is_negative() && observation.version().is_none())
            {
                return Err(SemanticError::IncompleteCoverage);
            }
        }
        Ok(Self {
            recipe,
            input,
            read_manifest: reads.version(),
            authority: authority.version(),
            coverage,
        })
    }

    /// Returns the recipe root bound to this request.
    #[must_use]
    pub const fn recipe(&self) -> RecipeVersion {
        self.recipe
    }

    /// Returns the exact input relation root.
    #[must_use]
    pub const fn input(&self) -> StateRoot<R> {
        self.input
    }

    /// Returns the exact read-manifest root.
    #[must_use]
    pub const fn read_manifest(&self) -> ScopedReadManifestVersion {
        self.read_manifest
    }

    /// Returns the exact authority-scope root.
    #[must_use]
    pub const fn authority(&self) -> AuthorityScopeVersion {
        self.authority
    }

    /// Returns the complete authority coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> AuthorizedCompleteCoverage {
        self.coverage
    }

    /// Returns the canonical request bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![1];
        version(&mut out, &self.recipe.to_bytes());
        version(&mut out, &self.input.to_bytes());
        version(&mut out, &self.read_manifest.to_bytes());
        version(&mut out, &self.authority.to_bytes());
        coverage(&mut out, CoverageWitness::Complete(self.coverage));
        out
    }

    /// Returns the immutable request version.
    #[must_use]
    pub fn version(&self) -> ReuseRequestVersion {
        ObjectVersion::from_value(&self.canonical_bytes())
    }
}

/// An immutable output retained for a complete reuse request.
pub struct ReusableOutput<I: Relation, O: Relation> {
    request: ReuseRequest<I>,
    output: StateRoot<O>,
}

impl<I: Relation, O: Relation> fmt::Debug for ReusableOutput<I, O> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReusableOutput")
            .field("request", &self.request)
            .field("output", &self.output)
            .finish()
    }
}

impl<I: Relation, O: Relation> Copy for ReusableOutput<I, O> {}

impl<I: Relation, O: Relation> Clone for ReusableOutput<I, O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I: Relation, O: Relation> PartialEq for ReusableOutput<I, O> {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request && self.output == other.output
    }
}

impl<I: Relation, O: Relation> Eq for ReusableOutput<I, O> {}

impl<I: Relation, O: Relation> ReusableOutput<I, O> {
    /// Binds an immutable output root to its exact request.
    #[must_use]
    pub const fn new(request: ReuseRequest<I>, output: StateRoot<O>) -> Self {
        Self { request, output }
    }

    /// Returns the exact request used to produce this output.
    #[must_use]
    pub const fn request(&self) -> &ReuseRequest<I> {
        &self.request
    }

    /// Returns the immutable output root.
    #[must_use]
    pub const fn output(&self) -> StateRoot<O> {
        self.output
    }
}

/// A checked proof that a retained output answers a request exactly.
pub struct ReuseProof<I: Relation, O: Relation> {
    request: ReuseRequest<I>,
    output: StateRoot<O>,
}

impl<I: Relation, O: Relation> fmt::Debug for ReuseProof<I, O> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReuseProof")
            .field("request", &self.request)
            .field("output", &self.output)
            .finish()
    }
}

impl<I: Relation, O: Relation> Copy for ReuseProof<I, O> {}

impl<I: Relation, O: Relation> Clone for ReuseProof<I, O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I: Relation, O: Relation> PartialEq for ReuseProof<I, O> {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request && self.output == other.output
    }
}

impl<I: Relation, O: Relation> Eq for ReuseProof<I, O> {}

impl<I: Relation, O: Relation> ReuseProof<I, O> {
    /// Returns the request roots covered by the proof.
    #[must_use]
    pub const fn request(&self) -> &ReuseRequest<I> {
        &self.request
    }

    /// Returns the exact output root covered by the proof.
    #[must_use]
    pub const fn output(&self) -> StateRoot<O> {
        self.output
    }

    /// Verifies that the proof still names the supplied request and output.
    #[must_use]
    pub fn matches(&self, request: &ReuseRequest<I>, output: StateRoot<O>) -> bool {
        &self.request == request && self.output == output
    }
}

/// Checks a candidate against the complete recipe/input/read/authority roots.
///
/// # Errors
///
/// Returns [`SemanticError::ReuseMismatch`] when any request root differs.
pub fn check_reuse<I: Relation, O: Relation>(
    request: &ReuseRequest<I>,
    candidate: &ReusableOutput<I, O>,
) -> Result<ReuseProof<I, O>, SemanticError> {
    if candidate.request() != request {
        return Err(SemanticError::ReuseMismatch);
    }
    Ok(ReuseProof {
        request: *request,
        output: candidate.output(),
    })
}
