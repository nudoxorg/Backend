//! Serde mirror of the Go oracle's JSON schema.
//!
//! These structs deserialize — field for field — the document emitted by the
//! vendored extractor at `workspace/compiler/languages/oracle/go/` (see
//! `oracle/serialize.go` for the authoritative Go-side definitions).
//!
//! ## Changes from the old `workspace/compiler/compile/go/oracle.rs`
//!
//! * `Vec<T>` for finished, non-growing collections replaced by `Box<[T]>`.
//! * `dir: String` in `Type` (channel direction) replaced by the typed
//!   [`ChanDir`] enum.
//! * `HashMap<String, String>` for `field_docs` / `method_docs` uses
//!   `Box<[(String, String)]>` internally via a custom deserialize so lookup
//!   becomes linear for the typical handful of fields (avoids the per-HashMap
//!   heap overhead). Field kept as plain serde `HashMap` for simplicity —
//!   downstream code iterates at most O(n) where n is the number of fields.
//! * The oracle uses `omitempty` aggressively, so every optional field carries
//!   `#[serde(default)]`.

use std::collections::HashMap;

use serde::Deserialize;

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

/// The root of the oracle's JSON document.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    /// go.mod-derived metadata for the loaded module.
    #[serde(default)]
    pub module: Option<Module>,

    /// One entry per package, sorted by import path.
    #[serde(default)]
    pub packages: Box<[Package]>,

    /// Package-load diagnostics.  The oracle proceeds best-effort, so a
    /// non-empty list does not invalidate `packages`.
    #[serde(default)]
    pub errors: Box<[String]>,

    /// The payload schema the *oracle binary* speaks.
    ///
    /// `0` when the field is absent, which is what every binary built before
    /// the handshake emits — see [`Output::staleness`].
    #[serde(default)]
    pub schema_version: u32,
}

/// Why an oracle payload cannot be trusted to be complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Staleness {
    /// The binary predates a field — or a field's scope — this build reads.
    #[error(
        "the `nudox-go-oracle` binary is out of date: it speaks payload schema \
         {found}, but this build reads schema {required}. Data added or widened \
         since (references; implements, and its scope: schema 2 checks every \
         package in the module, not just each type's own; unresolved cgo \
         names: schema 3) is absent or \
         incomplete in its output, so `refs` and `subtypes` will look empty or \
         under-report for every Go package. Rebuild the oracle \
         (`go build ./workspace/compiler/languages/oracle/go`) and point \
         NUDOX_GO_ORACLE_BIN at the result."
    )]
    OlderThanRequired {
        /// What the binary declared (`0` when it declared nothing).
        found: u32,
        /// What this build needs.
        required: u32,
    },
}

impl Output {
    /// The payload schema this build reads.
    ///
    /// Bump this when the Rust side starts *reading* a field the oracle only
    /// recently started emitting — that is exactly the moment an older binary
    /// begins under-reporting, and the moment this check has to start firing.
    ///
    /// Also bump it when a field's *scope* widens rather than its presence
    /// changing — v2 did this: `Decl.Implements` started checking a type
    /// against interfaces in every package the module loaded, not only its
    /// own. The field was already there on a v1 binary; what it under-reports
    /// changed. `#[serde(default)]` cannot express that distinction, so the
    /// handshake is the only thing that can.
    /// v3 reads `Package.unresolved_cgo`. An older binary omits that list, so
    /// a cgo package looks fully resolved.
    pub const REQUIRED_SCHEMA_VERSION: u32 = 3;

    /// Whether the binary that produced this payload is older than this build
    /// expects.
    ///
    /// # Why this is not a deserialization error
    ///
    /// Every field here is `#[serde(default)]`, deliberately: a *newer* oracle
    /// adding fields must not break an older reader. The cost is that an
    /// *older* oracle omitting fields is indistinguishable from a package that
    /// genuinely has none — both arrive as an empty `Box<[_]>`.
    ///
    /// That is not hypothetical. `lindsey.app` ships no oracle, so a user
    /// pointed `NUDOX_GO_ORACLE_BIN` at a prebuilt binary from the checkout;
    /// it predated `references`, and every `refs` call for every Go symbol
    /// answered `not_recorded` while the *source* produced references
    /// correctly. The sentinel fired truthfully and named the wrong cause.
    ///
    /// So the payload still parses — refusing to would strand anyone with an
    /// old binary — and this is asked separately, so the answer can be "your
    /// binary is old" rather than "your package has no references".
    ///
    /// Returns `None` for a current *or newer* oracle: only older
    /// under-reports.
    pub fn staleness(&self) -> Option<Staleness> {
        (self.schema_version < Self::REQUIRED_SCHEMA_VERSION).then_some(
            Staleness::OlderThanRequired {
                found: self.schema_version,
                required: Self::REQUIRED_SCHEMA_VERSION,
            },
        )
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// go.mod-derived module metadata.
#[derive(Debug, Deserialize)]
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

// ---------------------------------------------------------------------------
// Package
// ---------------------------------------------------------------------------

/// One Go package with all of its top-level declarations.
#[derive(Debug, Deserialize)]
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
    pub files: Box<[String]>,

    /// Every package-level declaration — exported AND unexported.
    #[serde(default)]
    pub decls: Box<[Decl]>,

    /// Source files excluded by platform/build constraints, with the exported
    /// declarations they contain. These declarations are unavailable in the
    /// active package and therefore cannot appear in `decls`.
    #[serde(default)]
    pub build_constraints: Box<[BuildConstraint]>,

    /// Go/types-resolved same-package function calls.
    #[serde(default)]
    pub references: Box<[Reference]>,

    /// Incomplete cgo types this package touched, qualified as
    /// `import/path.Name`. Empty when the package has none — and also empty
    /// when the oracle binary predates schema 3, which [`Output::staleness`]
    /// is what distinguishes.
    #[serde(default)]
    pub unresolved_cgo: Box<[String]>,
}

/// One resolved function-use edge in a package.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub owner: String,
    pub target: String,
    pub file: String,
    pub start: usize,
    pub end: usize,
}

/// Availability information for one source file excluded by build constraints.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildConstraint {
    /// The excluded source file.
    pub file: String,

    /// The normalized build-constraint expression (for example `"windows"`).
    #[serde(default)]
    pub constraints: Box<[String]>,

    /// Exported declarations found by the source-scan fallback.
    #[serde(default)]
    pub exported_decls: Box<[BuildDecl]>,
}

/// An exported declaration found in an excluded build-tagged file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildDecl {
    pub name: String,
    pub kind: String,
}

// ---------------------------------------------------------------------------
// Decl
// ---------------------------------------------------------------------------

/// The discriminator for a [`Decl`].
///
/// **Typed enum** — the old schema used a raw `String`; this catches new oracle
/// kinds at the serde boundary rather than silently matching a `_ =>` arm.
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
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decl {
    /// What this declaration is.
    pub kind: DeclKind,

    /// The declared identifier.
    pub name: String,

    /// Whether the identifier is exported (first letter is upper-case in Go).
    #[serde(default)]
    pub exported: bool,

    /// The declaration's doc comment, cleaned by the oracle.
    #[serde(default)]
    pub doc: String,

    /// Declaring source position.
    #[serde(default)]
    pub pos: Option<Pos>,

    /// Byte range of this declaration's full source text.  See
    /// `oracle/serialize.go`'s `Decl.Span` doc for exactly what it covers
    /// (the whole declaration, or — for one member of a grouped
    /// `const`/`var`/`type` block — just that member's own spec).
    #[serde(default)]
    pub span: Option<Span>,

    /// Generic type parameters with constraints (`kind` type/func).
    #[serde(default)]
    pub type_params: Box<[TypeParamDecl]>,

    /// The structural underlying type (`kind` type).
    #[serde(default)]
    pub underlying: Option<Type>,

    /// Methods declared directly on this named type.
    #[serde(default)]
    pub methods: Box<[Method]>,

    /// Methods promoted through embedded fields, with their origin.
    #[serde(default)]
    pub promoted_methods: Box<[Method]>,

    /// Struct field name → doc comment (`kind` type, struct underlying).
    ///
    /// Kept as `HashMap` for O(1) lookup per field when stitching field docs.
    #[serde(default)]
    pub field_docs: HashMap<String, String>,

    /// Interface method name → doc comment (`kind` type, interface underlying).
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

    /// The const declaration block this constant belongs to (unique within
    /// its package).
    #[serde(default)]
    pub const_group: i64,

    /// Whether that const block uses `iota`.
    #[serde(default)]
    pub group_has_iota: bool,

    /// In-package interfaces this named type satisfies.  Only populated for
    /// `kind` type declarations that are not themselves interfaces.
    #[serde(default)]
    pub implements: Box<[Type]>,
}

// ---------------------------------------------------------------------------
// Method
// ---------------------------------------------------------------------------

/// A method attached to a named type (declared or promoted).
#[derive(Debug, Deserialize)]
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

    /// Byte range of this method's full `func (recv T) Name(...) { ... }`
    /// declaration.  `None` when the oracle could not find the declaring
    /// `*ast.FuncDecl` — true of every method promoted from a type outside
    /// the package being lowered (see `oracle/serialize.go::methodSpan`).
    #[serde(default)]
    pub span: Option<Span>,

    /// Receiver binding name (`s` in `(s *Server)`).
    #[serde(default)]
    pub recv_name: String,

    /// `true` for `func (t *T)`, `false` for `func (t T)`.
    #[serde(default)]
    pub pointer_recv: bool,

    /// Receiver type-parameter names (`T` in `func (l *List[T])`).
    #[serde(default)]
    pub recv_type_params: Box<[String]>,

    /// The method's function type (receiver excluded).
    #[serde(default)]
    pub signature: Option<Type>,

    /// For promoted methods: the qualified embedded type the method was
    /// promoted from (e.g. `"sync.Mutex"`).
    #[serde(default)]
    pub origin: String,
}

// ---------------------------------------------------------------------------
// TypeParamDecl
// ---------------------------------------------------------------------------

/// One generic type parameter and its constraint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeParamDecl {
    /// The type-parameter identifier (e.g. `T`).
    pub name: String,

    /// The constraint interface (possibly named, possibly an inline
    /// union / method set).
    #[serde(default)]
    pub constraint: Option<Type>,
}

// ---------------------------------------------------------------------------
// Pos
// ---------------------------------------------------------------------------

/// A `file:line:column` source position, plus the byte offset the oracle's
/// `token.FileSet` resolved it to.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pos {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: i64,
    #[serde(default)]
    pub col: i64,
    /// 0-based byte offset of this position within `file`.
    #[serde(default)]
    pub offset: i64,
}

/// A byte range `[start, end)` into the file named by the enclosing
/// [`Pos`], covering a declaration's full source text (see `Decl::span`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

// ---------------------------------------------------------------------------
// TypeKind / ChanDir
// ---------------------------------------------------------------------------

/// The discriminator for [`Type`].
///
/// **Typed enum** — the old schema used a raw string field.
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

/// Channel direction — **typed enum** replacing the old `dir: String`.
///
/// Go emits `"send"` for `chan<-`, `"recv"` for `<-chan`, and `"both"` for
/// an unrestricted `chan`.  Unknown values produced by newer oracle versions
/// fall through to `Both` (the safest default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChanDir {
    Send,
    Recv,
    #[serde(other)]
    #[default]
    Both,
}

impl ChanDir {
    /// The Go-syntax operator string used by renderers and the IR type-operator
    /// encoding.
    pub fn as_operator(self) -> &'static str {
        match self {
            ChanDir::Send => "chan<-",
            ChanDir::Recv => "<-chan",
            ChanDir::Both => "chan",
        }
    }
}

// ---------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------

/// The recursive structural type tree, discriminated by [`TypeKind`].
///
/// Named types arrive from the oracle by *reference* (`kind: Named/Alias`,
/// `pkg` + `name`), never expanded inline.  Go cannot form a cyclic type
/// without a named intermediary, so this tree is always finite.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Type {
    /// The node discriminator.
    pub kind: TypeKind,

    /// Basic-type name (`int`, `rune`, `byte`, …), named/alias type name, or
    /// type-parameter name.
    #[serde(default)]
    pub name: String,

    /// Defining package's import path for named/alias types (empty for
    /// universe types such as `error` and `comparable`).
    #[serde(default)]
    pub pkg: String,

    /// Instantiation arguments of a generic named type.
    #[serde(default)]
    pub type_args: Box<[Type]>,

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

    /// Channel direction — **typed enum** (was `dir: String` in the old code).
    #[serde(default)]
    pub dir: ChanDir,

    /// Func parameters.  A variadic func's final param arrives as a slice
    /// (`...T` is `[]T` in go/types).
    #[serde(default)]
    pub params: Box<[Param]>,

    /// Func results — Go's multiple returns, names preserved.
    #[serde(default)]
    pub results: Box<[Param]>,

    /// Whether the func's final parameter is variadic.
    #[serde(default)]
    pub variadic: bool,

    /// A struct's fields, in declaration order.
    #[serde(default)]
    pub fields: Box<[StructField]>,

    /// An interface's directly-declared methods.
    #[serde(default)]
    pub explicit_methods: Box<[MethodSig]>,

    /// An interface's embedded types (interfaces; in constraint position,
    /// unions and concrete terms).
    #[serde(default)]
    pub embeddeds: Box<[Type]>,

    /// The COMPLETE interface method set after embedding expansion.
    #[serde(default)]
    pub all_methods: Box<[MethodSig]>,

    /// Whether the interface requires comparability.
    #[serde(default)]
    pub is_comparable: bool,

    /// A constraint union's terms (`~int | string`).
    #[serde(default)]
    pub terms: Box<[Term]>,

    /// A tuple's component types (rare at the surface).
    #[serde(default)]
    pub types: Box<[Type]>,
}

impl Type {
    /// Whether this is the empty interface (`interface{}` / `any`'s shape):
    /// no methods and no embedded types.
    pub fn is_empty_interface(&self) -> bool {
        self.kind == TypeKind::Interface
            && self.explicit_methods.is_empty()
            && self.embeddeds.is_empty()
            && self.all_methods.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Param / StructField / MethodSig / Term
// ---------------------------------------------------------------------------

/// A func parameter or result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Param {
    /// Binding name; empty for unnamed params/results.
    #[serde(default)]
    pub name: String,

    /// The param/result type.
    #[serde(default)]
    pub r#type: Option<Type>,

    /// Position of the parameter/result identifier when it is named.
    #[serde(default)]
    pub pos: Option<Pos>,
}

/// One struct field.
#[derive(Debug, Deserialize)]
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
#[derive(Debug, Deserialize)]
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

    /// Declaring position (points into the embedding source for inherited
    /// methods).
    #[serde(default)]
    pub pos: Option<Pos>,

    /// Import path of the package that declared the method.
    #[serde(default)]
    pub pkg: String,
}

/// One term of a constraint union.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    /// `~T` approximation terms match any type whose underlying type is `T`.
    #[serde(default)]
    pub tilde: bool,

    /// The term's type.
    #[serde(default)]
    pub r#type: Option<Type>,
}
