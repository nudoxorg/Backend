//! IR type helpers for the Nix producer.
//!
//! Provides canonical [`ir::ty::Type`] constructors for the Nix-specific
//! "kinds" that recur across the static and dynamic layers:
//!
//! * [`derivation_type`] — the Nix `Derivation` build product.
//! * [`attrset_type`] — an unparameterised attribute set.
//! * [`value_kind_type`] — maps snix value-kind strings to IR types; used
//!   by the dynamic layer when a value's runtime kind is known but its
//!   precise type cannot be inferred.

use ir::primitives::{Primitive, Width};
use ir::ty::{Type, TypeReference};

// ──────────────────────────────────────────────────────────────────────────
// Public helpers
// ──────────────────────────────────────────────────────────────────────────

/// The `Derivation` type — a Nix build product (a derivation is a
/// `StorePath` + `outPath` + build metadata).
///
/// Lowered as a nominal [`TypeReference`] so the renderer and graph layer
/// can attach documentation and link to the builtins `derivation` entry
/// without tying us to a particular structural encoding.
pub fn derivation_type() -> Type {
	Type::TypeReference(TypeReference {
		identifier:   "Derivation".into(),
		generic_args: None,
	})
}

/// An unparameterised Nix attribute set (`{ ... }`).
///
/// Where the dynamic layer knows a more precise record shape (e.g.
/// `{ name :: String; version :: String; ... }`) it builds a
/// [`Type::RecordLiteral`] directly; this function is the fallback for
/// "it's an attrset but we don't know what's in it".
pub fn attrset_type() -> Type {
	Type::TypeReference(TypeReference {
		identifier:   "AttrSet".into(),
		generic_args: None,
	})
}

/// Map a snix value-kind string to the closest IR [`Type`].
///
/// snix (and the Nix evaluator in general) reports runtime value kinds as
/// strings through its inspection API.  This function converts those
/// strings into structured IR types so the dynamic layer can annotate
/// bindings with at least a coarse type even when the signature parser
/// (`sig::parse`) can't find a `::` doc-comment type.
///
/// | snix kind string | IR type |
/// |-----------------|---------|
/// | `"string"`      | `Primitive(String)` |
/// | `"int"`         | `Primitive(Int(W64))` |
/// | `"float"`       | `Primitive(Float(W64))` |
/// | `"bool"`        | `Primitive(Bool)` |
/// | `"path"`        | `TypeReference("Path")` |
/// | `"null"`        | `TypeReference("Null")` |
/// | `"list"`        | `Slice(Any)` — a list of unknown element type |
/// | `"attrs"`       | `TypeReference("AttrSet")` |
/// | `"lambda"`      | `TypeReference("Lambda")` |
/// | `"derivation"`  | `TypeReference("Derivation")` |
/// | anything else   | `Any` |
pub fn value_kind_type(kind: &str) -> Type {
	match kind {
		"string" => Type::Primitive(Primitive::String),
		"int" => Type::Primitive(Primitive::Int(Width::W64)),
		"float" => Type::Primitive(Primitive::Float(Width::W64)),
		"bool" => Type::Primitive(Primitive::Bool),
		"path" => Type::TypeReference(TypeReference {
			identifier:   "Path".into(),
			generic_args: None,
		}),
		"null" => Type::TypeReference(TypeReference {
			identifier:   "Null".into(),
			generic_args: None,
		}),
		"list" => Type::Slice(Box::new(Type::Any)),
		"attrs" => attrset_type(),
		"lambda" => Type::TypeReference(TypeReference {
			identifier:   "Lambda".into(),
			generic_args: None,
		}),
		"derivation" => derivation_type(),
		_ => Type::Any,
	}
}
