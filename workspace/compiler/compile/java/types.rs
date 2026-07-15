//! Lowering the oracle's structural [`schema::TypeMirror`] tree into
//! `ir::ty::Type` at full fidelity — no stringified types.
//!
//! The mapping decisions (each also documented at its match arm):
//!
//! * **primitives** → `Primitive` × `Width`: `byte`/`short`/`int`/`long` →
//!   `Int(W8/W16/W32/W64)` (Java integrals are signed), `float`/`double` →
//!   `Float(W32/W64)`, `boolean` → `Bool` (the JVM's storage width is an
//!   implementation detail the language never exposes), and `char` → `Char`.
//!   A Java `char` is strictly a UTF-16 code *unit* — `UInt(W16)` would be
//!   the bit-accurate reading — but its language role is "character", so the
//!   IR's `Char` (single code point) is the closest semantic primitive; the
//!   surrogate-pair caveat is accepted and documented here.
//! * **`void`** → the unit type, `Type::Tuple(vec![])` (per `ir::ty`'s
//!   "empty vec represents the Unit type"). Method lowering elides the
//!   output slot entirely for `void` returns; this arm covers `void` in
//!   other positions.
//! * **declared types** → [`TypeReference`] with the fully qualified element
//!   name (`java.util.List`) and structural generic args. Two special cases:
//!   `java.lang.Object` → `Type::Any` (the IR's top type explicitly names
//!   Java's `Object`) and `java.lang.String` → `Primitive::String`. Boxed
//!   primitives (`java.lang.Integer`, …) intentionally stay references —
//!   boxing changes identity and nullability. A *generic-owner* qualified
//!   use (`Outer<String>.Inner`) becomes a [`QualifiedPath`] projecting
//!   `Inner` out of the fully lowered owner type, so the owner's type
//!   arguments are never dropped.
//! * **arrays** → `Type::Slice` — a Java array's length is a runtime value,
//!   never part of its type, so the dynamically-sized sequence form is the
//!   faithful shape (`Type::Array` would fabricate a length).
//! * **type variables** → `Type::GenericParam` by name; their bounds live on
//!   the declaring parameter list (see [`lower_type_params`]).
//! * **wildcards** → variance-carrying [`TypeOperator`]s, one operator per
//!   `ir::generics::Variance` reading of Java's use-site variance:
//!   `? extends X` → operator `"? extends"` (covariant), `? super X` →
//!   operator `"? super"` (contravariant), and the unbounded `?` → operator
//!   `"?"` over `Type::Any` (bivariant; semantically `? extends Object`).
//!   The bound stays fully structural in the operator's type slot.
//! * **intersections** (`T extends A & B`) → `Type::Intersection`;
//!   **unions** (multi-catch) → `Type::Union`.
//! * **error types** (unresolvable in the oracle's environment) →
//!   `TypeReference` over the source text — the one documented best-effort
//!   spot, kept referable rather than dropped.
//! * **the null type** → `TypeReference("null")` (JLS §4.1's null type has
//!   no IR primitive; `Never` would misstate its single inhabitant).
//! * **type-use annotations** → nested [`TypeOperator`] wrappers whose
//!   `operator` is the rendered annotation (`@pkg.NonNull`), outermost
//!   first. Parameter declaration annotations are applied the same way by
//!   callers via [`apply_annotations`].
//!
//! Also here: modifier→`Visibility` mapping, `Value`→`ConstExpr` constants,
//! declaration-site type-parameter lowering into `Generics`, and the
//! name-based [`TypeExpr`]/[`TraitRef`] projections used inside constraints.

use ir::generics::{Constraint, GenericArg, Generics, Kind, TraitRef, TypeExpr, Variance};
use ir::generics::ConstExpr;
use ir::kind::Visibility;
use ir::parameter::{Parameter as IrParameter, TypeParam, TypeParamOrigin};
use ir::primitives::{Primitive, Width};
use ir::ty::{GenericParam, QualifiedPath, Type as IrType, TypeOperator, TypeReference};

use super::schema;

/// Defensive recursion bound, mirroring the oracle's own depth guard. Type
/// *uses* are finite trees (Java cannot express an infinite type without a
/// named intermediary), so this only fails soft on malformed input.
const MAX_DEPTH: usize = 64;

/// Map a modifier set onto the IR visibility vocabulary. Java's default
/// (no access modifier) is package-private → `Visibility::Package`. The
/// oracle reports *implicit* modifiers too (interface members arrive
/// explicitly `public`), so no per-container special-casing is needed.
pub fn visibility(modifiers: &[String]) -> Visibility {
	for m in modifiers {
		match m.as_str() {
			"public" => return Visibility::Public,
			"protected" => return Visibility::Protected,
			"private" => return Visibility::Private,
			_ => {}
		}
	}
	Visibility::Package
}

/// Whether a modifier list contains `name`.
pub fn has_modifier(modifiers: &[String], name: &str) -> bool {
	modifiers.iter().any(|m| m == name)
}

/// Lower an oracle type mirror into the IR type algebra.
///
/// Type-use annotations (`@NonNull String`, `String @Interned []`, …) are
/// preserved as nested [`TypeOperator`] wrappers (`operator = "@…"`) around
/// the underlying type so they are not dropped.
pub fn lower_type(t: &schema::TypeMirror) -> IrType {
	lower_type_depth(t, 0)
}

/// Wrap `ty` with one [`TypeOperator`] per declaration/use annotation, outer-
/// most first. Used for both type-use annotations on a mirror and for formal
/// parameter declaration annotations (which have no parameter-attribute slot).
pub fn apply_annotations(ty: IrType, annotations: &[schema::Annotation]) -> IrType {
	if annotations.is_empty() {
		return ty;
	}
	// Outer-first: first annotation is the outermost operator.
	annotations.iter().rev().fold(ty, |inner, a| {
		IrType::TypeOperator(TypeOperator {
			operator: render_annotation(a),
			r#type:   Box::new(inner),
		})
	})
}

/// Type-use annotations carried on a mirror, when the variant has them.
fn type_use_annotations(t: &schema::TypeMirror) -> &[schema::Annotation] {
	match t {
		schema::TypeMirror::Primitive { annotations, .. }
		| schema::TypeMirror::Declared { annotations, .. }
		| schema::TypeMirror::Array { annotations, .. }
		| schema::TypeMirror::Typevar { annotations, .. } => annotations,
		_ => &[],
	}
}

fn lower_type_depth(t: &schema::TypeMirror, depth: usize) -> IrType {
	if depth > MAX_DEPTH {
		return IrType::Infer;
	}

	let base = match t {
		schema::TypeMirror::Primitive { name, .. } => lower_primitive(name),

		// Unit in the IR is the empty tuple.
		schema::TypeMirror::Void => IrType::Tuple(Vec::new()),

		schema::TypeMirror::Declared { name, args, owner, .. } => {
			// The IR's top type documents itself as Java's `Object`.
			if name == "java.lang.Object" {
				IrType::Any
			} else if name == "java.lang.String" {
				IrType::Primitive(Primitive::String)
			} else {
				let generic_args = lower_type_args(args, depth);
				match owner {
					// `Outer<T>.Inner` — the owner carries type arguments of
					// its own; project the member type out of the fully
					// lowered owner so nothing is dropped.
					Some(owner_mirror) => IrType::QualifiedPath(QualifiedPath {
						name:              simple_name(name).to_string(),
						generic_arguments: generic_args,
						self_type:         Box::new(lower_type_depth(owner_mirror, depth + 1)),
						tr:                None,
					}),
					None => IrType::TypeReference(TypeReference {
						identifier: name.clone(),
						generic_args,
					}),
				}
			}
		}

		// A Java array's length is a runtime property, never part of the
		// type — the dynamically-sized sequence form is the faithful one.
		schema::TypeMirror::Array { component, .. } => {
			IrType::Slice(Box::new(lower_type_depth(component, depth + 1)))
		}

		schema::TypeMirror::Typevar { name, .. } => {
			IrType::GenericParam(GenericParam { name: name.clone(), kind: None })
		}

		// Use-site variance. Operator ↔ `ir::generics::Variance`:
		// `"? extends"` = Covariant, `"? super"` = Contravariant,
		// `"?"` = Bivariant (unbounded; upper bound is `Object` = Any).
		schema::TypeMirror::Wildcard { extends_bound, super_bound } => {
			match (extends_bound, super_bound) {
				(Some(bound), _) => IrType::TypeOperator(TypeOperator {
					operator: "? extends".to_string(),
					r#type:   Box::new(lower_type_depth(bound, depth + 1)),
				}),
				(None, Some(bound)) => IrType::TypeOperator(TypeOperator {
					operator: "? super".to_string(),
					r#type:   Box::new(lower_type_depth(bound, depth + 1)),
				}),
				(None, None) => IrType::TypeOperator(TypeOperator {
					operator: "?".to_string(),
					r#type:   Box::new(IrType::Any),
				}),
			}
		}

		schema::TypeMirror::Intersection { bounds } => IrType::Intersection(
			bounds.iter().map(|b| lower_type_depth(b, depth + 1)).collect(),
		),

		schema::TypeMirror::Union { alternatives } => IrType::Union(
			alternatives.iter().map(|a| lower_type_depth(a, depth + 1)).collect(),
		),

		// Unresolvable in the oracle's compile environment (a missing
		// dependency, usually). The source text is the best identity we
		// have; keeping it referable beats dropping it.
		schema::TypeMirror::Error { name } => IrType::TypeReference(TypeReference {
			identifier:   name.clone(),
			generic_args: None,
		}),

		schema::TypeMirror::None => IrType::Infer,

		// JLS §4.1's null type: a real type with exactly one value. The IR
		// has no bottom-of-references primitive; `Never` (uninhabited)
		// would be wrong, so it stays a named reference.
		schema::TypeMirror::Null => IrType::TypeReference(TypeReference {
			identifier:   "null".to_string(),
			generic_args: None,
		}),

		schema::TypeMirror::Other { repr } => IrType::TypeReference(TypeReference {
			identifier:   repr.clone(),
			generic_args: None,
		}),
	};

	apply_annotations(base, type_use_annotations(t))
}

/// The primitive algebra. See the module doc for the `char` decision.
fn lower_primitive(name: &str) -> IrType {
	match name {
		"boolean" => IrType::Primitive(Primitive::Bool),
		"byte" => IrType::Primitive(Primitive::Int(Width::W8)),
		"short" => IrType::Primitive(Primitive::Int(Width::W16)),
		"int" => IrType::Primitive(Primitive::Int(Width::W32)),
		"long" => IrType::Primitive(Primitive::Int(Width::W64)),
		"char" => IrType::Primitive(Primitive::Char),
		"float" => IrType::Primitive(Primitive::Float(Width::W32)),
		"double" => IrType::Primitive(Primitive::Float(Width::W64)),
		// Future-proofing: an unknown primitive name stays referable.
		other => IrType::TypeReference(TypeReference {
			identifier:   other.to_string(),
			generic_args: None,
		}),
	}
}

fn lower_type_args(args: &[schema::TypeMirror], depth: usize) -> Option<Vec<GenericArg>> {
	if args.is_empty() {
		return None;
	}
	Some(args.iter().map(|a| GenericArg::Type(lower_type_depth(a, depth + 1))).collect())
}

/// The trailing segment of a dotted qualified name.
pub fn simple_name(qualified: &str) -> &str {
	qualified.rsplit('.').next().unwrap_or(qualified)
}

// ---------------------------------------------------------------------------
// Declaration-site generics
// ---------------------------------------------------------------------------

/// Lower a declaration-site type-parameter list (`<T extends A & B>`) into
/// IR [`Generics`].
///
/// Java type parameters are declaration-site *invariant* (variance is
/// expressed at use sites via wildcards), so every parameter carries
/// `Variance::Invariant`; each bound becomes a `Constraint::TraitBound` on
/// the parameter's name. The implicit `java.lang.Object` bound is elided.
pub fn lower_type_params(type_params: &[schema::TypeParam]) -> Option<Generics> {
	if type_params.is_empty() {
		return None;
	}

	let mut params = Vec::new();
	let mut constraints = Vec::new();

	for tp in type_params {
		params.push(IrParameter::Type(TypeParam {
			name:         Some(tp.name.clone()),
			kind:         Kind::Type,
			variance:     Variance::Invariant,
			default_type: None,
			params:       None,
			origin:       TypeParamOrigin::Free,
		}));
		for bound in &tp.bounds {
			if bound.declared_name() == Some("java.lang.Object") {
				continue;
			}
			constraints.push(Constraint::TraitBound {
				param:     tp.name.clone(),
				trait_ref: trait_ref(bound),
			});
		}
	}

	Some(Generics { params, constraints })
}

/// Project a type mirror into a [`TraitRef`] (used for bounds and
/// super-interfaces): the fully qualified name plus name-based args.
pub fn trait_ref(t: &schema::TypeMirror) -> TraitRef {
	let expr = type_expr(t);
	TraitRef { name: expr.name, args: expr.args }
}

/// Project a type mirror into the *name-based* [`TypeExpr`] grammar used
/// inside constraints and trait references. Structure is preserved
/// recursively through `args`; composite heads use Java syntax as the name
/// (`[]` for arrays, `? extends` / `? super` / `?` for wildcards, `&` / `|`
/// for intersections/unions).
pub fn type_expr(t: &schema::TypeMirror) -> TypeExpr {
	match t {
		schema::TypeMirror::Primitive { name, .. } => {
			TypeExpr { name: name.clone(), args: Vec::new() }
		}
		schema::TypeMirror::Void => TypeExpr { name: "void".to_string(), args: Vec::new() },
		schema::TypeMirror::Declared { name, args, .. } => TypeExpr {
			name: name.clone(),
			args: args.iter().map(type_expr).collect(),
		},
		schema::TypeMirror::Array { component, .. } => {
			TypeExpr { name: "[]".to_string(), args: vec![type_expr(component)] }
		}
		schema::TypeMirror::Typevar { name, .. } => {
			TypeExpr { name: name.clone(), args: Vec::new() }
		}
		schema::TypeMirror::Wildcard { extends_bound, super_bound } => {
			match (extends_bound, super_bound) {
				(Some(bound), _) => TypeExpr {
					name: "? extends".to_string(),
					args: vec![type_expr(bound)],
				},
				(None, Some(bound)) => TypeExpr {
					name: "? super".to_string(),
					args: vec![type_expr(bound)],
				},
				(None, None) => TypeExpr { name: "?".to_string(), args: Vec::new() },
			}
		}
		schema::TypeMirror::Intersection { bounds } => TypeExpr {
			name: "&".to_string(),
			args: bounds.iter().map(type_expr).collect(),
		},
		schema::TypeMirror::Union { alternatives } => TypeExpr {
			name: "|".to_string(),
			args: alternatives.iter().map(type_expr).collect(),
		},
		schema::TypeMirror::Error { name } => TypeExpr { name: name.clone(), args: Vec::new() },
		schema::TypeMirror::None => TypeExpr { name: "<none>".to_string(), args: Vec::new() },
		schema::TypeMirror::Null => TypeExpr { name: "null".to_string(), args: Vec::new() },
		schema::TypeMirror::Other { repr } => TypeExpr { name: repr.clone(), args: Vec::new() },
	}
}

// ---------------------------------------------------------------------------
// Constants and annotation rendering
// ---------------------------------------------------------------------------

/// Lower an oracle [`schema::Value`] (compile-time constant or annotation
/// value) into the IR's [`ConstExpr`] grammar.
///
/// * `char` values ride as one-character strings (`ConstExpr` has no char
///   literal);
/// * enum-constant references become `Var("com.example.Level.HIGH")`;
/// * class literals become `Var("java.lang.String.class")`;
/// * arrays become `Call { func: "array", args }` (the grammar has no
///   sequence literal);
/// * nested annotation values become `Var` over their rendered form.
pub fn lower_value(v: &schema::Value) -> ConstExpr {
	match v {
		schema::Value::String { value } => ConstExpr::Str(value.clone()),
		schema::Value::Boolean { value } => ConstExpr::Bool(*value),
		schema::Value::Char { value } => ConstExpr::Str(value.clone()),
		schema::Value::Double { value } => ConstExpr::Float(*value),
		schema::Value::Int { value } => ConstExpr::Int(*value),
		schema::Value::Enum { ty, name } => ConstExpr::Var(format!("{ty}.{name}")),
		schema::Value::Type { value } => {
			ConstExpr::Var(format!("{}.class", type_expr_display(&type_expr(value))))
		}
		schema::Value::Array { values } => ConstExpr::Call {
			func: "array".to_string(),
			args: values.iter().map(lower_value).collect(),
		},
		schema::Value::Annotation { value } => ConstExpr::Var(render_annotation(value)),
		schema::Value::Other { repr } => ConstExpr::Var(repr.clone()),
	}
}

/// Render a [`schema::Value`] to compact display text (for documentation
/// notes and decorator strings).
pub fn render_value(v: &schema::Value) -> String {
	match v {
		schema::Value::String { value } => format!("{value:?}"),
		schema::Value::Boolean { value } => value.to_string(),
		schema::Value::Char { value } => format!("'{value}'"),
		schema::Value::Double { value } => value.to_string(),
		schema::Value::Int { value } => value.to_string(),
		schema::Value::Enum { ty, name } => format!("{ty}.{name}"),
		schema::Value::Type { value } => {
			format!("{}.class", type_expr_display(&type_expr(value)))
		}
		schema::Value::Array { values } => {
			let inner: Vec<String> = values.iter().map(render_value).collect();
			format!("{{{}}}", inner.join(", "))
		}
		schema::Value::Annotation { value } => render_annotation(value),
		schema::Value::Other { repr } => repr.clone(),
	}
}

/// Render an annotation use to its source-like form:
/// `@com.example.Marked(value = "x", retries = 5)`.
pub fn render_annotation(a: &schema::Annotation) -> String {
	if a.values.is_empty() {
		return format!("@{}", a.ty);
	}
	let values: Vec<String> =
		a.values.iter().map(|(k, v)| format!("{k} = {}", render_value(v))).collect();
	format!("@{}({})", a.ty, values.join(", "))
}

/// Render a type mirror to compact display text (for documentation notes
/// where no structural slot exists).
pub fn type_display(t: &schema::TypeMirror) -> String {
	type_expr_display(&type_expr(t))
}

/// Render a name-based type expression back to display text.
fn type_expr_display(expr: &TypeExpr) -> String {
	if expr.args.is_empty() {
		return expr.name.clone();
	}
	let args: Vec<String> = expr.args.iter().map(type_expr_display).collect();
	match expr.name.as_str() {
		"[]" => format!("{}[]", args.join(", ")),
		"&" => args.join(" & "),
		"|" => args.join(" | "),
		"? extends" | "? super" => format!("{} {}", expr.name, args.join(", ")),
		_ => format!("{}<{}>", expr.name, args.join(", ")),
	}
}

// ---------------------------------------------------------------------------
// Tests (pure — no oracle required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn declared(name: &str, args: Vec<schema::TypeMirror>) -> schema::TypeMirror {
		schema::TypeMirror::Declared {
			name: name.to_string(),
			args,
			owner: None,
			annotations: Vec::new(),
		}
	}

	#[test]
	fn primitives_map_to_width() {
		assert_eq!(
			lower_type(&schema::TypeMirror::Primitive {
				name:        "long".to_string(),
				annotations: Vec::new(),
			}),
			IrType::Primitive(Primitive::Int(Width::W64))
		);
		assert_eq!(
			lower_type(&schema::TypeMirror::Primitive {
				name:        "char".to_string(),
				annotations: Vec::new(),
			}),
			IrType::Primitive(Primitive::Char)
		);
	}

	#[test]
	fn object_and_string_are_special() {
		assert_eq!(lower_type(&declared("java.lang.Object", Vec::new())), IrType::Any);
		assert_eq!(
			lower_type(&declared("java.lang.String", Vec::new())),
			IrType::Primitive(Primitive::String)
		);
		// Boxed primitives stay references.
		assert!(matches!(
			lower_type(&declared("java.lang.Integer", Vec::new())),
			IrType::TypeReference(_)
		));
	}

	#[test]
	fn wildcards_carry_variance_operators() {
		let extends = schema::TypeMirror::Wildcard {
			extends_bound: Some(Box::new(declared("java.lang.Number", Vec::new()))),
			super_bound:   None,
		};
		match lower_type(&extends) {
			IrType::TypeOperator(op) => assert_eq!(op.operator, "? extends"),
			other => panic!("expected TypeOperator, got {other:?}"),
		}

		let unbounded =
			schema::TypeMirror::Wildcard { extends_bound: None, super_bound: None };
		match lower_type(&unbounded) {
			IrType::TypeOperator(op) => {
				assert_eq!(op.operator, "?");
				assert_eq!(*op.r#type, IrType::Any);
			}
			other => panic!("expected TypeOperator, got {other:?}"),
		}
	}

	#[test]
	fn generic_owner_becomes_qualified_path() {
		let inner = schema::TypeMirror::Declared {
			name:        "com.example.Outer.Inner".to_string(),
			args:        Vec::new(),
			owner:       Some(Box::new(declared(
				"com.example.Outer",
				vec![declared("java.lang.String", Vec::new())],
			))),
			annotations: Vec::new(),
		};
		match lower_type(&inner) {
			IrType::QualifiedPath(qp) => {
				assert_eq!(qp.name, "Inner");
				assert_eq!(*qp.self_type, IrType::TypeReference(TypeReference {
					identifier:   "com.example.Outer".to_string(),
					generic_args: Some(vec![GenericArg::Type(IrType::Primitive(
						Primitive::String,
					))]),
				}));
			}
			other => panic!("expected QualifiedPath, got {other:?}"),
		}
	}

	#[test]
	fn bounds_become_constraints() {
		let params = vec![schema::TypeParam {
			name:        "T".to_string(),
			bounds:      vec![
				declared("java.lang.Object", Vec::new()),
				declared("java.lang.Comparable", vec![schema::TypeMirror::Typevar {
					name:        "T".to_string(),
					annotations: Vec::new(),
				}]),
			],
			annotations: Vec::new(),
		}];
		let generics = lower_type_params(&params).expect("one parameter");
		assert_eq!(generics.params.len(), 1);
		// The Object bound is elided; the Comparable bound survives.
		assert_eq!(generics.constraints.len(), 1);
		match &generics.constraints[0] {
			Constraint::TraitBound { param, trait_ref } => {
				assert_eq!(param, "T");
				assert_eq!(trait_ref.name, "java.lang.Comparable");
				assert_eq!(trait_ref.args[0].name, "T");
			}
			other => panic!("expected TraitBound, got {other:?}"),
		}
	}

	#[test]
	fn visibility_defaults_to_package() {
		assert_eq!(visibility(&["static".to_string()]), Visibility::Package);
		assert_eq!(visibility(&["public".to_string()]), Visibility::Public);
		assert_eq!(visibility(&["protected".to_string()]), Visibility::Protected);
		assert_eq!(visibility(&["private".to_string()]), Visibility::Private);
	}

	#[test]
	fn values_render_and_lower() {
		let arr = schema::Value::Array {
			values: vec![
				schema::Value::Int { value: 1 },
				schema::Value::Enum { ty: "L".to_string(), name: "HIGH".to_string() },
			],
		};
		assert_eq!(render_value(&arr), "{1, L.HIGH}");
		match lower_value(&arr) {
			ConstExpr::Call { func, args } => {
				assert_eq!(func, "array");
				assert_eq!(args.len(), 2);
			}
			other => panic!("expected Call, got {other:?}"),
		}
	}

	#[test]
	#[test]
	fn type_use_annotations_become_operators() {
		let annotated = schema::TypeMirror::Declared {
			name: "java.lang.String".to_string(),
			args: Vec::new(),
			owner: None,
			annotations: vec![schema::Annotation {
				ty: "org.jspecify.annotations.NonNull".to_string(),
				values: Default::default(),
			}],
		};
		match lower_type(&annotated) {
			IrType::TypeOperator(op) => {
				assert_eq!(op.operator, "@org.jspecify.annotations.NonNull");
				assert_eq!(*op.r#type, IrType::Primitive(Primitive::String));
			}
			other => panic!("expected TypeOperator, got {other:?}"),
		}
	}

	#[test]
	fn apply_annotations_wraps_outer_first() {
		use std::collections::BTreeMap;
		let base = IrType::Primitive(Primitive::Int(Width::W32));
		let anns = vec![
			schema::Annotation { ty: "A".to_string(), values: BTreeMap::new() },
			schema::Annotation { ty: "B".to_string(), values: BTreeMap::new() },
		];
		match apply_annotations(base, &anns) {
			IrType::TypeOperator(outer) => {
				assert_eq!(outer.operator, "@A");
				match *outer.r#type {
					IrType::TypeOperator(inner) => {
						assert_eq!(inner.operator, "@B");
						assert_eq!(*inner.r#type, IrType::Primitive(Primitive::Int(Width::W32)));
					}
					other => panic!("expected inner TypeOperator, got {other:?}"),
				}
			}
			other => panic!("expected TypeOperator, got {other:?}"),
		}
	}
}
