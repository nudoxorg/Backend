//! Lowering the oracle's structural [`schema::TypeSig`] tree into
//! `ir::ty::Type` at full fidelity — no stringified types (CSHARP-PLAN §3.5).
//!
//! Track A: everything maps onto the existing IR. The notable Track-A lossy
//! spots are documented at their arms — 3-state nullability collapses the
//! oblivious case to a bare type, and `decimal` rides a `TypeReference`
//! (there is no `Primitive::Decimal` yet — Track B item 1).

use ir::generics::{ConstExpr, Constraint, GenericArg, Generics, Kind, TraitRef, TypeExpr, Variance};
use ir::kind::Visibility;
use ir::parameter::{Parameter as IrParameter, TypeParam, TypeParamOrigin};
use ir::primitives::{Primitive, Width};
use ir::ty::{
	FunctionPointer as IrFunctionPointer, GenericParam, QualifiedPath, TupleMember,
	Type as IrType, TypeOperator, TypeReference,
};

use super::schema::{self, Nullability, TypeSig};

/// Defensive recursion bound, mirroring the oracle's own depth guard.
const MAX_DEPTH: usize = 64;

/// Map a Roslyn accessibility token onto the IR visibility vocabulary
/// (CSHARP-PLAN §3.6). `protected internal` widens to `Protected`;
/// `private protected` narrows to `Package`; both keep a doc note upstream.
pub fn visibility(accessibility: &str) -> Visibility {
	match accessibility {
		"public" => Visibility::Public,
		"protected" => Visibility::Protected,
		"internal" => Visibility::Internal,
		// `protected internal` = protected OR internal — surfaced as Protected.
		"protectedInternal" => Visibility::Protected,
		// `private protected` = protected AND internal — closest is package-ish.
		"privateProtected" => Visibility::Package,
		"private" => Visibility::Private,
		// Unknown / unset accessibility defaults to the C# member default
		// (private); type-level default is internal, handled by the oracle.
		_ => Visibility::Private,
	}
}

/// Strip the metadata arity backticks (`List\`1` → `List`) from a name; the
/// generic-argument list already conveys arity, and the backticked form is
/// noise in rendered signatures. Backticks never occur otherwise in C# names.
pub fn strip_arity(name: &str) -> String {
	if !name.contains('`') {
		return name.to_string();
	}
	let mut out = String::with_capacity(name.len());
	let mut chars = name.chars().peekable();
	while let Some(c) = chars.next() {
		if c == '`' {
			while chars.peek().is_some_and(char::is_ascii_digit) {
				chars.next();
			}
		} else {
			out.push(c);
		}
	}
	out
}

/// The trailing segment of a dotted qualified name (arity stripped).
pub fn simple_name(qualified: &str) -> String {
	let bare = strip_arity(qualified);
	bare.rsplit('.').next().unwrap_or(&bare).to_string()
}

/// Lower an oracle type signature into the IR type algebra.
pub fn lower_type(t: &TypeSig) -> IrType {
	lower_type_depth(t, 0)
}

fn lower_type_depth(t: &TypeSig, depth: usize) -> IrType {
	if depth > MAX_DEPTH {
		return IrType::Infer;
	}

	match t {
		TypeSig::Named { name, args, owner, nullable, .. } => {
			let base = lower_named(name, args, owner.as_deref(), depth);
			apply_nullable(base, nullable)
		}

		TypeSig::TypeParam { name, nullable, .. } => {
			let base = IrType::GenericParam(GenericParam { name: name.clone(), kind: None });
			apply_nullable(base, nullable)
		}

		// SZ array → Slice (length is a runtime property). Multidimensional
		// arrays carry their rank in a `[,]`-style TypeOperator.
		TypeSig::Array { element, rank, nullable } => {
			let elem = lower_type_depth(element, depth + 1);
			let base = if *rank <= 1 {
				IrType::Slice(Box::new(elem))
			} else {
				let commas = ",".repeat((*rank as usize).saturating_sub(1));
				IrType::TypeOperator(TypeOperator {
					operator: format!("[{commas}]"),
					r#type:   Box::new(elem),
				})
			};
			apply_nullable(base, nullable)
		}

		// Unmanaged pointer — always writable in the C# model.
		TypeSig::Pointer { pointee } => IrType::RawPointer {
			is_mutable: true,
			r#type:     Box::new(lower_type_depth(pointee, depth + 1)),
		},

		// Function pointer (`delegate*<...>`): calling convention rides no
		// structural slot, so it is dropped here (kept in the `display` string).
		TypeSig::FuncPtr { params, return_type, .. } => {
			let inputs: Vec<IrParameter> = params
				.iter()
				.map(|p| {
					IrParameter::Literal(ir::parameter::LiteralParameter {
						name:          String::new(),
						r#type:        Some(lower_type_depth(p, depth + 1)),
						attributes:    None,
						default_value: None,
						description:   None,
					})
				})
				.collect();
			let outputs = return_type.as_ref().map(|ret| {
				vec![IrParameter::Literal(ir::parameter::LiteralParameter {
					name:          String::new(),
					r#type:        Some(lower_type_depth(ret, depth + 1)),
					attributes:    None,
					default_value: None,
					description:   None,
				})]
			});
			IrType::FunctionPointer(IrFunctionPointer {
				inputs: if inputs.is_empty() { None } else { Some(inputs) },
				outputs,
				attributes: None,
			})
		}

		// Value tuples: labelled → NamedTuple, all-unlabelled → plain Tuple.
		TypeSig::Tuple { elements, nullable } => {
			let base = if elements.iter().any(|e| e.name.is_some()) {
				IrType::NamedTuple(
					elements
						.iter()
						.map(|e| TupleMember {
							label:  e.name.clone(),
							r#type: lower_type_depth(&e.ty, depth + 1),
						})
						.collect(),
				)
			} else {
				IrType::Tuple(
					elements.iter().map(|e| lower_type_depth(&e.ty, depth + 1)).collect(),
				)
			};
			apply_nullable(base, nullable)
		}

		// `dynamic` — the IR top type, with the distinction kept in docs.
		TypeSig::Dynamic {} => IrType::Any,

		// `Nullable<T>` value type: an explicit reference so the wrapper is
		// never confused with reference-type `?` annotation.
		TypeSig::NullableValue { inner } => IrType::TypeReference(TypeReference {
			identifier:   "System.Nullable".to_string(),
			generic_args: Some(vec![GenericArg::Type(lower_type_depth(inner, depth + 1))]),
		}),

		// Unresolvable (a missing dependency ref, usually) — kept referable.
		TypeSig::Error { name } => IrType::TypeReference(TypeReference {
			identifier:   strip_arity(name),
			generic_args: None,
		}),
	}
}

/// Lower a `named` node: primitive special-cases first, then generic-owner
/// projection, then an ordinary type reference.
fn lower_named(name: &str, args: &[TypeSig], owner: Option<&TypeSig>, depth: usize) -> IrType {
	if let Some(prim) = lower_primitive(name) {
		return prim;
	}
	let generic_args = lower_type_args(args, depth);
	match owner {
		// `Outer<T>.Inner` — project the member out of the fully lowered owner
		// so the owner's type arguments are never dropped.
		Some(owner_sig) => IrType::QualifiedPath(QualifiedPath {
			name:              simple_name(name),
			generic_arguments: generic_args,
			self_type:         Box::new(lower_type_depth(owner_sig, depth + 1)),
			tr:                None,
		}),
		None => IrType::TypeReference(TypeReference {
			identifier: strip_arity(name),
			generic_args,
		}),
	}
}

/// The primitive algebra. C# integrals are signed/unsigned by type; `char` is
/// a UTF-16 code unit but its language role is "character" (→ `Char`).
/// Returns `None` for a non-primitive named type.
fn lower_primitive(name: &str) -> Option<IrType> {
	let prim = match name {
		"System.Boolean" => IrType::Primitive(Primitive::Bool),
		"System.SByte" => IrType::Primitive(Primitive::Int(Width::W8)),
		"System.Int16" => IrType::Primitive(Primitive::Int(Width::W16)),
		"System.Int32" => IrType::Primitive(Primitive::Int(Width::W32)),
		"System.Int64" => IrType::Primitive(Primitive::Int(Width::W64)),
		"System.Int128" => IrType::Primitive(Primitive::Int(Width::W128)),
		"System.Byte" => IrType::Primitive(Primitive::UInt(Width::W8)),
		"System.UInt16" => IrType::Primitive(Primitive::UInt(Width::W16)),
		"System.UInt32" => IrType::Primitive(Primitive::UInt(Width::W32)),
		"System.UInt64" => IrType::Primitive(Primitive::UInt(Width::W64)),
		"System.UInt128" => IrType::Primitive(Primitive::UInt(Width::W128)),
		// Native ints: the oracle emits `nint`/`nuint` for `IsNativeIntegerType`;
		// the underlying `System.IntPtr`/`UIntPtr` names map the same way.
		"nint" | "System.IntPtr" => IrType::Primitive(Primitive::Int(Width::Arch)),
		"nuint" | "System.UIntPtr" => IrType::Primitive(Primitive::UInt(Width::Arch)),
		"System.Half" => IrType::Primitive(Primitive::Float(Width::W16)),
		"System.Single" => IrType::Primitive(Primitive::Float(Width::W32)),
		"System.Double" => IrType::Primitive(Primitive::Float(Width::W64)),
		"System.Char" => IrType::Primitive(Primitive::Char),
		"System.String" => IrType::Primitive(Primitive::String),
		// The IR top type explicitly documents itself as `System.Object`.
		"System.Object" => IrType::Any,
		// `void` is the unit type (the empty tuple).
		"System.Void" => IrType::Tuple(Vec::new()),
		// Track A: no `Primitive::Decimal` yet — keep it a reference (Track B
		// item 1 promotes it to a dedicated primitive).
		_ => return None,
	};
	Some(prim)
}

/// Apply 3-state nullability to a lowered base type: an *annotated* reference
/// type becomes `T?` (a `"?"` TypeOperator); non-null and oblivious stay bare
/// (oblivious→bare is the documented Track-A lossy spot — pitfall #6/#10).
fn apply_nullable(base: IrType, nullable: &str) -> IrType {
	match Nullability::parse(nullable) {
		Nullability::Annotated => IrType::TypeOperator(TypeOperator {
			operator: "?".to_string(),
			r#type:   Box::new(base),
		}),
		Nullability::NotAnnotated | Nullability::Oblivious => base,
	}
}

fn lower_type_args(args: &[TypeSig], depth: usize) -> Option<Vec<GenericArg>> {
	if args.is_empty() {
		return None;
	}
	Some(args.iter().map(|a| GenericArg::Type(lower_type_depth(a, depth + 1))).collect())
}

// ---------------------------------------------------------------------------
// Declaration-site generics
// ---------------------------------------------------------------------------

/// Lower a declaration-site type-parameter list into IR [`Generics`].
///
/// C# variance is declaration-site (`in`/`out` on interfaces & delegates), so
/// each parameter carries its variance. Type constraints (`where T : Base`)
/// become `TraitBound`s; the special constraints (`class`, `struct`, `new()`,
/// `notnull`, `unmanaged`, `allows ref struct`) have no structural IR slot, so
/// they ride as synthetic trait bounds (Track A) — the least-lossy fit.
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
			variance:     variance_of(&tp.variance),
			default_type: None,
			params:       None,
			origin:       TypeParamOrigin::Free,
		}));

		let c = &tp.constraints;
		if c.reference_type {
			constraints.push(synthetic_bound(&tp.name, "class"));
		}
		if c.value_type {
			constraints.push(synthetic_bound(&tp.name, "struct"));
		}
		if c.not_null {
			constraints.push(synthetic_bound(&tp.name, "notnull"));
		}
		if c.unmanaged {
			constraints.push(synthetic_bound(&tp.name, "unmanaged"));
		}
		if c.allows_ref_like {
			constraints.push(synthetic_bound(&tp.name, "allows ref struct"));
		}
		for bound in &c.types {
			constraints.push(Constraint::TraitBound {
				param:     tp.name.clone(),
				trait_ref: trait_ref(bound),
			});
		}
		if c.constructor {
			constraints.push(synthetic_bound(&tp.name, "new()"));
		}
	}

	Some(Generics { params, constraints })
}

/// `in` → Contravariant, `out` → Covariant, else Invariant.
fn variance_of(variance: &str) -> Variance {
	match variance {
		"in" => Variance::Contravariant,
		"out" => Variance::Covariant,
		_ => Variance::Invariant,
	}
}

/// A synthetic trait bound for a special constraint keyword (no structural
/// slot exists for `class`/`struct`/`new()`/…).
fn synthetic_bound(param: &str, keyword: &str) -> Constraint {
	Constraint::TraitBound {
		param:     param.to_string(),
		trait_ref: TraitRef { name: keyword.to_string(), args: Vec::new() },
	}
}

/// Project a type signature into a [`TraitRef`] (for supertypes and bounds):
/// the FQN (arity stripped) plus name-based args.
pub fn trait_ref(t: &TypeSig) -> TraitRef {
	let expr = type_expr(t);
	TraitRef { name: expr.name, args: expr.args }
}

/// Project a type signature into the name-based [`TypeExpr`] grammar used
/// inside constraints and trait references (structure preserved through
/// `args`; composite heads use C# syntax as the name).
pub fn type_expr(t: &TypeSig) -> TypeExpr {
	match t {
		TypeSig::Named { name, args, .. } => TypeExpr {
			name: strip_arity(name),
			args: args.iter().map(type_expr).collect(),
		},
		TypeSig::TypeParam { name, .. } => TypeExpr { name: name.clone(), args: Vec::new() },
		TypeSig::Array { element, rank, .. } => {
			let commas = ",".repeat((*rank as usize).saturating_sub(1));
			TypeExpr { name: format!("[{commas}]"), args: vec![type_expr(element)] }
		}
		TypeSig::Pointer { pointee } => {
			TypeExpr { name: "*".to_string(), args: vec![type_expr(pointee)] }
		}
		TypeSig::FuncPtr { params, return_type, .. } => {
			let mut args: Vec<TypeExpr> = params.iter().map(type_expr).collect();
			if let Some(ret) = return_type {
				args.push(type_expr(ret));
			}
			TypeExpr { name: "delegate*".to_string(), args }
		}
		TypeSig::Tuple { elements, .. } => TypeExpr {
			name: "()".to_string(),
			args: elements.iter().map(|e| type_expr(&e.ty)).collect(),
		},
		TypeSig::Dynamic {} => TypeExpr { name: "dynamic".to_string(), args: Vec::new() },
		TypeSig::NullableValue { inner } => {
			TypeExpr { name: "System.Nullable".to_string(), args: vec![type_expr(inner)] }
		}
		TypeSig::Error { name } => TypeExpr { name: strip_arity(name), args: Vec::new() },
	}
}

// ---------------------------------------------------------------------------
// Display + constant rendering (for documentation notes with no structural slot)
// ---------------------------------------------------------------------------

/// Render a type signature to compact C# display text.
pub fn type_display(t: &TypeSig) -> String {
	type_expr_display(&type_expr(t))
}

/// Render a name-based type expression back to C# display text.
fn type_expr_display(expr: &TypeExpr) -> String {
	if expr.args.is_empty() {
		return expr.name.clone();
	}
	let args: Vec<String> = expr.args.iter().map(type_expr_display).collect();
	match expr.name.as_str() {
		s if s.starts_with('[') && s.ends_with(']') => format!("{}{}", args[0], s),
		"*" => format!("{}*", args.join("")),
		"()" => format!("({})", args.join(", ")),
		"delegate*" => format!("delegate*<{}>", args.join(", ")),
		_ => format!("{}<{}>", expr.name, args.join(", ")),
	}
}

/// Lower a constant display string into the IR's [`ConstExpr`] grammar. The
/// oracle already renders the value to text, so we classify by shape.
pub fn lower_constant(display: &str) -> ConstExpr {
	let trimmed = display.trim();
	if trimmed == "true" {
		return ConstExpr::Bool(true);
	}
	if trimmed == "false" {
		return ConstExpr::Bool(false);
	}
	if let Ok(i) = trimmed.parse::<i64>() {
		return ConstExpr::Int(i);
	}
	if let Ok(f) = trimmed.parse::<f64>() {
		return ConstExpr::Float(f);
	}
	if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
		return ConstExpr::Str(trimmed[1..trimmed.len() - 1].to_string());
	}
	ConstExpr::Var(trimmed.to_string())
}

/// Render an attribute use to its source-like form: `[System.Obsolete("x")]`.
pub fn render_attribute(a: &schema::Attr) -> String {
	let short = simple_name(&a.ty);
	// Attribute-name convention: `FooAttribute` is written `[Foo]`.
	let name = short.strip_suffix("Attribute").unwrap_or(&short);
	let mut args: Vec<String> = a.args.clone();
	args.extend(a.named.iter().map(|(k, v)| format!("{k} = {v}")));
	if args.is_empty() {
		format!("[{name}]")
	} else {
		format!("[{name}({})]", args.join(", "))
	}
}

// ---------------------------------------------------------------------------
// Tests (pure — no oracle required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	fn named(name: &str) -> TypeSig {
		TypeSig::Named {
			name:      name.to_string(),
			args:      Vec::new(),
			owner:     None,
			nullable:  "none".to_string(),
			type_kind: String::new(),
		}
	}

	#[test]
	fn primitives_map_to_width() {
		assert_eq!(lower_type(&named("System.Int64")), IrType::Primitive(Primitive::Int(Width::W64)));
		assert_eq!(lower_type(&named("System.Byte")), IrType::Primitive(Primitive::UInt(Width::W8)));
		assert_eq!(lower_type(&named("System.Char")), IrType::Primitive(Primitive::Char));
		assert_eq!(lower_type(&named("nint")), IrType::Primitive(Primitive::Int(Width::Arch)));
	}

	#[test]
	fn object_string_void_are_special() {
		assert_eq!(lower_type(&named("System.Object")), IrType::Any);
		assert_eq!(lower_type(&named("System.String")), IrType::Primitive(Primitive::String));
		assert_eq!(lower_type(&named("System.Void")), IrType::Tuple(Vec::new()));
	}

	#[test]
	fn arity_backticks_stripped() {
		assert_eq!(strip_arity("System.Collections.Generic.List`1"), "System.Collections.Generic.List");
		assert_eq!(simple_name("System.Collections.Generic.List`1"), "List");
	}

	#[test]
	fn annotated_reference_becomes_optional_operator() {
		let annotated = TypeSig::Named {
			name:      "System.String".to_string(),
			args:      Vec::new(),
			owner:     None,
			nullable:  "annotated".to_string(),
			type_kind: String::new(),
		};
		match lower_type(&annotated) {
			IrType::TypeOperator(op) => {
				assert_eq!(op.operator, "?");
				assert_eq!(*op.r#type, IrType::Primitive(Primitive::String));
			}
			other => panic!("expected TypeOperator, got {other:?}"),
		}
	}

	#[test]
	fn sz_array_is_slice_multidim_is_operator() {
		let sz = TypeSig::Array {
			element:  Box::new(named("System.Int32")),
			rank:     1,
			nullable: "none".to_string(),
		};
		assert!(matches!(lower_type(&sz), IrType::Slice(_)));
		let md = TypeSig::Array {
			element:  Box::new(named("System.Int32")),
			rank:     3,
			nullable: "none".to_string(),
		};
		match lower_type(&md) {
			IrType::TypeOperator(op) => assert_eq!(op.operator, "[,,]"),
			other => panic!("expected TypeOperator, got {other:?}"),
		}
	}

	#[test]
	fn visibility_mapping() {
		assert_eq!(visibility("public"), Visibility::Public);
		assert_eq!(visibility("internal"), Visibility::Internal);
		assert_eq!(visibility("protectedInternal"), Visibility::Protected);
		assert_eq!(visibility("privateProtected"), Visibility::Package);
		assert_eq!(visibility("private"), Visibility::Private);
	}
}
