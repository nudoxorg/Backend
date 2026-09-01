//! Frozen declaration-kind discriminants shared by every producer, fragment,
//! and identity preimage. The codes are part of the canonical wire format and
//! never move; `codes 0..=2 predate the full parity set and never move`.

/// Closed declaration shape retained in the semantic entity lane.
///
/// The discriminant set is exactly the declaration-kind rows named by the
/// compiler parity matrix (Module..Param); codes 0..=2 predate the full set
/// and never move.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityKind {
    /// A callable declaration with parameters and an optional result.
    Function = 0,
    /// An immutable named value with an optional exact value.
    Constant = 1,
    /// A field-bearing nominal record (struct, class, union form).
    Record = 2,
    /// A namespace or module boundary.
    Module = 3,
    /// A named member of a record.
    Field = 4,
    /// A type alias or typedef target.
    Alias = 5,
    /// A interface/trait/protocol declaration.
    Trait = 6,
    /// A trait/interface implementation binding.
    Implementation = 7,
    /// An enumerated type declaration.
    Enum = 8,
    /// One variant of an enum declaration.
    Variant = 9,
    /// A named mutable or immutable storage binding.
    Static = 10,
    /// A re-exported alternate name for an existing declaration.
    Reexport = 11,
    /// One declared parameter of a callable or generic declaration.
    Parameter = 12,
}

impl EntityKind {
    /// Canonical declaration-kind order used by registry reports and tests.
    pub const ALL: [Self; 13] = [
        Self::Function,
        Self::Constant,
        Self::Record,
        Self::Module,
        Self::Field,
        Self::Alias,
        Self::Trait,
        Self::Implementation,
        Self::Enum,
        Self::Variant,
        Self::Static,
        Self::Reexport,
        Self::Parameter,
    ];
}

impl From<EntityKind> for u16 {
    /// Encodes the stable wire discriminant.
    fn from(value: EntityKind) -> Self {
        match value {
            EntityKind::Function => 0,
            EntityKind::Constant => 1,
            EntityKind::Record => 2,
            EntityKind::Module => 3,
            EntityKind::Field => 4,
            EntityKind::Alias => 5,
            EntityKind::Trait => 6,
            EntityKind::Implementation => 7,
            EntityKind::Enum => 8,
            EntityKind::Variant => 9,
            EntityKind::Static => 10,
            EntityKind::Reexport => 11,
            EntityKind::Parameter => 12,
        }
    }
}

impl TryFrom<u16> for EntityKind {
    type Error = EntityKindCodeError;

    /// Decodes a stable wire discriminant, rejecting unknown values with the
    /// exact observed operand.
    fn try_from(actual: u16) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Function),
            1 => Ok(Self::Constant),
            2 => Ok(Self::Record),
            3 => Ok(Self::Module),
            4 => Ok(Self::Field),
            5 => Ok(Self::Alias),
            6 => Ok(Self::Trait),
            7 => Ok(Self::Implementation),
            8 => Ok(Self::Enum),
            9 => Ok(Self::Variant),
            10 => Ok(Self::Static),
            11 => Ok(Self::Reexport),
            12 => Ok(Self::Parameter),
            actual => Err(EntityKindCodeError { actual }),
        }
    }
}

/// Exact kind-code rejection retaining the observed discriminant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityKindCodeError {
    /// Rejected kind discriminant.
    pub actual: u16,
}
