use super::*;

/// Language extension family.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExtensionKind {
    /// Rust extension fields.
    Rust,
    /// TypeScript extension fields.
    TypeScript,
    /// C# extension fields.
    CSharp,
    /// Go extension fields.
    Go,
    /// Python extension fields.
    Python,
    /// Java extension fields.
    Java,
    /// Clang extension fields.
    Clang,
    /// Authority-specific extension fields.
    Custom,
}

pub(crate) fn extension_kind_tag(value: ExtensionKind) -> u8 {
    match value {
        ExtensionKind::Rust => 1,
        ExtensionKind::TypeScript => 2,
        ExtensionKind::CSharp => 3,
        ExtensionKind::Go => 4,
        ExtensionKind::Python => 5,
        ExtensionKind::Java => 6,
        ExtensionKind::Clang => 7,
        ExtensionKind::Custom => 8,
    }
}

/// Language extension compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExtensionFact {
    /// Entity owning the extension.
    pub entity: EntityId,
    /// Language family.
    pub language: ExtensionKind,
    /// Authority-specific extension tag.
    pub tag: u16,
    /// Canonical extension payload.
    pub payload: Vec<u8>,
}

/// Extension relation value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionRecord {
    /// Extension key.
    key: ExtensionKey,
    /// Extension value when present.
    value: Option<Arc<ExtensionFact>>,
    /// Coverage and explicit absence.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

/// Stable extension logical key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExtensionKey {
    /// Owning entity.
    entity: EntityId,
    /// Language family.
    language: ExtensionKind,
    /// Extension field tag.
    tag: u16,
}

impl ExtensionKey {
    /// Creates one stable extension identity.
    #[must_use]
    pub const fn new(entity: EntityId, language: ExtensionKind, tag: u16) -> Self {
        Self {
            entity,
            language,
            tag,
        }
    }

    /// Returns the owning declaration.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns the language family.
    #[must_use]
    pub const fn language(self) -> ExtensionKind {
        self.language
    }

    /// Returns the authority-specific field tag.
    #[must_use]
    pub const fn tag(self) -> u16 {
        self.tag
    }
}

impl ExtensionRecord {
    /// Admits an extension row whose value repeats its logical key.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] or
    /// [`SemanticError::InvalidFacetBinding`] for an inconsistent row.
    pub fn new(
        key: ExtensionKey,
        value: Option<ExtensionFact>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        if value.as_ref().is_some_and(|extension| {
            extension.entity != key.entity()
                || extension.language != key.language()
                || extension.tag != key.tag()
        }) {
            return Err(SemanticError::InvalidFacetBinding);
        }
        Ok(Self {
            key,
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the stable extension key.
    #[must_use]
    pub const fn key(&self) -> ExtensionKey {
        self.key
    }

    /// Returns the extension value, when live.
    #[must_use]
    pub fn value(&self) -> Option<&ExtensionFact> {
        self.value.as_deref()
    }

    /// Returns the checked coverage/state.
    #[must_use]
    pub const fn coverage(&self) -> FacetCoverage {
        self.coverage
    }

    /// Returns the bound provenance.
    #[must_use]
    pub const fn provenance(&self) -> Provenance {
        self.provenance
    }
}

/// Stable extension logical key.
pub type ExtensionId = ObjectKey<ExtensionSchema>;
/// Extension value version.
pub type ExtensionVersion = ObjectVersion<ExtensionValueSchema>;
