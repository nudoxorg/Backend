use super::*;

/// Rich recursive-free expression used by compatibility and signature facts.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypeExpr {
    /// A named type.
    Named(String),
    /// An ordered tuple.
    Tuple(Vec<TypeExpr>),
    /// A function type.
    Function {
        /// Ordered arguments.
        args: Vec<TypeExpr>,
        /// Result type.
        result: Box<TypeExpr>,
    },
    /// A type constructor with ordered arguments.
    Applied {
        /// Constructor/base type.
        base: Box<TypeExpr>,
        /// Ordered type arguments.
        args: Vec<TypeExpr>,
    },
    /// An inferred or unavailable type.
    Infer,
}

/// Rich type state from a native authority.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeState {
    /// Explicit concrete type.
    Concrete,
    /// Checker-computed type.
    Computed,
    /// Authority knows a type is dynamic/unknown.
    Unknown,
    /// Complete observation that no type applies.
    Absent,
}

pub(crate) fn type_state_tag(value: TypeState) -> u8 {
    match value {
        TypeState::Concrete => 1,
        TypeState::Computed => 2,
        TypeState::Unknown => 3,
        TypeState::Absent => 4,
    }
}

/// Type grammar tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeTag {
    /// Named declaration type.
    Named,
    /// Tuple/product type.
    Tuple,
    /// Function type.
    Function,
    /// Application type.
    Applied,
    /// Type parameter.
    Parameter,
    /// Union or sum type.
    Union,
    /// Unknown type constructor.
    Other,
}

pub(crate) fn type_tag(value: TypeTag) -> u8 {
    match value {
        TypeTag::Named => 1,
        TypeTag::Tuple => 2,
        TypeTag::Function => 3,
        TypeTag::Applied => 4,
        TypeTag::Parameter => 5,
        TypeTag::Union => 6,
        TypeTag::Other => 7,
    }
}

/// Symbolic operand in the rich type relation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TypeOperand {
    /// A local logical type node.
    Local(TypeId),
    /// A foreign logical type identity and optional admitted value version.
    External {
        /// Foreign namespace.
        namespace: String,
        /// Foreign stable identity bytes.
        identity: String,
        /// Known external variant/version bytes.
        version: Option<Vec<u8>>,
    },
    /// A literal operand.
    Literal(Vec<u8>),
}

pub(crate) fn encode_type_operand(out: &mut Vec<u8>, value: &TypeOperand) {
    match value {
        TypeOperand::Local(key) => {
            out.push(1);
            object_key(out, key);
        }
        TypeOperand::External {
            namespace,
            identity,
            version,
        } => {
            out.push(2);
            text(out, namespace);
            text(out, identity);
            match version {
                Some(value) => {
                    out.push(1);
                    bytes(out, value);
                }
                None => out.push(0),
            }
        }
        TypeOperand::Literal(value) => {
            out.push(3);
            bytes(out, value);
        }
    }
}

/// Canonical rich type node. Cycles are represented by symbolic [`TypeId`]
/// operands and are therefore finite at each row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TypeNode {
    /// Type constructor tag.
    pub tag: TypeTag,
    /// Ordered operand role names.
    pub roles: Vec<String>,
    /// Ordered symbolic/literal operands.
    pub operands: Vec<TypeOperand>,
    /// Ordered qualifiers.
    pub qualifiers: Vec<String>,
    /// Optional literal payload.
    pub literal: Option<Vec<u8>>,
    /// Ordered bound type identities.
    pub bounds: Vec<TypeId>,
    /// Concrete/computed/unknown state.
    pub state: TypeState,
}

/// Stable logical key for one type node.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TypeIdentity {
    /// Owning declaration.
    owner: EntityId,
    /// Authority-scoped type role.
    local_key: String,
}

impl TypeIdentity {
    /// Admits a stable type role under an owning declaration.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidIdentity`] for an empty role.
    pub fn new(owner: EntityId, local_key: impl Into<String>) -> Result<Self, SemanticError> {
        let local_key = local_key.into();
        if local_key.is_empty() {
            return Err(SemanticError::InvalidIdentity);
        }
        Ok(Self { owner, local_key })
    }

    /// Returns the owning declaration.
    #[must_use]
    pub const fn owner(&self) -> EntityId {
        self.owner
    }

    /// Returns the authority-scoped type role.
    #[must_use]
    pub fn local_key(&self) -> &str {
        &self.local_key
    }
}

/// Type relation value with coverage and provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeRecord {
    /// Stable type identity repeated in the value.
    identity: TypeIdentity,
    /// Type node value when present.
    value: Option<Arc<TypeNode>>,
    /// Coverage and explicit absence.
    coverage: FacetCoverage,
    /// Authority/source/version basis.
    provenance: Provenance,
}

impl TypeRecord {
    /// Admits a type row whose value/coverage state is self-consistent.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] when the value and
    /// coverage disagree.
    pub fn new(
        identity: TypeIdentity,
        value: Option<TypeNode>,
        coverage: FacetCoverage,
        provenance: Provenance,
    ) -> Result<Self, SemanticError> {
        validate_value_state(value.is_some(), coverage)?;
        Ok(Self {
            identity,
            value: value.map(Arc::new),
            coverage,
            provenance,
        })
    }

    /// Returns the stable type identity.
    #[must_use]
    pub const fn identity(&self) -> &TypeIdentity {
        &self.identity
    }

    /// Returns the type node, when live.
    #[must_use]
    pub fn value(&self) -> Option<&TypeNode> {
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

/// Stable type node logical key.
pub type TypeId = ObjectKey<TypeSchema>;
/// Complete type node value version.
pub type TypeVersion = ObjectVersion<TypeValueSchema>;
