//! Serde mirror of the C# Roslyn oracle's JSON document.
//!
//! The oracle (`oracle/Program.cs` + friends) prints one JSON object per
//! invocation; every struct/enum here mirrors that shape exactly (camelCase,
//! see CSHARP-PLAN §2.6). The oracle always emits every key (using `null` /
//! `[]` for absent values), but `#[serde(default)]` is applied liberally so
//! schema evolution on the C# side degrades gracefully instead of failing
//! deserialization.
//!
//! The one recursive shape is [`TypeSig`] (a structural type *use*, mirroring
//! Roslyn's `ITypeSymbol`), internally tagged by `"kind"`. String-typed
//! enumerations (accessibility, nullability, ref-kind, …) stay `String` so a
//! new oracle value never breaks the parse — the lowering interprets them.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The whole oracle output: one document per invocation.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extraction {
	/// Schema version stamped by the oracle (currently `1`).
	pub format: u32,
	/// The .NET runtime version the oracle ran on (`"10.0"`).
	#[serde(default)]
	pub dotnet_version: String,
	/// The Roslyn version (`"5.6.0"`).
	#[serde(default)]
	pub roslyn: String,
	/// `"metadata"` or `"source"`.
	#[serde(default)]
	pub mode: String,
	/// The extracted assembly's identity + facade metadata.
	pub assembly: Assembly,
	/// Compilation diagnostics (error-type + total error counts) for tiering.
	#[serde(default)]
	pub diagnostics: Diagnostics,
	/// Every namespace inhabited by an extracted type, with its `<summary>`
	/// when a `namespace-info`-style doc exists (rare in C#).
	#[serde(default)]
	pub namespaces: Vec<Namespace>,
	/// Every included type declaration; nesting is expressed via
	/// [`TypeDecl::enclosing`] (a flat list).
	#[serde(default)]
	pub types: Vec<TypeDecl>,
}

/// The assembly identity and facade metadata.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assembly {
	#[serde(default)]
	pub name: String,
	pub version: Option<String>,
	pub tfm: Option<String>,
	/// Type-forwarded names (facade packages like `System.Memory`).
	#[serde(default)]
	pub forwarded_types: Vec<String>,
	/// `InternalsVisibleTo` targets (metadata only).
	#[serde(default)]
	pub ivt: Vec<String>,
}

/// Compilation diagnostics summary.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
	/// Count of `TypeKind.Error` symbols encountered (missing-ref fidelity).
	#[serde(default)]
	pub error_type_count: u64,
	/// Total compilation error count.
	#[serde(default)]
	pub error_count: u64,
}

/// A namespace, with a doc summary when present.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
	pub name: String,
	pub doc: Option<String>,
}

/// A type declaration: class, struct, interface, enum, delegate, or record.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeDecl {
	/// The Roslyn documentation-comment id (`T:System.String`) — the join key
	/// for cref/doc-link resolution and future occurrence work.
	#[serde(default)]
	pub doc_id: String,
	/// Fully qualified metadata name with arity backticks kept
	/// (`System.Collections.Generic.List\`1`).
	pub qualified_name: String,
	pub simple_name: String,
	/// `CLASS` | `STRUCT` | `INTERFACE` | `ENUM` | `DELEGATE` | `RECORD` |
	/// `RECORD_STRUCT`.
	pub kind: String,
	/// The declaring namespace (empty string for the global namespace).
	#[serde(default)]
	pub namespace: String,
	/// The enclosing type's qualified name / doc-id, for nested declarations.
	pub enclosing: Option<String>,
	#[serde(default)]
	pub modifiers: Vec<String>,
	#[serde(default)]
	pub type_params: Vec<TypeParam>,
	pub base_type: Option<TypeSig>,
	#[serde(default)]
	pub interfaces: Vec<TypeSig>,
	/// The underlying integral type of an `enum`.
	pub enum_underlying: Option<TypeSig>,
	/// The invoke signature of a `delegate`.
	pub delegate_sig: Option<DelegateSig>,
	#[serde(default)]
	pub attributes: Vec<Attr>,
	pub deprecated: Option<Deprecated>,
	#[serde(default)]
	pub hidden: bool,
	#[serde(default)]
	pub forwarded: bool,
	pub doc: Option<String>,
	#[serde(default)]
	pub doc_inherited: bool,
	pub doc_links: Option<BTreeMap<String, String>>,
	/// The receiver type for a C# 14 extension block.
	pub extension_receiver: Option<TypeSig>,
	#[serde(default)]
	pub members: Members,
}

/// A delegate's invoke signature.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateSig {
	#[serde(default)]
	pub params: Vec<Param>,
	#[serde(rename = "return")]
	pub return_type: Option<TypeSig>,
}

/// The members of a type declaration, split by member kind.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Members {
	#[serde(default)]
	pub fields: Vec<Field>,
	#[serde(default)]
	pub properties: Vec<Property>,
	#[serde(default)]
	pub events: Vec<Event>,
	#[serde(default)]
	pub constructors: Vec<Method>,
	#[serde(default)]
	pub methods: Vec<Method>,
	#[serde(default)]
	pub operators: Vec<Method>,
	#[serde(default)]
	pub conversions: Vec<Method>,
	#[serde(default)]
	pub indexers: Vec<Property>,
	/// Directly nested type declarations. The oracle emits **doc-ids**
	/// (`T:Ns.Outer+Inner`) when available, falling back to the metadata
	/// qualified name. Resolve via `Lowering::resolve_type_ref`.
	#[serde(default)]
	pub nested: Vec<String>,
}

/// A declaration-site type parameter (`<T>` / `<in T>` / `<out T>`), with
/// its constraint clause.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParam {
	pub name: String,
	/// `"none"` | `"in"` (contravariant) | `"out"` (covariant).
	#[serde(default)]
	pub variance: String,
	#[serde(default)]
	pub constraints: TypeParamConstraints,
}

/// The constraint clause of a type parameter (`where T : class, IFoo, new()`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParamConstraints {
	/// `class` constraint.
	#[serde(default)]
	pub reference_type: bool,
	/// `struct` constraint.
	#[serde(default)]
	pub value_type: bool,
	/// `notnull` constraint.
	#[serde(default)]
	pub not_null: bool,
	/// `unmanaged` constraint.
	#[serde(default)]
	pub unmanaged: bool,
	/// `new()` constructor constraint.
	#[serde(default)]
	pub constructor: bool,
	/// `allows ref struct` (C# 13).
	#[serde(default)]
	pub allows_ref_like: bool,
	/// Explicit type constraints (`where T : Base`).
	#[serde(default)]
	pub types: Vec<TypeSig>,
}

/// A formal parameter.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
	pub name: String,
	#[serde(rename = "type")]
	pub ty: TypeSig,
	/// `"none"` | `"ref"` | `"out"` | `"in"` | `"refReadonly"`.
	#[serde(default)]
	pub ref_kind: String,
	#[serde(default)]
	pub is_params: bool,
	#[serde(default)]
	pub has_default: bool,
	pub default: Option<String>,
	#[serde(default)]
	pub scoped: bool,
	#[serde(default)]
	pub attributes: Vec<Attr>,
}

/// A field declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
	pub name: String,
	#[serde(default)]
	pub doc_id: String,
	#[serde(rename = "type")]
	pub ty: TypeSig,
	#[serde(default)]
	pub accessibility: String,
	#[serde(default)]
	pub is_const: bool,
	/// The compile-time constant value's display text, for `const` fields /
	/// enum members.
	pub constant: Option<String>,
	#[serde(default)]
	pub is_readonly: bool,
	#[serde(default)]
	pub is_volatile: bool,
	#[serde(default)]
	pub is_required: bool,
	#[serde(default)]
	pub is_static: bool,
	#[serde(default)]
	pub attributes: Vec<Attr>,
	pub deprecated: Option<Deprecated>,
	#[serde(default)]
	pub hidden: bool,
	pub doc: Option<String>,
	#[serde(default)]
	pub doc_inherited: bool,
	pub doc_links: Option<BTreeMap<String, String>>,
}

/// A property or indexer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Property {
	pub name: String,
	#[serde(default)]
	pub doc_id: String,
	#[serde(rename = "type")]
	pub ty: TypeSig,
	#[serde(default)]
	pub accessibility: String,
	/// The getter's own accessibility, when it differs (asymmetric accessors).
	pub get_accessibility: Option<String>,
	/// The setter's own accessibility, when it differs.
	pub set_accessibility: Option<String>,
	/// `"none"` (read-only) | `"set"` | `"init"`.
	#[serde(default)]
	pub set_kind: String,
	#[serde(default)]
	pub is_required: bool,
	#[serde(default)]
	pub is_static: bool,
	#[serde(default)]
	pub is_indexer: bool,
	/// Indexer parameters (empty for ordinary properties).
	#[serde(default)]
	pub parameters: Vec<Param>,
	#[serde(default)]
	pub returns_by_ref: bool,
	#[serde(default)]
	pub returns_by_ref_readonly: bool,
	#[serde(default)]
	pub attributes: Vec<Attr>,
	pub deprecated: Option<Deprecated>,
	#[serde(default)]
	pub hidden: bool,
	pub doc: Option<String>,
	#[serde(default)]
	pub doc_inherited: bool,
	pub doc_links: Option<BTreeMap<String, String>>,
}

/// An event declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
	pub name: String,
	#[serde(default)]
	pub doc_id: String,
	/// The event's delegate type.
	#[serde(rename = "type")]
	pub ty: TypeSig,
	#[serde(default)]
	pub accessibility: String,
	pub add_accessibility: Option<String>,
	pub remove_accessibility: Option<String>,
	#[serde(default)]
	pub is_static: bool,
	#[serde(default)]
	pub attributes: Vec<Attr>,
	pub deprecated: Option<Deprecated>,
	#[serde(default)]
	pub hidden: bool,
	pub doc: Option<String>,
	#[serde(default)]
	pub doc_inherited: bool,
	pub doc_links: Option<BTreeMap<String, String>>,
}

/// A method, constructor, operator, or conversion.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
	/// The metadata name (`.ctor`, `op_Addition`, `get_Item`, `M`).
	pub name: String,
	#[serde(default)]
	pub doc_id: String,
	/// Roslyn `MethodKind` (`Ordinary`, `Constructor`, `UserDefinedOperator`,
	/// `Conversion`, `ExplicitInterfaceImplementation`, …).
	#[serde(default)]
	pub method_kind: String,
	#[serde(default)]
	pub accessibility: String,
	#[serde(default)]
	pub is_static: bool,
	#[serde(default)]
	pub is_abstract: bool,
	#[serde(default)]
	pub is_virtual: bool,
	#[serde(default)]
	pub is_override: bool,
	#[serde(default)]
	pub is_sealed: bool,
	#[serde(default)]
	pub is_extern: bool,
	#[serde(default)]
	pub is_async: bool,
	#[serde(default)]
	pub is_iterator: bool,
	#[serde(default)]
	pub is_extension_method: bool,
	#[serde(default)]
	pub is_readonly: bool,
	#[serde(default)]
	pub type_params: Vec<TypeParam>,
	#[serde(default)]
	pub parameters: Vec<Param>,
	/// `None` for constructors and `void` returns is a `named{name:"System.Void"}`.
	pub return_type: Option<TypeSig>,
	#[serde(default)]
	pub returns_by_ref: bool,
	#[serde(default)]
	pub returns_by_ref_readonly: bool,
	/// The explicitly-implemented interface member (`IFoo.Bar`), if any.
	pub explicit_interface: Option<String>,
	/// `"none"` | `"implicit"` | `"explicit"` | `"checked"` (conversions/ops).
	#[serde(default)]
	pub operator_kind: String,
	#[serde(default)]
	pub attributes: Vec<Attr>,
	pub deprecated: Option<Deprecated>,
	#[serde(default)]
	pub hidden: bool,
	pub doc: Option<String>,
	#[serde(default)]
	pub doc_inherited: bool,
	pub doc_links: Option<BTreeMap<String, String>>,
}

/// An attribute use (`[Foo(1, Name = "x")]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attr {
	/// The attribute class's metadata name (`System.ObsoleteAttribute`).
	#[serde(rename = "type")]
	pub ty: String,
	/// Positional argument display strings.
	#[serde(default)]
	pub args: Vec<String>,
	/// Named-argument display strings, keyed by property name.
	#[serde(default)]
	pub named: BTreeMap<String, String>,
}

/// Deprecation (`[Obsolete]`) marker.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deprecated {
	pub message: Option<String>,
	#[serde(default)]
	pub is_error: bool,
}

/// A recursive structural type use (mirrors CSHARP-PLAN §2.4). Internally
/// tagged by `"kind"`; every reference-type node carries 3-state
/// [`nullable`](TypeSig) annotation.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TypeSig {
	/// A named class / struct / interface / enum / delegate use.
	Named {
		/// Metadata FQN, arity backticks kept.
		name: String,
		#[serde(default)]
		args: Vec<TypeSig>,
		/// The generic owner for `Outer<T>.Inner` uses.
		owner: Option<Box<TypeSig>>,
		/// `"none"` (oblivious) | `"annotated"` (`T?`) | `"notAnnotated"`.
		#[serde(default)]
		nullable: String,
		/// Roslyn `TypeKind` (`Class`/`Struct`/`Interface`/`Enum`/`Delegate`/…).
		#[serde(default)]
		type_kind: String,
	},
	/// A generic type-parameter reference.
	TypeParam {
		name: String,
		/// `"type"` | `"method"`.
		#[serde(default)]
		owner_kind: String,
		#[serde(default)]
		nullable: String,
	},
	/// An array (`T[]`, `T[,]`, jagged = nested).
	Array {
		element: Box<TypeSig>,
		/// The array rank (1 = SZ vector; >1 = multidimensional).
		#[serde(default = "one")]
		rank: u32,
		#[serde(default)]
		nullable: String,
	},
	/// An unmanaged pointer (`T*`).
	Pointer { pointee: Box<TypeSig> },
	/// A function pointer (`delegate*<...>`).
	FuncPtr {
		#[serde(default)]
		params: Vec<TypeSig>,
		#[serde(rename = "return")]
		return_type: Option<Box<TypeSig>>,
		#[serde(default)]
		call_conv: String,
		#[serde(default)]
		unmanaged_call_convs: Vec<String>,
	},
	/// A value tuple (`(int, string)` / `(int x, string y)`).
	Tuple {
		#[serde(default)]
		elements: Vec<TupleElement>,
		#[serde(default)]
		nullable: String,
	},
	/// `dynamic`.
	Dynamic {},
	/// `Nullable<T>` — a nullable *value* type.
	NullableValue { inner: Box<TypeSig> },
	/// An unresolvable type; `name` is the source/metadata text.
	Error { name: String },
}

/// One element of a value tuple.
#[derive(Debug, Clone, Deserialize)]
pub struct TupleElement {
	pub name: Option<String>,
	#[serde(rename = "type")]
	pub ty: TypeSig,
}

fn one() -> u32 {
	1
}

impl TypeSig {
	/// The metadata name for named/error uses, when meaningful.
	pub fn named_name(&self) -> Option<&str> {
		match self {
			TypeSig::Named { name, .. } => Some(name),
			TypeSig::Error { name } => Some(name),
			_ => None,
		}
	}
}

/// The 3-state nullability of a reference-type node (never collapse oblivious
/// and non-null — pitfall #6/#10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nullability {
	/// No annotation context (oblivious).
	Oblivious,
	/// `T?` — annotated nullable.
	Annotated,
	/// `T` under an enabled nullable context — non-null.
	NotAnnotated,
}

impl Nullability {
	/// Interpret the oracle's `nullable` string.
	pub fn parse(s: &str) -> Self {
		match s {
			"annotated" => Nullability::Annotated,
			"notAnnotated" => Nullability::NotAnnotated,
			_ => Nullability::Oblivious,
		}
	}
}
