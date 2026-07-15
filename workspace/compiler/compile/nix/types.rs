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
//! * [`typed_binding_from_value`] / [`typed_binding_from_shape`] — fill
//!   [`TypedBinding`](ir::kind::TypedBinding) from runtime or static shape.

use ir::generics::ConstExpr;
use ir::kind::TypedBinding;
use ir::primitives::{Primitive, Width};
use ir::ty::{LiteralKind, LiteralValue, Type, TypeReference};
use snix_eval::Value;

use super::syntax::ValueShape;

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
/// | `"attrs"`/`"set"` | `TypeReference("AttrSet")` |
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
		// snix `Value::type_of` returns `"set"`; older docs / our own
		// classifiers also use `"attrs"`.
		"attrs" | "set" => attrset_type(),
		"lambda" => Type::TypeReference(TypeReference {
			identifier:   "Lambda".into(),
			generic_args: None,
		}),
		"derivation" => derivation_type(),
		_ => Type::Any,
	}
}

/// Build a fully-populated [`TypedBinding`] from a forced snix [`Value`].
///
/// Scalars receive a [`Primitive`] (or [`Type::Literal`] for bool/string
/// when the exact spelling is known) plus a [`ConstExpr`] value; attrsets
/// / lists / lambdas get the coarse [`value_kind_type`] only.
pub fn typed_binding_from_value(value: &Value) -> TypedBinding {
	match value {
		Value::Null => TypedBinding {
			ty:      Some(value_kind_type("null")),
			value:   None,
			mutable: Some(false),
		},
		Value::Bool(b) => TypedBinding {
			ty: Some(Type::Literal(LiteralValue {
				kind:  LiteralKind::Boolean,
				value: b.to_string(),
			})),
			value:   Some(ConstExpr::Bool(*b)),
			mutable: Some(false),
		},
		Value::Integer(i) => TypedBinding {
			ty:      Some(value_kind_type("int")),
			value:   Some(ConstExpr::Int(*i)),
			mutable: Some(false),
		},
		Value::Float(f) => TypedBinding {
			ty:      Some(value_kind_type("float")),
			value:   Some(ConstExpr::Float(*f)),
			mutable: Some(false),
		},
		Value::String(s) => {
			let text = s.to_string();
			TypedBinding {
				ty: Some(Type::Literal(LiteralValue {
					kind:  LiteralKind::String,
					value: text.clone(),
				})),
				value:   Some(ConstExpr::Str(text)),
				mutable: Some(false),
			}
		}
		Value::Path(p) => TypedBinding {
			ty:      Some(value_kind_type("path")),
			value:   Some(ConstExpr::Str(p.display().to_string())),
			mutable: Some(false),
		},
		Value::List(_) => TypedBinding {
			ty:      Some(value_kind_type("list")),
			value:   None,
			mutable: Some(false),
		},
		Value::Closure(_) | Value::Builtin(_) => TypedBinding {
			ty:      Some(value_kind_type("lambda")),
			value:   None,
			mutable: Some(false),
		},
		Value::Attrs(_) => {
			// Caller is expected to special-case derivations before invoking
			// this; still detect here so a missed branch stays typed.
			let kind = if is_derivation_attrs(value) {
				"derivation"
			} else {
				"set"
			};
			TypedBinding {
				ty:      Some(value_kind_type(kind)),
				value:   None,
				mutable: Some(false),
			}
		}
		_ => TypedBinding {
			ty:      Some(Type::Any),
			value:   None,
			mutable: Some(false),
		},
	}
}

/// A derivation-typed binding (used by `mint_derivation`).
pub fn derivation_binding() -> TypedBinding {
	TypedBinding {
		ty:      Some(derivation_type()),
		value:   None,
		mutable: Some(false),
	}
}

/// Build a [`TypedBinding`] from a static-layer [`ValueShape`].
///
/// When `source_slice` is the raw binding RHS text, scalar literals get a
/// recovered [`ConstExpr`]; otherwise only the type is filled.
pub fn typed_binding_from_shape(shape: &ValueShape, source_slice: &str) -> TypedBinding {
	match shape {
		ValueShape::Integer => {
			let v = source_slice.trim().parse::<i64>().ok();
			TypedBinding {
				ty:      Some(value_kind_type("int")),
				value:   v.map(ConstExpr::Int),
				mutable: Some(false),
			}
		}
		ValueShape::Float => {
			let v = source_slice.trim().parse::<f64>().ok();
			TypedBinding {
				ty:      Some(value_kind_type("float")),
				value:   v.map(ConstExpr::Float),
				mutable: Some(false),
			}
		}
		ValueShape::Bool(b) => TypedBinding {
			ty: Some(Type::Literal(LiteralValue {
				kind:  LiteralKind::Boolean,
				value: b.to_string(),
			})),
			value:   Some(ConstExpr::Bool(*b)),
			mutable: Some(false),
		},
		ValueShape::Null => TypedBinding {
			ty:      Some(value_kind_type("null")),
			value:   None,
			mutable: Some(false),
		},
		ValueShape::String => {
			let unquoted = unquote_simple(source_slice);
			TypedBinding {
				ty: Some(Type::Literal(LiteralValue {
					kind:  LiteralKind::String,
					value: unquoted.clone(),
				})),
				value:   Some(ConstExpr::Str(unquoted)),
				mutable: Some(false),
			}
		}
		ValueShape::Path => TypedBinding {
			ty:      Some(value_kind_type("path")),
			value:   Some(ConstExpr::Str(source_slice.trim().to_string())),
			mutable: Some(false),
		},
		ValueShape::List => TypedBinding {
			ty:      Some(value_kind_type("list")),
			value:   None,
			mutable: Some(false),
		},
		ValueShape::AttrSet { .. } => TypedBinding {
			ty:      Some(value_kind_type("set")),
			value:   None,
			mutable: Some(false),
		},
		ValueShape::Lambda => TypedBinding {
			ty:      Some(value_kind_type("lambda")),
			value:   None,
			mutable: Some(false),
		},
		ValueShape::Other => TypedBinding::default(),
	}
}

// ──────────────────────────────────────────────────────────────────────────
// Internals
// ──────────────────────────────────────────────────────────────────────────

fn is_derivation_attrs(value: &Value) -> bool {
	let Value::Attrs(attrs) = value else {
		return false;
	};
	attrs.iter().any(|(k, v)| {
		k.to_string() == "type"
			&& matches!(v, Value::String(s) if s.to_string() == "derivation")
	})
}

/// Best-effort unquote of a simple `"…"` or `''…''` Nix string literal.
fn unquote_simple(raw: &str) -> String {
	let trimmed = raw.trim();
	if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
		return trimmed[1..trimmed.len() - 1]
			.replace("\\\"", "\"")
			.replace("\\n", "\n")
			.replace("\\t", "\t")
			.replace("\\\\", "\\");
	}
	if trimmed.starts_with("''") && trimmed.ends_with("''") && trimmed.len() >= 4 {
		return trimmed[2..trimmed.len() - 2].to_string();
	}
	trimmed.to_string()
}
