//! Lowering the oracle's structural Go type tree into `ir::ty::Type`.
//!
//! Every Go type form maps to the closest *structural* IR form — no
//! stringified types. The notable mapping decisions (each documented at
//! its match arm):
//!
//! * named/alias types → [`TypeReference`] with a fully-qualified
//!   `import/path.Name` identifier and structural generic args;
//! * `*T` → `RawPointer { is_mutable: true }` — Go pointers are freely
//!   mutable and carry no borrow/lifetime semantics, so `BorrowedRef`
//!   would over-claim;
//! * `map[K]V` → an anonymous `RecordLiteral` holding one
//!   `Field::Pattern` index signature (`[K]: V`) — the IR has no map
//!   primitive and an index signature is exactly a map shape;
//! * `chan T` → `TypeOperator { operator: "chan"|"chan<-"|"<-chan" }` —
//!   the IR has no channel type; direction is preserved in the operator
//!   (lossy in kind, lossless in information);
//! * anonymous interfaces → structural: named embeddings and unions are
//!   intersected, explicit methods become function-typed fields of a
//!   `RecordLiteral` (Go interfaces are structural, so a record of
//!   function members is a faithful shape);
//! * constraint unions (`~int | string`) → `Union`, with `~T`
//!   approximation terms wrapped in `TypeOperator { operator: "~" }`;
//! * `complex64`/`complex128` → `TypeReference` — the IR `Primitive`
//!   algebra has no complex numbers.
//!
//! ## Recursion
//! Named types arrive from the oracle by *reference* and are never
//! inlined, so a cyclic Go type (only expressible through a named
//! intermediary) can never recurse here. A depth cap mirrors the
//! oracle's own defensive bound anyway.

use std::collections::HashMap;

use anyhow::{Result, bail};

use ir::function::Attribute;
use ir::generics::{Constraint, GenericArg, Generics, Kind, Predicate, Term, TraitRef, TypeExpr, Variance};
use ir::kind::Visibility;
use ir::parameter::{LiteralParameter, Parameter as IrParameter, ParameterAttribute, TypeParam, TypeParamOrigin};
use ir::primitives::{Primitive, Width};
use ir::record::{Field, FieldAttributes, FieldKey, IndexSignature, KnownField, Record};
use ir::ty::{FunctionPointer, GenericParam, Type as IrType, TypeOperator, TypeReference};

use super::oracle::{self, TypeKind};

/// Defensive recursion bound; see the module doc — anonymous Go types
/// cannot cycle, so this exists only to fail soft on malformed input.
const MAX_DEPTH: usize = 64;

/// Fully qualify a Go identifier as `import/path.Name`. Universe-scope
/// names (`error`, `comparable`) have no package and stay bare.
pub fn qualify(pkg: &str, name: &str) -> String {
	if pkg.is_empty() {
		name.to_string()
	} else {
		format!("{pkg}.{name}")
	}
}

/// Go's unexported names are visible throughout their package — the IR's
/// `Package` visibility, not `Private`.
pub fn visibility(exported: bool) -> Visibility {
	if exported { Visibility::Public } else { Visibility::Package }
}

/// Lower an oracle type node into the IR type algebra.
pub fn lower_type(t: &oracle::Type) -> IrType {
	lower_type_depth(t, 0)
}

fn lower_type_depth(t: &oracle::Type, depth: usize) -> IrType {
	if depth > MAX_DEPTH {
		return IrType::Infer;
	}

	match t.kind {
		TypeKind::Basic => lower_basic(&t.name),

		TypeKind::Named | TypeKind::Alias => {
			// `any` is the universe alias for the empty interface —
			// exactly the IR's top type.
			if t.pkg.is_empty() && t.name == "any" {
				return IrType::Any;
			}
			IrType::TypeReference(TypeReference {
				identifier:   qualify(&t.pkg, &t.name),
				generic_args: lower_type_args(&t.type_args, depth),
			})
		}

		TypeKind::TypeParam => {
			IrType::GenericParam(GenericParam { name: t.name.clone(), kind: None })
		}

		// Go pointers are GC-managed but freely mutable and carry no
		// borrow discipline; `RawPointer` (mutable) over-claims less
		// than `BorrowedRef` (which implies lifetimes Go lacks).
		TypeKind::Pointer => IrType::RawPointer {
			is_mutable: true,
			r#type:     Box::new(lower_elem(&t.elem, depth)),
		},

		TypeKind::Slice => IrType::Slice(Box::new(lower_elem(&t.elem, depth))),

		TypeKind::Array => IrType::Array {
			r#type: Box::new(lower_elem(&t.elem, depth)),
			length: t.len.max(0) as usize,
		},

		// The IR has no map primitive; a record with a single index
		// signature (`[K]: V`) is the same structural shape.
		TypeKind::Map => IrType::RecordLiteral(Box::new(record_shell(vec![Field::Pattern(
			IndexSignature {
				key_type:   Box::new(lower_elem(&t.key, depth)),
				value_type: Box::new(lower_elem(&t.value, depth)),
			},
		)]))),

		// The IR has no channel type. `TypeOperator` keeps the element
		// structural and records the directionality in the operator —
		// `chan` / `chan<-` (send-only) / `<-chan` (receive-only).
		TypeKind::Chan => IrType::TypeOperator(TypeOperator {
			operator: chan_operator(&t.dir).to_string(),
			r#type:   Box::new(lower_elem(&t.elem, depth)),
		}),

		TypeKind::Func => IrType::FunctionPointer(lower_function_pointer(t, depth)),

		TypeKind::Struct => IrType::RecordLiteral(Box::new(record_shell(lower_struct_fields(
			&t.fields, None,
		)))),

		TypeKind::Interface => lower_anonymous_interface(t, depth),

		TypeKind::Union => lower_union(t, depth),

		TypeKind::Tuple => {
			IrType::Tuple(t.types.iter().map(|inner| lower_type_depth(inner, depth + 1)).collect())
		}

		TypeKind::Invalid => IrType::Infer,
	}
}

/// Map a Go basic-type name onto the IR primitive algebra.
///
/// `rune` and `byte` keep their alias identities (the oracle preserves
/// the universe alias names): `rune` → `Char`, `byte` → `UInt(W8)`.
/// `complex64`/`complex128` have no `Primitive` counterpart and fall
/// back to `TypeReference` — a documented loss.
fn lower_basic(name: &str) -> IrType {
	// Untyped constant kinds ("untyped int", …) share their typed
	// counterparts' default representation.
	let name = name.strip_prefix("untyped ").unwrap_or(name);
	match name {
		"bool" => IrType::Primitive(Primitive::Bool),
		"string" => IrType::Primitive(Primitive::String),
		"int" => IrType::Primitive(Primitive::Int(Width::Arch)),
		"int8" => IrType::Primitive(Primitive::Int(Width::W8)),
		"int16" => IrType::Primitive(Primitive::Int(Width::W16)),
		"int32" => IrType::Primitive(Primitive::Int(Width::W32)),
		"int64" => IrType::Primitive(Primitive::Int(Width::W64)),
		"uint" => IrType::Primitive(Primitive::UInt(Width::Arch)),
		"uint8" | "byte" => IrType::Primitive(Primitive::UInt(Width::W8)),
		"uint16" => IrType::Primitive(Primitive::UInt(Width::W16)),
		"uint32" => IrType::Primitive(Primitive::UInt(Width::W32)),
		"uint64" => IrType::Primitive(Primitive::UInt(Width::W64)),
		// uintptr is an integer wide enough to hold a pointer, not a
		// pointer itself.
		"uintptr" => IrType::Primitive(Primitive::UInt(Width::Arch)),
		"rune" => IrType::Primitive(Primitive::Char),
		// An untyped float constant defaults to float64.
		"float32" => IrType::Primitive(Primitive::Float(Width::W32)),
		"float64" | "float" => IrType::Primitive(Primitive::Float(Width::W64)),
		"unsafe.Pointer" => IrType::Primitive(Primitive::Address),
		// No complex primitive in the IR — keep the name referable.
		other => IrType::TypeReference(TypeReference {
			identifier:   other.to_string(),
			generic_args: None,
		}),
	}
}

fn chan_operator(dir: &str) -> &'static str {
	match dir {
		"send" => "chan<-",
		"recv" => "<-chan",
		_ => "chan",
	}
}

fn lower_elem(elem: &Option<Box<oracle::Type>>, depth: usize) -> IrType {
	match elem {
		Some(inner) => lower_type_depth(inner, depth + 1),
		None => IrType::Infer,
	}
}

fn lower_type_args(args: &[oracle::Type], depth: usize) -> Option<Vec<GenericArg>> {
	if args.is_empty() {
		return None;
	}
	Some(args.iter().map(|a| GenericArg::Type(lower_type_depth(a, depth + 1))).collect())
}

/// Lower a func type node into a [`FunctionPointer`]. A variadic final
/// parameter keeps its slice type (as go/types reports it) and is marked
/// with `ParameterAttribute::Variadic`; the pointer itself carries
/// `Attribute::Variadic`.
fn lower_function_pointer(t: &oracle::Type, depth: usize) -> FunctionPointer {
	let inputs = lower_fn_params_depth(&t.params, t.variadic, depth);
	let outputs = lower_fn_results_depth(&t.results, depth);
	FunctionPointer {
		inputs:     if inputs.is_empty() { None } else { Some(inputs) },
		outputs:    if outputs.is_empty() { None } else { Some(outputs) },
		attributes: if t.variadic { Some(vec![Attribute::Variadic]) } else { None },
	}
}

/// Lower func parameters (shared with `function.rs` for top-level
/// function/method signatures).
pub fn lower_fn_params(params: &[oracle::Param], variadic: bool) -> Vec<IrParameter> {
	lower_fn_params_depth(params, variadic, 0)
}

fn lower_fn_params_depth(
	params: &[oracle::Param],
	variadic: bool,
	depth: usize,
) -> Vec<IrParameter> {
	let last = params.len().saturating_sub(1);
	params
		.iter()
		.enumerate()
		.map(|(idx, p)| {
			let is_variadic = variadic && idx == last;
			IrParameter::Literal(LiteralParameter {
				name:          p.name.clone(),
				r#type:        p.r#type.as_ref().map(|t| lower_type_depth(t, depth + 1)),
				attributes:    if is_variadic {
					Some(vec![ParameterAttribute::Variadic])
				} else {
					None
				},
				default_value: None,
				description:   None,
			})
		})
		.collect()
}

/// Lower func results — Go's multiple returns map one-to-one onto the
/// IR's `output_parameters` list, named results keeping their names.
pub fn lower_fn_results(results: &[oracle::Param]) -> Vec<IrParameter> {
	lower_fn_results_depth(results, 0)
}

fn lower_fn_results_depth(results: &[oracle::Param], depth: usize) -> Vec<IrParameter> {
	results
		.iter()
		.map(|r| {
			IrParameter::Literal(LiteralParameter {
				name:          r.name.clone(),
				r#type:        r.r#type.as_ref().map(|t| lower_type_depth(t, depth + 1)),
				attributes:    None,
				default_value: None,
				description:   None,
			})
		})
		.collect()
}

/// Lower struct fields into IR record fields.
///
/// Embedded fields are kept as *marked fields* rather than flattened:
/// the field keeps its implicit name and gains an `"embedded"` decorator
/// (the full promoted method/field surface is derivable, and flattening
/// would erase the source structure Go programmers navigate by). Raw
/// struct tags are preserved verbatim as a `tag:` decorator.
pub fn lower_struct_fields(
	fields: &[oracle::StructField],
	docs: Option<&HashMap<String, String>>,
) -> Vec<Field> {
	fields
		.iter()
		.map(|f| {
			let mut decorators = Vec::new();
			if f.embedded {
				decorators.push("embedded".to_string());
			}
			if !f.tag.is_empty() {
				decorators.push(format!("tag:{}", f.tag));
			}
			Field::Known(KnownField {
				key:           FieldKey::Ident(f.name.clone()),
				r#type:        f.r#type.as_ref().map(|t| Box::new(lower_type(t))),
				default_value: None,
				attributes:    FieldAttributes {
					decorators,
					// Go struct fields are always assignable.
					is_mutable: true,
					is_optional: false,
					is_static: false,
				},
				visibility:    Some(visibility(f.exported)),
				documentation: docs.and_then(|d| d.get(&f.name)).cloned(),
			})
		})
		.collect()
}

/// Lower an *anonymous* interface used in type position.
///
/// Go interfaces are structural, so the faithful shape is an
/// intersection of parts: named embeddings stay `TypeReference`s, union
/// embeddings become `Union`s, and explicit methods become
/// function-typed fields of a `RecordLiteral` (which preserves the
/// method names the IR's inline-`Function` slot would drop). The empty
/// interface is the IR's top type.
fn lower_anonymous_interface(t: &oracle::Type, depth: usize) -> IrType {
	if t.is_empty_interface() {
		return IrType::Any;
	}

	let mut parts: Vec<IrType> = Vec::new();
	for embedded in &t.embeddeds {
		parts.push(lower_type_depth(embedded, depth + 1));
	}

	if !t.explicit_methods.is_empty() {
		let fields = t
			.explicit_methods
			.iter()
			.map(|m| {
				Field::Known(KnownField {
					key:           FieldKey::Ident(m.name.clone()),
					r#type:        m
						.signature
						.as_ref()
						.map(|sig| Box::new(lower_type_depth(sig, depth + 1))),
					default_value: None,
					attributes:    FieldAttributes {
						decorators:  Vec::new(),
						is_mutable:  false,
						is_optional: false,
						is_static:   false,
					},
					visibility:    Some(visibility(m.exported)),
					documentation: None,
				})
			})
			.collect();
		parts.push(IrType::RecordLiteral(Box::new(record_shell(fields))));
	}

	match parts.len() {
		0 => IrType::Any,
		1 => parts.pop().expect("len checked"),
		_ => IrType::Intersection(parts),
	}
}

/// Lower a constraint union (`~int | string`) into an IR `Union`, with
/// approximation (`~T`) terms wrapped in a `"~"` type operator.
fn lower_union(t: &oracle::Type, depth: usize) -> IrType {
	let members = t
		.terms
		.iter()
		.map(|term| {
			let inner = match &term.r#type {
				Some(inner) => lower_type_depth(inner, depth + 1),
				None => IrType::Infer,
			};
			if term.tilde {
				IrType::TypeOperator(TypeOperator {
					operator: "~".to_string(),
					r#type:   Box::new(inner),
				})
			} else {
				inner
			}
		})
		.collect();
	IrType::Union(members)
}

/// An anonymous record carrying only the given fields.
fn record_shell(fields: Vec<Field>) -> Record {
	Record {
		name: None,
		generics: None,
		fields,
		call_signatures: None,
		constructors: None,
		methods: None,
		index_signatures: None,
		super_types: None,
		members: None,
		implemented_protocols: None,
	}
}

// ---------------------------------------------------------------------------
// Generics
// ---------------------------------------------------------------------------

/// Lower a generic type-parameter list (with constraints) into IR
/// [`Generics`].
///
/// Each parameter becomes a plain `TypeParam` of kind `*`; the
/// constraint interface is decomposed into the IR constraint vocabulary
/// by [`constraints_of`].
pub fn lower_generics(type_params: &[oracle::TypeParamDecl]) -> Option<Generics> {
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
		if let Some(constraint) = &tp.constraint {
			constraints.extend(constraints_of(&tp.name, constraint));
		}
	}

	Some(Generics { params, constraints })
}

/// Decompose a Go constraint into IR [`Constraint`]s (conjunctive):
///
/// * a named constraint interface → `TraitBound` on its qualified name;
/// * an embedded union / type set → `LogicalPredicate(Or(...))` whose
///   atoms are `TraitBound`s over the term names (`~` prefix marks
///   approximation terms) — the IR constraint grammar is name-based
///   (`TraitRef`), so the terms' *names* carry the set here while the
///   fully structural set remains available on the constraint interface
///   declaration itself;
/// * an explicit method requirement → `AssociatedItem` equating the
///   member name with its structural function type;
/// * `any` / the empty interface → no constraint.
pub fn constraints_of(param: &str, constraint: &oracle::Type) -> Vec<Constraint> {
	match constraint.kind {
		TypeKind::Named | TypeKind::Alias => {
			if constraint.pkg.is_empty() && constraint.name == "any" {
				return Vec::new();
			}
			vec![Constraint::TraitBound {
				param:     param.to_string(),
				trait_ref: TraitRef {
					name: qualify(&constraint.pkg, &constraint.name),
					args: constraint.type_args.iter().map(type_expr).collect(),
				},
			}]
		}

		TypeKind::Interface => {
			let mut out = Vec::new();
			for embedded in &constraint.embeddeds {
				if embedded.kind == TypeKind::Union {
					out.push(union_predicate(param, embedded));
				} else {
					out.extend(constraints_of(param, embedded));
				}
			}
			for method in &constraint.explicit_methods {
				let term_type = method
					.signature
					.as_ref()
					.map(lower_type)
					.unwrap_or(IrType::Infer);
				out.push(Constraint::AssociatedItem {
					name: method.name.clone(),
					args: None,
					term: Term::Equality(Box::new(term_type)),
				});
			}
			out
		}

		TypeKind::Union => vec![union_predicate(param, constraint)],

		// A bare concrete type in constraint position (valid Go:
		// `interface{ int }` collapses to this) — a single-term set.
		_ => {
			let expr = type_expr(constraint);
			vec![Constraint::TraitBound {
				param:     param.to_string(),
				trait_ref: TraitRef { name: expr.name, args: expr.args },
			}]
		}
	}
}

/// Render a union type set as an `Or` predicate of per-term bounds.
fn union_predicate(param: &str, union: &oracle::Type) -> Constraint {
	let atoms = union
		.terms
		.iter()
		.map(|term| {
			let expr = match &term.r#type {
				Some(t) => type_expr(t),
				None => TypeExpr { name: "<invalid>".to_string(), args: Vec::new() },
			};
			let name = if term.tilde { format!("~{}", expr.name) } else { expr.name };
			Predicate::Atom(Box::new(Constraint::TraitBound {
				param:     param.to_string(),
				trait_ref: TraitRef { name, args: expr.args },
			}))
		})
		.collect();
	Constraint::LogicalPredicate { pred: Predicate::Or(atoms) }
}

/// Project an oracle type into the *name-based* [`TypeExpr`] grammar used
/// inside constraints and trait references. Structure is preserved
/// recursively through `args`; composite heads use their Go syntax as
/// the name (`*`, `[]`, `map`, `chan`, …).
pub fn type_expr(t: &oracle::Type) -> TypeExpr {
	let elem_args = |elem: &Option<Box<oracle::Type>>| -> Vec<TypeExpr> {
		elem.as_deref().map(|e| vec![type_expr(e)]).unwrap_or_default()
	};

	match t.kind {
		TypeKind::Basic | TypeKind::TypeParam => {
			TypeExpr { name: t.name.clone(), args: Vec::new() }
		}
		TypeKind::Named | TypeKind::Alias => TypeExpr {
			name: qualify(&t.pkg, &t.name),
			args: t.type_args.iter().map(type_expr).collect(),
		},
		TypeKind::Pointer => TypeExpr { name: "*".to_string(), args: elem_args(&t.elem) },
		TypeKind::Slice => TypeExpr { name: "[]".to_string(), args: elem_args(&t.elem) },
		TypeKind::Array => {
			TypeExpr { name: format!("[{}]", t.len.max(0)), args: elem_args(&t.elem) }
		}
		TypeKind::Map => {
			let mut args = elem_args(&t.key);
			args.extend(elem_args(&t.value));
			TypeExpr { name: "map".to_string(), args }
		}
		TypeKind::Chan => TypeExpr {
			name: chan_operator(&t.dir).to_string(),
			args: elem_args(&t.elem),
		},
		TypeKind::Func => TypeExpr { name: "func".to_string(), args: Vec::new() },
		TypeKind::Struct => TypeExpr { name: "struct".to_string(), args: Vec::new() },
		TypeKind::Interface => TypeExpr { name: "interface".to_string(), args: Vec::new() },
		TypeKind::Union => TypeExpr {
			name: "union".to_string(),
			args: t
				.terms
				.iter()
				.filter_map(|term| term.r#type.as_ref().map(type_expr))
				.collect(),
		},
		TypeKind::Tuple => TypeExpr {
			name: "tuple".to_string(),
			args: t.types.iter().map(type_expr).collect(),
		},
		TypeKind::Invalid => TypeExpr { name: "<invalid>".to_string(), args: Vec::new() },
	}
}

/// A defensive check exposed for tests / callers that want to validate
/// oracle input early: rejects nodes whose kind-specific payload is
/// missing where structurally required.
pub fn validate(t: &oracle::Type) -> Result<()> {
	match t.kind {
		TypeKind::Pointer | TypeKind::Slice | TypeKind::Array | TypeKind::Chan
			if t.elem.is_none() =>
		{
			bail!("oracle `{:?}` node missing `elem`", t.kind)
		}
		TypeKind::Map if t.key.is_none() || t.value.is_none() => {
			bail!("oracle map node missing `key`/`value`")
		}
		_ => Ok(()),
	}
}
