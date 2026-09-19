use super::*;

/// Name fact kept as a compact compatibility value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NameFact {
    /// Entity owning the name.
    pub entity: EntityId,
    /// Authority namespace for name resolution.
    pub namespace: String,
    /// Canonical name bytes represented as UTF-8.
    pub name: String,
}

/// Documentation compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DocumentationFact {
    /// Entity owning the document.
    pub entity: EntityId,
    /// Plain text compatibility view.
    pub text: String,
}

/// Signature compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SignatureFact {
    /// Entity owning the signature.
    pub entity: EntityId,
    /// Structured signature expression.
    pub signature: TypeExpr,
}

/// Attribute compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AttributesFact {
    /// Entity owning the attributes.
    pub entity: EntityId,
    /// Ordered source attributes.
    pub attributes: Vec<Attribute>,
}

/// Generic parameter compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GenericsFact {
    /// Entity owning the parameters.
    pub entity: EntityId,
    /// Ordered parameter names.
    pub parameters: Vec<String>,
}

/// Constraint compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConstraintsFact {
    /// Entity owning the constraints.
    pub entity: EntityId,
    /// Ordered constraint expressions.
    pub constraints: Vec<TypeExpr>,
}

/// Type compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TypeFact {
    /// Entity owning the type.
    pub entity: EntityId,
    /// Structured type expression.
    pub ty: TypeExpr,
}
