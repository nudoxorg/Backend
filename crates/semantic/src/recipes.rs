//! Typed semantic recipes and propagation suppression.

use crate::canonical::canonical_recipe;
use crate::schema::{
    AuthorityScopeSchema, ExactReadManifestSchema, ReadManifestSchema, RecipeSchema,
    ScopedReadManifestSchema,
};
use crate::{FacetKind, Read, ReadManifest, ScopeIdentity, SemanticError};
use backend_version::ObjectVersion;

/// Typed recipe kind retained for compatibility with existing callers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Recipe {
    /// Exact name projection.
    Names,
    /// Lexical token projection.
    LexicalFacts,
    /// Forward edge projection.
    ReferencesForward,
    /// Reverse edge projection.
    ReferencesReverse,
    /// Embedding input projection.
    EmbeddingInput,
    /// Documentation rendering projection.
    Documents,
    /// Recursive component projection.
    Components,
}

impl Recipe {
    /// Static facet families used by this recipe's default projection.
    #[must_use]
    pub const fn reads(self) -> &'static [FacetKind] {
        match self {
            Self::Names => &[FacetKind::Name],
            Self::LexicalFacts => &[FacetKind::Lexical],
            Self::ReferencesForward | Self::ReferencesReverse => &[FacetKind::Occurrence],
            Self::EmbeddingInput => &[
                FacetKind::Name,
                FacetKind::Documentation,
                FacetKind::Signature,
                FacetKind::Type,
                FacetKind::Visibility,
            ],
            Self::Documents => &[
                FacetKind::Name,
                FacetKind::Documentation,
                FacetKind::Member,
                FacetKind::Visibility,
            ],
            Self::Components => &[FacetKind::Type, FacetKind::Edge, FacetKind::Component],
        }
    }

    /// Creates a typed recipe value with an exact dependency manifest.
    #[must_use]
    pub fn typed(self, version: u16, parameters: Vec<u8>, reads: ReadManifest) -> RecipeSpec {
        RecipeSpec::new(self, version, parameters, reads)
    }

    /// Creates a default positive manifest for one scope.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateRead`] if the registered facets
    /// contain a duplicate selector.
    pub fn manifest<S: ScopeIdentity>(self, scope: S) -> Result<ReadManifest, SemanticError> {
        let scope = scope.scope_root();
        ReadManifest::new(
            self.reads()
                .iter()
                .copied()
                .map(|facet| Read::exact(facet, scope))
                .collect(),
        )
    }
}

/// Versioned recipe data. The manifest is part of recipe identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RecipeSpec {
    /// Recipe kind.
    kind: Recipe,
    /// Recipe schema revision.
    version: u16,
    /// Typed parameters/model settings.
    parameters: Vec<u8>,
    /// Exact positive/negative/range reads.
    reads: ReadManifest,
}

impl RecipeSpec {
    /// Creates immutable typed recipe data.
    #[must_use]
    pub const fn new(kind: Recipe, version: u16, parameters: Vec<u8>, reads: ReadManifest) -> Self {
        Self {
            kind,
            version,
            parameters,
            reads,
        }
    }

    /// Returns the recipe operation kind.
    #[must_use]
    pub const fn kind(&self) -> Recipe {
        self.kind
    }

    /// Returns the recipe schema revision.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Returns typed recipe parameters.
    #[must_use]
    pub fn parameters(&self) -> &[u8] {
        &self.parameters
    }

    /// Returns the exact read manifest bound to this recipe.
    #[must_use]
    pub const fn reads(&self) -> &ReadManifest {
        &self.reads
    }

    /// Returns canonical recipe bytes.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_recipe(self)
    }

    /// Returns the immutable typed recipe value version.
    #[must_use]
    pub fn value_version(&self) -> RecipeVersion {
        ObjectVersion::from_value(self)
    }
}

/// Typed recipe value version.
pub type RecipeVersion = ObjectVersion<RecipeSchema>;
/// Compact read manifest value version.
pub type ReadManifestVersion = ObjectVersion<ReadManifestSchema>;
/// Witnessed read manifest value version.
pub type ExactReadManifestVersion = ObjectVersion<ExactReadManifestSchema>;
/// Full-width scoped read manifest value version.
pub type ScopedReadManifestVersion = ObjectVersion<ScopedReadManifestSchema>;
/// Authority scope value version.
pub type AuthorityScopeVersion = ObjectVersion<AuthorityScopeSchema>;
/// A versioned recipe and its typed value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedRecipe {
    /// Complete recipe data.
    spec: RecipeSpec,
    /// Value version derived from `spec`.
    version: RecipeVersion,
}

impl VersionedRecipe {
    /// Creates a self-consistent versioned recipe.
    #[must_use]
    pub fn new(spec: RecipeSpec) -> Self {
        let version = spec.value_version();
        Self { spec, version }
    }

    /// Returns the immutable typed recipe data.
    #[must_use]
    pub const fn spec(&self) -> &RecipeSpec {
        &self.spec
    }

    /// Returns the version derived from the complete recipe data.
    #[must_use]
    pub const fn version(&self) -> RecipeVersion {
        self.version
    }
}

/// Emission used to stop propagation when a value version stays equal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Emission<T> {
    /// No downstream update is required.
    Unchanged,
    /// A changed value must be emitted.
    Changed(T),
}

/// Suppresses propagation when the complete value is equal.
#[must_use]
pub fn suppress_equal<T: Eq>(old: Option<&T>, new: T) -> Emission<T> {
    if old == Some(&new) {
        Emission::Unchanged
    } else {
        Emission::Changed(new)
    }
}
