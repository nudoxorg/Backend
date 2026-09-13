//! Version-bound read, authority, and recipe dependency facts.

use super::graph::has_recipe_cycle;
use super::{coverage, frame, version};
use crate::canonical::canonical_scoped_observation;
use crate::{
    AuthorityScope, AuthorityScopeVersion, AuthorityVersion, FacetKind, FacetVersion, ReadSelector,
    RecipeVersion, ScopedRead, ScopedReadManifest, ScopedReadObservation, SemanticError,
};
use backend_version::{
    AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion, Schema, ScopeRoot,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Canonical schema for one dependency fact.
#[derive(Debug)]
pub struct DependencyFactSchema;

impl Schema for DependencyFactSchema {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 0x0030;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Canonical schema for a complete dependency manifest.
#[derive(Debug)]
pub struct DependencyManifestSchema;

impl Schema for DependencyManifestSchema {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 0x0031;
    type Value = Vec<u8>;

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Version of one dependency fact.
pub type DependencyFactVersion = ObjectVersion<DependencyFactSchema>;
/// Version of a complete dependency manifest.
pub type DependencyManifestVersion = ObjectVersion<DependencyManifestSchema>;
fn dependency_read_bytes(read: &ScopedReadObservation) -> Vec<u8> {
    let mut bytes = Vec::new();
    canonical_scoped_observation(&mut bytes, read);
    bytes
}

/// A positive or negative read tied to the recipe that issued it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadDependencyFact {
    recipe: RecipeVersion,
    observation: ScopedReadObservation,
}

impl ReadDependencyFact {
    /// Admits a witnessed read fact.
    ///
    /// Negative reads must carry complete coverage.  Positive reads may be
    /// retained while incomplete, but such a fact cannot be used to produce
    /// a [`crate::ReuseProof`].
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::IncompleteNegativeFact`] when a negative read
    /// lacks a complete coverage witness.
    pub fn witnessed(
        recipe: RecipeVersion,
        observation: ScopedReadObservation,
    ) -> Result<Self, SemanticError> {
        if observation.read().is_negative() && !observation.authoritative_negative() {
            return Err(SemanticError::IncompleteNegativeFact);
        }
        Ok(Self {
            recipe,
            observation,
        })
    }

    /// Admits a complete positive exact, range, or prefix read.
    ///
    /// # Errors
    ///
    /// Returns the read observation admission error or
    /// [`SemanticError::IncompleteNegativeFact`] for an invalid negative
    /// selector.
    pub fn positive(
        recipe: RecipeVersion,
        read: ScopedRead,
        observed: FacetVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::witnessed(
            recipe,
            ScopedReadObservation::positive(read, observed, coverage)?,
        )
    }

    /// Admits a complete negative exact, range, or prefix read.
    ///
    /// # Errors
    ///
    /// Returns the read observation admission error or
    /// [`SemanticError::IncompleteNegativeFact`] when coverage is incomplete.
    pub fn negative(
        recipe: RecipeVersion,
        read: ScopedRead,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::witnessed(recipe, ScopedReadObservation::negative(read, coverage)?)
    }

    /// Admits a complete positive half-open range read.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] for invalid range bounds or
    /// a read observation admission error.
    pub fn range(
        recipe: RecipeVersion,
        facet: FacetKind,
        scope: ScopeRoot,
        start: Vec<u8>,
        end: Vec<u8>,
        observed: FacetVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::positive(
            recipe,
            ScopedRead::range(facet, scope, start, end)?,
            observed,
            coverage,
        )
    }

    /// Admits a complete negative half-open range read.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidSelector`] for invalid range bounds or
    /// a read observation admission error.
    pub fn negative_range(
        recipe: RecipeVersion,
        facet: FacetKind,
        scope: ScopeRoot,
        start: Vec<u8>,
        end: Vec<u8>,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::negative(
            recipe,
            ScopedRead::negative_range(facet, scope, start, end)?,
            coverage,
        )
    }

    /// Admits a complete positive prefix read.
    ///
    /// # Errors
    ///
    /// Returns a read observation admission error when the witness does not
    /// match the selected scope or polarity.
    pub fn prefix(
        recipe: RecipeVersion,
        facet: FacetKind,
        scope: ScopeRoot,
        prefix: Vec<u8>,
        observed: FacetVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::positive(
            recipe,
            ScopedRead::prefix(facet, scope, prefix),
            observed,
            coverage,
        )
    }

    /// Admits a complete negative prefix read.
    ///
    /// # Errors
    ///
    /// Returns a read observation admission error when the witness is not a
    /// complete negative fact.
    pub fn negative_prefix(
        recipe: RecipeVersion,
        facet: FacetKind,
        scope: ScopeRoot,
        prefix: Vec<u8>,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        Self::negative(
            recipe,
            ScopedRead::negative_prefix(facet, scope, prefix),
            coverage,
        )
    }

    /// Returns the recipe that issued this read.
    #[must_use]
    pub const fn recipe(&self) -> RecipeVersion {
        self.recipe
    }

    /// Returns the exact witnessed read.
    #[must_use]
    pub const fn observation(&self) -> &ScopedReadObservation {
        &self.observation
    }

    /// Returns the read selector.
    #[must_use]
    pub const fn read(&self) -> &ScopedRead {
        self.observation.read()
    }

    /// Returns whether the read is a range or prefix read.
    #[must_use]
    pub fn is_range(&self) -> bool {
        matches!(
            self.read().selector(),
            ReadSelector::Range(_) | ReadSelector::Prefix(_)
        )
    }

    /// Returns the canonical fact bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![1];
        version(&mut out, &self.recipe.to_bytes());
        frame(&mut out, &dependency_read_bytes(&self.observation));
        out
    }

    /// Returns the immutable fact version.
    #[must_use]
    pub fn version(&self) -> DependencyFactVersion {
        ObjectVersion::from_value(&self.canonical_bytes())
    }
}

/// A complete authority scope read tied to a recipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityDependencyFact {
    recipe: RecipeVersion,
    authority: AuthorityVersion,
    scope: AuthorityScopeVersion,
    coverage: AuthorizedCompleteCoverage,
}

impl AuthorityDependencyFact {
    /// Admits an authority dependency from a declared scope.
    ///
    /// The coverage witness is checked against the exact scope root.  The
    /// authority revision is taken from the scope itself so two unrelated
    /// authority values cannot be mixed accidentally.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::ScopeMismatch`] when the coverage witness
    /// names another scope.
    pub fn new(
        recipe: RecipeVersion,
        scope: &AuthorityScope,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if coverage.scope_root() != scope.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Ok(Self {
            recipe,
            authority: scope.authority_version(),
            scope: scope.version(),
            coverage,
        })
    }

    /// Returns the recipe that issued this authority read.
    #[must_use]
    pub const fn recipe(&self) -> RecipeVersion {
        self.recipe
    }

    /// Returns the exact authority revision.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.authority
    }

    /// Returns the exact declared authority scope.
    #[must_use]
    pub const fn scope(&self) -> AuthorityScopeVersion {
        self.scope
    }

    /// Returns the complete coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> AuthorizedCompleteCoverage {
        self.coverage
    }

    /// Returns canonical fact bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![2];
        version(&mut out, &self.recipe.to_bytes());
        version(&mut out, &self.authority.to_bytes());
        version(&mut out, &self.scope.to_bytes());
        coverage(&mut out, CoverageWitness::Complete(self.coverage));
        out
    }

    /// Returns the immutable fact version.
    #[must_use]
    pub fn version(&self) -> DependencyFactVersion {
        ObjectVersion::from_value(&self.canonical_bytes())
    }
}

/// A recipe-to-recipe dependency fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecipeDependencyFact {
    recipe: RecipeVersion,
    dependency: RecipeVersion,
}

impl RecipeDependencyFact {
    /// Admits a recipe dependency and rejects self-validation.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DependencyCycle`] when both versions are the
    /// same recipe.
    pub fn new(recipe: RecipeVersion, dependency: RecipeVersion) -> Result<Self, SemanticError> {
        if recipe == dependency {
            return Err(SemanticError::DependencyCycle);
        }
        Ok(Self { recipe, dependency })
    }

    /// Returns the dependent recipe.
    #[must_use]
    pub const fn recipe(&self) -> RecipeVersion {
        self.recipe
    }

    /// Returns the prerequisite recipe.
    #[must_use]
    pub const fn dependency(&self) -> RecipeVersion {
        self.dependency
    }

    /// Returns canonical fact bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = vec![3];
        version(&mut out, &self.recipe.to_bytes());
        version(&mut out, &self.dependency.to_bytes());
        out
    }

    /// Returns the immutable fact version.
    #[must_use]
    pub fn version(&self) -> DependencyFactVersion {
        ObjectVersion::from_value(&self.canonical_bytes())
    }
}

/// One typed dependency fact retained by a recipe shard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyFact {
    /// A positive, negative, exact, range, or prefix read.
    Read(ReadDependencyFact),
    /// An authority scope and revision read.
    Authority(AuthorityDependencyFact),
    /// A recipe prerequisite edge.
    Recipe(RecipeDependencyFact),
}

impl DependencyFact {
    /// Creates a read fact.
    #[must_use]
    pub const fn read(value: ReadDependencyFact) -> Self {
        Self::Read(value)
    }

    /// Creates an authority fact.
    #[must_use]
    pub const fn authority(value: AuthorityDependencyFact) -> Self {
        Self::Authority(value)
    }

    /// Creates a recipe fact.
    #[must_use]
    pub const fn recipe(value: RecipeDependencyFact) -> Self {
        Self::Recipe(value)
    }

    /// Returns the owning/issuing recipe.
    #[must_use]
    pub const fn recipe_version(&self) -> RecipeVersion {
        match self {
            Self::Read(value) => value.recipe(),
            Self::Authority(value) => value.recipe(),
            Self::Recipe(value) => value.recipe(),
        }
    }

    /// Returns the canonical fact bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::Read(value) => value.canonical_bytes(),
            Self::Authority(value) => value.canonical_bytes(),
            Self::Recipe(value) => value.canonical_bytes(),
        }
    }

    /// Returns the immutable fact version.
    #[must_use]
    pub fn version(&self) -> DependencyFactVersion {
        ObjectVersion::from_value(&self.canonical_bytes())
    }
}

/// A sorted, cycle-checked dependency manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyManifest {
    // Admission computes each fact encoding exactly once.  Keeping the
    // canonical bytes beside the sorted facts turns repeated version/key
    // requests into a cheap copy of an already shared immutable value.  This
    // matters because dependency manifests sit on the hot path for both
    // candidate lookup and registration rollback.
    facts: Arc<[DependencyFact]>,
    canonical: Arc<[u8]>,
    version: DependencyManifestVersion,
}

impl DependencyManifest {
    /// Sorts facts by canonical bytes, rejects duplicate facts, and rejects
    /// every recipe dependency cycle before the manifest becomes usable.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateFact`] for duplicate facts or
    /// [`SemanticError::DependencyCycle`] for a recipe cycle.
    pub fn new(facts: Vec<DependencyFact>) -> Result<Self, SemanticError> {
        // Sort an owned `(canonical, fact)` pair so the comparator never
        // allocates and duplicate detection compares the exact bytes that
        // will be committed.  The old `sort_by_key` form re-encoded each
        // candidate repeatedly, which became quadratic in allocation volume
        // for large manifests even though the semantic work was unchanged.
        let mut keyed = facts
            .into_iter()
            .map(|fact| {
                let canonical = fact.canonical_bytes();
                (canonical, fact)
            })
            .collect::<Vec<_>>();
        keyed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        if keyed.windows(2).any(|window| window[0].0 == window[1].0) {
            return Err(SemanticError::DuplicateFact);
        }
        let facts = keyed
            .iter()
            .map(|(_, fact)| fact.clone())
            .collect::<Vec<_>>();
        // A reader can retain one witnessed answer per scoped selector. Two
        // facts that claim the same selector with opposite polarity (or with
        // different recipe provenance) would otherwise pass fact-level
        // hashing and fail much later during index registration. Normalize
        // the read projection at the same boundary so no invalid manifest can
        // become a reusable candidate.
        let reads = facts
            .iter()
            .filter_map(|fact| match fact {
                DependencyFact::Read(value) => Some(value.observation().clone()),
                DependencyFact::Authority(_) | DependencyFact::Recipe(_) => None,
            })
            .collect::<Vec<_>>();
        if !reads.is_empty() {
            let _ = ScopedReadManifest::new(reads)?;
        }
        let mut edges: BTreeMap<RecipeVersion, BTreeSet<RecipeVersion>> = BTreeMap::new();
        for fact in &facts {
            if let DependencyFact::Recipe(edge) = fact {
                edges
                    .entry(edge.recipe())
                    .or_default()
                    .insert(edge.dependency());
            }
        }
        if has_recipe_cycle(&edges) {
            return Err(SemanticError::DependencyCycle);
        }
        let canonical_len = keyed.iter().try_fold(9_usize, |length, (bytes, _)| {
            length.checked_add(8)?.checked_add(bytes.len())
        });
        let canonical_len = canonical_len.ok_or(SemanticError::Overflow)?;
        let mut canonical = Vec::with_capacity(canonical_len);
        canonical.push(1);
        canonical.extend_from_slice(
            &u64::try_from(keyed.len())
                .map_or(u64::MAX, |length| length)
                .to_be_bytes(),
        );
        for (fact, _) in &keyed {
            frame(&mut canonical, fact);
        }
        let canonical: Arc<[u8]> = Arc::from(canonical);
        let version = ObjectVersion::from_value(&canonical.to_vec());
        Ok(Self {
            facts: Arc::from(facts),
            canonical,
            version,
        })
    }

    /// Returns the sorted facts.
    #[must_use]
    pub fn facts(&self) -> &[DependencyFact] {
        &self.facts
    }

    /// Projects the read facts into the exact scoped read manifest they
    /// describe.
    ///
    /// # Errors
    ///
    /// Returns the scoped manifest admission error when projected facts are
    /// duplicated or contain incomplete negative coverage.
    pub fn read_manifest(&self) -> Result<ScopedReadManifest, SemanticError> {
        ScopedReadManifest::new(
            self.facts
                .iter()
                .filter_map(|fact| match fact {
                    DependencyFact::Read(value) => Some(value.observation().clone()),
                    DependencyFact::Authority(_) | DependencyFact::Recipe(_) => None,
                })
                .collect(),
        )
    }

    /// Returns whether every retained read and authority fact can support
    /// exact reuse.
    #[must_use]
    pub fn is_reuse_ready(&self) -> bool {
        self.facts.iter().all(|fact| match fact {
            DependencyFact::Read(value) => {
                matches!(value.observation().coverage(), CoverageWitness::Complete(_))
                    && (value.read().is_negative() || value.observation().version().is_some())
            }
            DependencyFact::Authority(value) => {
                value.coverage().scope_root() == ScopeRoot::from_bytes(value.scope().to_bytes())
            }
            DependencyFact::Recipe(_) => true,
        })
    }

    /// Returns the canonical manifest bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.canonical.to_vec()
    }

    /// Returns the canonical byte length without materializing another copy.
    /// Owners use this for bounded retention accounting while the manifest's
    /// immutable `Arc` remains shared with dependency registration.
    #[must_use]
    pub fn canonical_len(&self) -> usize {
        self.canonical.len()
    }

    /// Returns the immutable manifest version.
    #[must_use]
    pub fn version(&self) -> DependencyManifestVersion {
        self.version
    }
}
