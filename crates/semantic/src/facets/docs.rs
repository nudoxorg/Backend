use super::*;

/// Documentation fragment preserving source order and fragment kind.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DocFragment {
    /// Plain text bytes.
    Text(Vec<u8>),
    /// Code/preformatted bytes.
    Code(Vec<u8>),
    /// A link fragment.
    Link(DocLink),
    /// An explicit line break.
    Break,
}

/// A documentation target with local, stable foreign, and unknown states.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DocTarget {
    /// A local logical entity target.
    Local(EntityId),
    /// A stable foreign target with no local key convention.
    Foreign {
        /// Foreign authority namespace.
        namespace: String,
        /// Exact foreign identity.
        identity: String,
        /// Known foreign variant, if any.
        variant: Option<Vec<u8>>,
    },
    /// Target was not resolved by the docs authority.
    Unknown,
}

/// Documentation link fragment.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocLink {
    /// Display label bytes.
    pub label: Vec<u8>,
    /// Exact target semantics.
    pub target: DocTarget,
}

/// Complete documentation value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocumentationValue {
    /// Ordered text/code/link/break fragments.
    pub fragments: Vec<DocFragment>,
}

/// Documentation relation value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentationRecord {
    /// Entity owning the document.
    entity: EntityId,
    /// Ordered document when present.
    value: Option<Arc<DocumentationValue>>,
    /// Coverage and explicit empty/unavailable state.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl DocumentationRecord {
    /// Admits a documentation row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when value presence
    /// disagrees with coverage.
    pub fn new(
        entity: EntityId,
        value: Option<DocumentationValue>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        Ok(Self {
            entity,
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the owning declaration.
    #[must_use]
    pub const fn entity(&self) -> EntityId {
        self.entity
    }

    /// Returns the document, when live.
    #[must_use]
    pub fn value(&self) -> Option<&DocumentationValue> {
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

/// Complete documentation value version.
pub type DocumentationVersion = ObjectVersion<DocumentationValueSchema>;
