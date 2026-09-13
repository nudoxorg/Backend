use super::*;

/// Stable ordered membership key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MemberKey {
    /// Owning declaration.
    owner: EntityId,
    /// Member declaration.
    member: EntityId,
    /// Authority order ordinal.
    ordinal: u32,
}

impl MemberKey {
    /// Creates one stable ordered membership key.
    #[must_use]
    pub const fn new(owner: EntityId, member: EntityId, ordinal: u32) -> Self {
        Self {
            owner,
            member,
            ordinal,
        }
    }

    /// Returns the owning declaration.
    #[must_use]
    pub const fn owner(self) -> EntityId {
        self.owner
    }

    /// Returns the member declaration.
    #[must_use]
    pub const fn member(self) -> EntityId {
        self.member
    }

    /// Returns the source order ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

/// Ordered member relation value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemberRecord {
    /// Ordered member key.
    key: MemberKey,
    /// Explicit complete membership state.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl MemberRecord {
    /// Admits an observed membership key. Membership has no separate value;
    /// therefore it must remain a live state even when coverage is partial.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] for an explicit
    /// absence/deletion state.
    pub fn new(
        key: MemberKey,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        if coverage.deletion() != Deletion::Live {
            return Err(SemanticError::InvalidCoverageState);
        }
        Ok(Self {
            key,
            coverage,
            provenance,
        })
    }

    /// Returns the ordered membership key.
    #[must_use]
    pub const fn key(&self) -> MemberKey {
        self.key
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

/// Stable ordered member key.
pub type MemberId = ObjectKey<MemberSchema>;
/// Ordered member value version.
pub type MemberVersion = ObjectVersion<MemberValueSchema>;

/// Stable attribute key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributeKey {
    /// Entity owning the attribute.
    entity: EntityId,
    /// Source order ordinal.
    ordinal: u32,
}

impl AttributeKey {
    /// Creates one stable ordered attribute key.
    #[must_use]
    pub const fn new(entity: EntityId, ordinal: u32) -> Self {
        Self { entity, ordinal }
    }

    /// Returns the owning declaration.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns the source order ordinal.
    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

/// Attribute atom.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Attribute {
    /// Attribute name/kind.
    pub name: AttributeKind,
    /// Canonical attribute payload.
    pub value: Vec<u8>,
}

/// Attribute name kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AttributeKind {
    /// Documentation marker.
    Doc,
    /// Derive/generated marker.
    Derive,
    /// Representation marker.
    Repr,
    /// Authority-specific attribute.
    Custom,
}

pub(crate) fn attribute_kind_tag(value: AttributeKind) -> u8 {
    match value {
        AttributeKind::Doc => 1,
        AttributeKind::Derive => 2,
        AttributeKind::Repr => 3,
        AttributeKind::Custom => 4,
    }
}

/// Attribute relation value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeRecord {
    /// Attribute key.
    key: AttributeKey,
    /// Ordered attribute atom when present.
    value: Option<Arc<Attribute>>,
    /// Coverage and explicit absence.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl AttributeRecord {
    /// Admits an attribute row with a valid value/coverage state.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when value presence
    /// disagrees with coverage.
    pub fn new(
        key: AttributeKey,
        value: Option<Attribute>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        Ok(Self {
            key,
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the stable attribute key.
    #[must_use]
    pub const fn key(&self) -> AttributeKey {
        self.key
    }

    /// Returns the attribute atom, when live.
    #[must_use]
    pub fn value(&self) -> Option<&Attribute> {
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

/// Stable attribute key.
pub type AttributeId = ObjectKey<AttributeSchema>;
/// Attribute atom version.
pub type AttributeVersion = ObjectVersion<AttributeValueSchema>;
