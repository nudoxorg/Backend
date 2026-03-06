#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ty::Type;

/// A universal representation of generics across languages.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Generics {
	pub type_params:     Vec<TypeParam>,
	pub const_params:    Vec<ConstParam>,
	pub lifetime_params: Vec<LifetimeParam>,
	pub constraints:     Vec<Constraint>,
}

// MARK: - Type Parameters

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeParam {
	pub name:         String,
	pub kind:         TypeKind,
	pub variance:     Variance,
	pub default_type: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TypeKind {
	Type,
	HigherKinded,
	Associated,
}

// MARK: - Const Parameters

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstParam {
	pub name:          String,
	pub ty:            TypeExpr,
	pub default_value: Option<ConstExpr>,
}

// MARK: - Lifetime Parameters (Rust-style)

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct LifetimeParam {
	pub name:     String,
	pub variance: Variance,
}

// MARK: - Constraints

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Constraint {
	TraitBound { param: String, trait_ref: TraitRef },
	AssociatedTypeBound { param: String, assoc_name: String, bound: TypeExpr },
	HigherKindedBound { param: String, kind: KindExpr },
	LifetimeBound { shorter: String, longer: String },
	ConstExprBound { param: String, expr: ConstExpr },
	LogicalPredicate { expr: PredicateExpr },
}

// MARK: - Supporting Types

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TraitRef {
	pub name: String,
	pub args: Vec<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Variance {
	Covariant,
	Contravariant,
	Invariant,
	Bivariant,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct TypeExpr {
	pub name: String,
	pub args: Vec<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ConstExpr {
	pub expr: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KindExpr {
	pub signature: String, // e.g. "* -> *"
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PredicateExpr {
	pub expr: String, // logical expression, e.g. "T: Clone && U: Copy"
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))] // Example: Tagged enum for Serde
pub enum GenericArg {
	#[cfg_attr(feature = "serde", serde(rename = "type"))]
	Type(Type),
	#[cfg_attr(feature = "serde", serde(rename = "constExpr"))]
	ConstExpr(ConstExpr),
	Lifetime(String),
}
