//! Serde mirror of the Go oracle's JSON schema.
//!
//! These structs deserialize — field for field — the document emitted by
//! the vendored extractor at `oracle/` (see `oracle/serialize.go` for the
//! authoritative Go-side definitions). The oracle uses `omitempty`
//! aggressively, so every optional field here carries `#[serde(default)]`.
//!
//! The one structural invariant inherited from the oracle: named types
//! and aliases appear as *references* (`kind: "named"/"alias"` with
//! `pkg` + `name`), never expanded inline. Their definitions live in the
//! owning package's `decls` list. Anonymous composites (structs,
//! interfaces, funcs, maps, …) are expanded structurally. Go cannot form
//! a cyclic type without a named intermediary, so the [`Type`] tree is
//! always finite.

use std::collections::HashMap;

use serde::Deserialize;

/// The root of the oracle's JSON document.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
	/// go.mod-derived metadata for the loaded module.
	#[serde(default)]
	pub module: Option<Module>,

	/// One entry per package, sorted by import path.
	#[serde(default)]
	pub packages: Vec<Package>,

	/// Package-load diagnostics. The oracle proceeds best-effort, so a
	/// non-empty list does not invalidate `packages`.
	#[serde(default)]
	pub errors: Vec<String>,
}

/// go.mod-derived module metadata.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
	/// The module path (the `module` directive).
	#[serde(default)]
	pub path: String,

	/// The on-disk module root.
	#[serde(default)]
	pub dir: String,

	/// The `go` directive (e.g. `"1.23"`).
	#[serde(default)]
	pub go_version: String,

	/// Resolved module version — empty for a working tree.
	#[serde(default)]
	pub version: String,
}

/// One Go package with all of its top-level declarations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package {
	/// Fully-qualified import path.
	pub import_path: String,

	/// The package identifier (the `package` clause).
	#[serde(default)]
	pub name: String,

	/// Package doc comment (markers stripped, directives removed).
	#[serde(default)]
	pub doc: String,

	/// The package's Go source files.
	#[serde(default)]
	pub files: Vec<String>,

	/// Every package-level declaration — exported AND unexported.
	#[serde(default)]
	pub decls: Vec<Decl>,
}

/// The discriminator for [`Decl`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeclKind {
	/// A defined type (`type T ...`).
	Type,
	/// A true alias (`type T = ...`).
	Alias,
	/// A package-level function.
	Func,
	/// A constant.
	Const,
	/// A package-level variable.
	Var,
}

/// One package-level declaration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decl {
	/// What this declaration is.
	pub kind: DeclKind,

	/// The declared identifier.
	pub name: String,

	/// Whether the identifier is exported.
	#[serde(default)]
	pub exported: bool,

	/// The declaration's doc comment, cleaned by the oracle.
	#[serde(default)]
	pub doc: String,

	/// Declaring source position.
	#[serde(default)]
	pub pos: Option<Pos>,

	/// Generic type parameters with constraints (`kind` type/func).
	#[serde(default)]
	pub type_params: Vec<TypeParamDecl>,

	/// The structural underlying type (`kind` type).
	#[serde(default)]
	pub underlying: Option<Type>,

	/// Methods declared directly on this named type.
	#[serde(default)]
	pub methods: Vec<Method>,

	/// Methods promoted through embedded fields, with their origin.
	#[serde(default)]
	pub promoted_methods: Vec<Method>,

	/// Struct field name → doc comment (`kind` type, struct underlying).
	#[serde(default)]
	pub field_docs: HashMap<String, String>,

	/// Interface method name → doc comment (`kind` type, interface
	/// underlying).
	#[serde(default)]
	pub method_docs: HashMap<String, String>,

	/// The aliased type (`kind` alias).
	#[serde(default)]
	pub target: Option<Type>,

	/// The function type (`kind` func).
	#[serde(default)]
	pub signature: Option<Type>,

	/// The declared/inferred type (`kind` const/var).
	#[serde(default)]
	pub r#type: Option<Type>,

	/// The exact constant value (`constant.Value.ExactString`).
	#[serde(default)]
	pub value: String,

	/// The const declaration block this constant belongs to
	/// (unique within its package).
	#[serde(default)]
	pub const_group: i64,

	/// Whether that const block uses `iota`.
	#[serde(default)]
	pub group_has_iota: bool,

	/// In-package interfaces this named type satisfies (method-set
	/// inclusion via `types.Implements`). Only populated for `kind`
	/// type declarations that are not themselves interfaces.
	#[serde(default)]
	pub implements: Vec<Type>,
}

/// A method attached to a named type (declared or promoted).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Method {
	/// Method identifier.
	pub name: String,

	/// Whether the method is exported.
	#[serde(default)]
	pub exported: bool,

	/// The method's doc comment.
	#[serde(default)]
	pub doc: String,

	/// Declaring position.
	#[serde(default)]
	pub pos: Option<Pos>,

	/// Receiver binding name (`s` in `(s *Server)`).
	#[serde(default)]
	pub recv_name: String,

	/// `true` for `func (t *T)`, `false` for `func (t T)`.
	#[serde(default)]
	pub pointer_recv: bool,

	/// Receiver type-parameter names (`T` in `func (l *List[T])`).
	#[serde(default)]
	pub recv_type_params: Vec<String>,

	/// The method's function type (receiver excluded).
	#[serde(default)]
	pub signature: Option<Type>,

	/// For promoted methods: the qualified embedded type the method was
	/// promoted from (e.g. `"sync.Mutex"`).
	#[serde(default)]
	pub origin: String,
}

/// One generic type parameter and its constraint.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParamDecl {
	/// The type-parameter identifier (e.g. `T`).
	pub name: String,

	/// The constraint interface (possibly named, possibly an inline
	/// union / method set).
	#[serde(default)]
	pub constraint: Option<Type>,
}

/// A `file:line:column` source position.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pos {
	#[serde(default)]
	pub file: String,
	#[serde(default)]
	pub line: i64,
	#[serde(default)]
	pub col:  i64,
}

/// The discriminator for [`Type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeKind {
	Basic,
	Named,
	Alias,
	TypeParam,
	Pointer,
	Slice,
	Array,
	Map,
	Chan,
	Func,
	Struct,
	Interface,
	Union,
	Tuple,
	#[default]
	Invalid,
}

/// The recursive structural type tree, discriminated by [`TypeKind`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Type {
	/// The node discriminator.
	pub kind: TypeKind,

	/// Basic-type name (`int`, `rune`, `byte`, …), named/alias type
	/// name, or type-parameter name.
	#[serde(default)]
	pub name: String,

	/// Defining package's import path for named/alias types (empty for
	/// universe types such as `error` and `comparable`).
	#[serde(default)]
	pub pkg: String,

	/// Instantiation arguments of a generic named type.
	#[serde(default)]
	pub type_args: Vec<Type>,

	/// Element type of a pointer, slice, array, or chan.
	#[serde(default)]
	pub elem: Option<Box<Type>>,

	/// Fixed length of an array.
	#[serde(default)]
	pub len: i64,

	/// A map's key type.
	#[serde(default)]
	pub key: Option<Box<Type>>,

	/// A map's value type.
	#[serde(default)]
	pub value: Option<Box<Type>>,

	/// Channel direction: `"send"` (`chan<-`), `"recv"` (`<-chan`), or
	/// `"both"`.
	#[serde(default)]
	pub dir: String,

	/// Func parameters. A variadic func's final param arrives as a
	/// slice (`...T` is `[]T` in go/types).
	#[serde(default)]
	pub params: Vec<Param>,

	/// Func results — Go's multiple returns, names preserved.
	#[serde(default)]
	pub results: Vec<Param>,

	/// Whether the func's final parameter is variadic.
	#[serde(default)]
	pub variadic: bool,

	/// A struct's fields, in declaration order.
	#[serde(default)]
	pub fields: Vec<StructField>,

	/// An interface's directly-declared methods.
	#[serde(default)]
	pub explicit_methods: Vec<MethodSig>,

	/// An interface's embedded types (interfaces; in constraint
	/// position, unions and concrete terms).
	#[serde(default)]
	pub embeddeds: Vec<Type>,

	/// The COMPLETE interface method set after embedding expansion.
	#[serde(default)]
	pub all_methods: Vec<MethodSig>,

	/// Whether the interface requires comparability.
	#[serde(default)]
	pub is_comparable: bool,

	/// A constraint union's terms (`~int | string`).
	#[serde(default)]
	pub terms: Vec<Term>,

	/// A tuple's component types (rare at the surface).
	#[serde(default)]
	pub types: Vec<Type>,
}

/// A func parameter or result.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
	/// Binding name; empty for unnamed params/results.
	#[serde(default)]
	pub name: String,

	/// The param/result type.
	#[serde(default)]
	pub r#type: Option<Type>,
}

/// One struct field.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructField {
	/// Field name (the implicit name for embedded fields).
	pub name: String,

	/// The field type.
	#[serde(default)]
	pub r#type: Option<Type>,

	/// The raw struct tag, if any.
	#[serde(default)]
	pub tag: String,

	/// Whether the field is anonymous/embedded.
	#[serde(default)]
	pub embedded: bool,

	/// Whether the field name is exported.
	#[serde(default)]
	pub exported: bool,
}

/// An interface method: name + signature + provenance.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodSig {
	/// Method name.
	pub name: String,

	/// Whether the method name is exported.
	#[serde(default)]
	pub exported: bool,

	/// The method's func type.
	#[serde(default)]
	pub signature: Option<Type>,

	/// Declaring position (points into the embedding source for
	/// inherited methods).
	#[serde(default)]
	pub pos: Option<Pos>,

	/// Import path of the package that declared the method.
	#[serde(default)]
	pub pkg: String,
}

/// One term of a constraint union.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Term {
	/// `~T` approximation terms match any type whose underlying type
	/// is `T`.
	#[serde(default)]
	pub tilde: bool,

	/// The term's type.
	#[serde(default)]
	pub r#type: Option<Type>,
}

impl Type {
	/// Whether this is the empty interface (`interface{}` / `any`'s
	/// shape): no methods, no embedded types.
	pub fn is_empty_interface(&self) -> bool {
		self.kind == TypeKind::Interface
			&& self.explicit_methods.is_empty()
			&& self.embeddeds.is_empty()
			&& self.all_methods.is_empty()
	}
}
