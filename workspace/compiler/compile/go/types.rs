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
//! * `map[K]V` → `TypeOperator { operator: "map", type: Tuple([K, V]) }` —
//!   first-class operator so K/V are not collapsed to `any` by renderers
//!   that special-case the operator (the previous index-signature
//!   `RecordLiteral` encoding remains recoverable as structure);
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

use ir::function::Attribute;

use super::error::{GoError, Result};
use ir::generics::{
	ConstExpr, Constraint, GenericArg, Generics, Kind, Predicate, Term, TraitRef, TypeExpr, UnaryOp,
	Variance,
};
use ir::kind::Visibility;
use ir::parameter::{LiteralParameter, Parameter as IrParameter, ParameterAttribute, TypeParam, TypeParamOrigin};
use ir::primitives::{Primitive, Width};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record};
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

		// First-class map: operator "map" over a 2-tuple (K, V). Keeps
		// both type arguments structural (renderers special-case "map").
		TypeKind::Map => IrType::TypeOperator(TypeOperator {
			operator: "map".to_string(),
			r#type:   Box::new(IrType::Tuple(vec![
				lower_elem(&t.key, depth),
				lower_elem(&t.value, depth),
			])),
		}),

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
/// Identity-preserving universe aliases:
/// * `byte` → `TypeReference("byte")` (not collapsed into `uint8`/`UInt(W8)`)
/// * `rune` → `Char` (Go's conventional code-point alias)
/// * `unsafe.Pointer` → `TypeReference("unsafe.Pointer")` (not `Address`/
///   `uintptr` — those are a different Go type)
///
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
		// `uint8` is the primitive; `byte` is a distinct universe alias
		// spelling that must remain distinguishable in the IR.
		"uint8" => IrType::Primitive(Primitive::UInt(Width::W8)),
		"byte" => IrType::TypeReference(TypeReference {
			identifier:   "byte".to_string(),
			generic_args: None,
		}),
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
		// Distinct from uintptr / Primitive::Address.
		"unsafe.Pointer" => IrType::TypeReference(TypeReference {
			identifier:   "unsafe.Pointer".to_string(),
			generic_args: None,
		}),
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
			return Err(GoError::OracleSchema { detail: "oracle `elem` node missing `elem`" });
		}
		TypeKind::Map if t.key.is_none() || t.value.is_none() => {
			return Err(GoError::OracleSchema { detail: "oracle map node missing `key`/`value`" });
		}
		_ => Ok(()),
	}
}

// ---------------------------------------------------------------------------
// Constant values
// ---------------------------------------------------------------------------

/// Parse a go/constant `ExactString` into a structured [`ConstExpr`].
///
/// Covers the common cases the oracle emits for package-level consts:
/// booleans, string literals (double-quoted or raw backticks), integer
/// and floating-point literals (including leading `+`/`-`), and the
/// rational form `num/den` that go/constant uses for untyped floats.
/// Everything else falls back to [`ConstExpr::Var`] with the original
/// spelling so no information is dropped.
pub fn parse_const_value(raw: &str) -> Option<ConstExpr> {
	let s = raw.trim();
	if s.is_empty() {
		return None;
	}

	match s {
		"true" => return Some(ConstExpr::Bool(true)),
		"false" => return Some(ConstExpr::Bool(false)),
		_ => {}
	}

	if let Some(inner) = strip_go_string(s) {
		return Some(ConstExpr::Str(inner));
	}

	// Leading unary + / −.
	if let Some(rest) = s.strip_prefix('+') {
		return parse_const_value(rest);
	}
	if let Some(rest) = s.strip_prefix('-') {
		return parse_const_value(rest).map(|inner| match inner {
			ConstExpr::Int(n) => ConstExpr::Int(-n),
			ConstExpr::Float(f) => ConstExpr::Float(-f),
			other => ConstExpr::UnaryOp { op: UnaryOp::Neg, operand: Box::new(other) },
		});
	}

	// go/constant rationals for untyped floats: "1/2", "22/7".
	if let Some((num, den)) = s.split_once('/') {
		if let (Ok(n), Ok(d)) = (num.trim().parse::<f64>(), den.trim().parse::<f64>()) {
			if d != 0.0 {
				return Some(ConstExpr::Float(n / d));
			}
		}
	}

	if let Ok(n) = s.parse::<i64>() {
		return Some(ConstExpr::Int(n));
	}
	// Decimal / hex / octal / binary integers that don't fit the simple
	// parse above (e.g. "0xff") — try radix-aware parse via Go-like prefixes.
	if let Some(n) = parse_go_int(s) {
		return Some(ConstExpr::Int(n));
	}
	if let Ok(f) = s.parse::<f64>() {
		return Some(ConstExpr::Float(f));
	}

	// Named reference / unevaluated expression (iota expressions already
	// reduced by go/constant when ExactString is used).
	Some(ConstExpr::Var(s.to_string()))
}

/// Strip surrounding Go string quotes and return the unescaped content.
/// Handles `"…"` (interpreted) and `` `…` `` (raw); minimal unescape for
/// common interpreted escapes.
fn strip_go_string(s: &str) -> Option<String> {
	if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
		let inner = &s[1..s.len() - 1];
		return Some(unescape_go_double(inner));
	}
	if s.len() >= 2 && s.starts_with('`') && s.ends_with('`') {
		return Some(s[1..s.len() - 1].to_string());
	}
	// Single-quoted rune literal → one-char string (or keep as int via Var).
	if s.len() >= 3 && s.starts_with('\'') && s.ends_with('\'') {
		let inner = &s[1..s.len() - 1];
		return Some(unescape_go_double(inner));
	}
	None
}

fn unescape_go_double(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars().peekable();
	while let Some(c) = chars.next() {
		if c != '\\' {
			out.push(c);
			continue;
		}
		match chars.next() {
			Some('n') => out.push('\n'),
			Some('t') => out.push('\t'),
			Some('r') => out.push('\r'),
			Some('\\') => out.push('\\'),
			Some('"') => out.push('"'),
			Some('\'') => out.push('\''),
			Some('0') => out.push('\0'),
			Some(other) => {
				out.push('\\');
				out.push(other);
			}
			None => out.push('\\'),
		}
	}
	out
}

/// Parse Go integer spellings: `0x…`, `0o…`, `0b…`, bare decimal.
fn parse_go_int(s: &str) -> Option<i64> {
	let (radix, digits) = if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
		(16, rest)
	} else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
		(8, rest)
	} else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
		(2, rest)
	} else {
		return None;
	};
	// Drop underscores allowed in Go numeric literals.
	let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
	i64::from_str_radix(&cleaned, radix).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn byte_and_uint8_are_distinct() {
		let byte = lower_basic("byte");
		let uint8 = lower_basic("uint8");
		assert_ne!(byte, uint8, "byte must not collapse to uint8");
		assert!(
			matches!(
				byte,
				IrType::TypeReference(TypeReference { ref identifier, .. }) if identifier == "byte"
			),
			"byte → TypeReference(byte), got {byte:?}"
		);
		assert!(
			matches!(uint8, IrType::Primitive(Primitive::UInt(Width::W8))),
			"uint8 → UInt(W8), got {uint8:?}"
		);
	}

	#[test]
	fn unsafe_pointer_is_type_reference() {
		let t = lower_basic("unsafe.Pointer");
		assert!(
			matches!(
				t,
				IrType::TypeReference(TypeReference { ref identifier, .. })
					if identifier == "unsafe.Pointer"
			),
			"unsafe.Pointer must not become Address/uintptr, got {t:?}"
		);
	}

	#[test]
	fn map_is_type_operator_with_kv_tuple() {
		let t = oracle::Type {
			kind: TypeKind::Map,
			key: Some(Box::new(oracle::Type {
				kind: TypeKind::Basic,
				name: "string".into(),
				..Default::default()
			})),
			value: Some(Box::new(oracle::Type {
				kind: TypeKind::Basic,
				name: "int".into(),
				..Default::default()
			})),
			..Default::default()
		};
		let lowered = lower_type(&t);
		match lowered {
			IrType::TypeOperator(op) => {
				assert_eq!(op.operator, "map");
				match *op.r#type {
					IrType::Tuple(parts) => {
						assert_eq!(parts.len(), 2);
						assert!(matches!(parts[0], IrType::Primitive(Primitive::String)));
						assert!(matches!(parts[1], IrType::Primitive(Primitive::Int(_))));
					}
					other => panic!("map payload should be Tuple, got {other:?}"),
				}
			}
			other => panic!("map should be TypeOperator, got {other:?}"),
		}
	}

	#[test]
	fn parse_const_int_string_bool() {
		assert_eq!(parse_const_value("3"), Some(ConstExpr::Int(3)));
		assert_eq!(parse_const_value("-7"), Some(ConstExpr::Int(-7)));
		assert_eq!(parse_const_value("true"), Some(ConstExpr::Bool(true)));
		assert_eq!(
			parse_const_value(r#""hello""#),
			Some(ConstExpr::Str("hello".into()))
		);
		assert_eq!(parse_const_value("0xff"), Some(ConstExpr::Int(255)));
		assert_eq!(parse_const_value("1/2"), Some(ConstExpr::Float(0.5)));
		assert_eq!(
			parse_const_value("SomeName"),
			Some(ConstExpr::Var("SomeName".into()))
		);
	}
}
