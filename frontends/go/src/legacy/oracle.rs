//! Serde mirror of the Go oracle's JSON schema.
//!
//! These structs deserialize — field for field — the document emitted by the
//! vendored extractor at `compiler/languages/go/oracle/` (see
//! `oracle/serialize.go` for the authoritative Go-side definitions).
//!
//! ## Changes from the old `workspace/compiler/compile/go/oracle.rs`
//!
//! * `Vec<T>` for finished, non-growing collections replaced by `Box<[T]>`.
//! * `dir: String` in `Type` (channel direction) replaced by the typed
//!   [`ChanDir`] enum.
//! * `HashMap<String, String>` is retained for `field_docs` / `method_docs`;
//!   these fields are sparse and callers iterate at most O(n), where n is the
//!   number of fields. No custom map representation is involved.
//! * The oracle uses `omitempty` aggressively, so every optional field carries
//!   `#[serde(default)]`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use backend_platform::{DirectoryCapability, EntryKind};

use backend_semantic::vocabulary::{NativeWorker, NativeWorkerPanic};
use serde::Deserialize;
use sha2::{Digest, Sha256};

mod authority_witness;
mod dependency_witness;
pub use self::dependency_witness::GoDependencyClosureFailure;
pub use self::authority_witness::{
    GoFilesystemTargetKind, GoLocalOnlyReason, GoPackageAuthorityWitness,
    GoPackageAuthorityWitnessError, GoWorkFileWitness, GoWorkWitness, GoWorkWitnessError,
};

const DIAGNOSTIC_PREFIX_LIMIT: usize = 4096;
const DIAGNOSTIC_TAIL_LIMIT: usize = 4096;
const UNSUPPORTED_CGO_SENTINEL: &str = "NUDOX_GO_UNSUPPORTED_CGO_CLOSURE";
const MAX_HELPER_CACHE_ROOT_ENTRIES: usize = 256;
const MAX_HELPER_CACHE_ENTRY_ENTRIES: usize = 8;
const MAX_HELPER_MANIFEST_BYTES: u64 = 512;
const MAX_HELPER_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GO_TOOLCHAIN_ENTRIES: usize = 50_000;
const MAX_GO_TOOLCHAIN_DEPTH: usize = 64;
const MAX_GO_TOOLCHAIN_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GO_TOOLCHAIN_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_GO_TOOLCHAIN_PATH_BYTES: usize = 4096;

/// Versioned identity of the explicit Go package-authority child environment.
pub const GO_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1: &str = "go-package-child-environment.v1";

include!(concat!(env!("OUT_DIR"), "/go_oracle_sources.rs"));

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

/// The root of the oracle's JSON document.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
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
    /// the handshake emits and is rejected by [`GoOracle::decode`].
    #[serde(default)]
    pub schema_version: u32,
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
    ///
    /// v3 reads `Package.unresolved_cgo` and widens `references` to method
    /// bodies and cross-package calls. An older binary omits the cgo list
    /// (so a cgo package looks fully resolved) and under-reports the call
    /// graph.
    ///
    /// v4 widens `references` from the free-function call graph to the full
    /// go/types Uses map: every use of a keyable named object with a closed
    /// `kind` (call/read/typeref/import), a closed object `class`, the
    /// receiver type name for method and field targets, and the exact
    /// NAME-TOKEN extent. The addition is byte-compatible — every pre-v4
    /// call row carries an empty `kind`/`class`/`recv` and serializes
    /// exactly as before — but a pre-v4 binary under-reports so massively
    /// (zero type uses, method uses, field uses, imports) that the
    /// handshake must fire. `Decl.NameSpan` also arrives with v4.
    pub const REQUIRED_SCHEMA_VERSION: u32 = 5;
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// go.mod-derived module metadata.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

    /// Files excluded because CGO_ENABLED=0 removes Go source importing C.
    #[serde(default)]
    pub cgo_excluded_files: Box<[String]>,

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

/// One resolved named-object use edge in a package — the go/types Uses
/// table rendered as rows. The v3 schema carried only free-function calls;
/// v4 covers every use of a keyable object.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// The enclosing declaration's bare name: the calling function or
    /// method for body uses, or the package-level declaration whose spec
    /// contains the use.
    pub owner: String,

    /// The bare receiver type name when `owner` is a method (empty for a
    /// package-level function).
    #[serde(default)]
    pub owner_recv: String,

    /// The used object's bare name.
    pub target: String,

    /// The target's defining package's import path, present only when it
    /// differs from the package this `Reference` was extracted from (empty
    /// for a same-package target; the imported package's path for an
    /// import use). See `oracle/serialize.go`'s doc comment on the Go-side
    /// field for why routing (same-module `Intro`-eligible vs. genuinely
    /// foreign) is decided on this side, not the oracle's.
    #[serde(default)]
    pub target_pkg: String,

    /// The used object's closed class. Empty for a free function (every v3
    /// row); `method`, `field`, `var`, `const`, `type`, or `pkg` from v4
    /// on.
    #[serde(default)]
    pub class: String,

    /// The use's closed kind. Empty for a call (every v3 row); `read`
    /// (value use, writes included — the Uses table records no lvalue
    /// distinction), `typeref`, or `import` from v4 on.
    #[serde(default)]
    pub kind: String,

    /// The receiver type's bare name for method and field targets
    /// (`Server` for `x.Close` on a `*Server`), empty otherwise.
    #[serde(default)]
    pub recv: String,

    pub file: String,
    /// The used identifier token's exact byte extent — the NAME-TOKEN
    /// extent.
    pub start: usize,
    pub end: usize,
}

/// Availability information for one source file excluded by build constraints.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct BuildConstraint {
    /// The excluded source file.
    pub file: String,

    /// The normalized build-constraint expression (for example `"windows"`).
    #[serde(default)]
    pub constraints: Box<[String]>,

    /// Compiler selection reason that is not a source build-tag expression.
    #[serde(default)]
    pub excluded_reason: String,

    /// Exported declarations found by the source-scan fallback.
    #[serde(default)]
    pub exported_decls: Box<[BuildDecl]>,
}

/// An exported declaration found in an excluded build-tagged file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

    /// Byte range of the declaration's own identifier token (v4).  The
    /// authority image carries it beside `span` so owner-relative
    /// occurrence spans can name the declared entity itself.
    #[serde(default)]
    pub name_span: Option<Span>,

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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Term {
    /// `~T` approximation terms match any type whose underlying type is `T`.
    #[serde(default)]
    pub tilde: bool,

    /// The term's type.
    #[serde(default)]
    pub r#type: Option<Type>,
}

struct EmbeddedGoOracleSource(tempfile::TempDir);

impl EmbeddedGoOracleSource {
    fn materialize() -> Result<Self, OracleError> {
        let directory = tempfile::Builder::new()
            .prefix("nudox-go-oracle-")
            .tempdir()
            .map_err(OracleError::GoOracleSourceDirectory)?;
        for (name, contents) in GO_ORACLE_SOURCE_FILES {
            let path = directory.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(OracleError::GoOracleSourceDirectory)?;
            }
            std::fs::write(&path, contents)
                .map_err(|source| OracleError::GoOracleSourceFile { path, source })?;
        }
        Ok(Self(directory))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

struct GoOracleCommand {
    command: std::process::Command,
    _source: Option<EmbeddedGoOracleSource>,
}

impl GoOracleCommand {
    fn binary(command: std::process::Command) -> Self {
        Self {
            command,
            _source: None,
        }
    }

    fn toolchain(
        executable: impl AsRef<std::ffi::OsStr>,
        args: impl IntoIterator<Item = std::ffi::OsString>,
    ) -> Result<Self, OracleError> {
        let source = EmbeddedGoOracleSource::materialize()?;
        let mut command = std::process::Command::new(executable);
        command.args(args).current_dir(source.path());
        Ok(Self {
            command,
            _source: Some(source),
        })
    }
}

impl GoOracleCommand {
    fn command_mut(&mut self) -> &mut std::process::Command {
        &mut self.command
    }
}

/// Typed failures at the subprocess, private helper workspace, and protocol boundary.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    /// The configured executable could not be started.
    #[error("Go oracle could not be started ({program}): {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// The private directory for the embedded Go oracle could not be created.
    #[error("Go oracle source workspace could not be created: {0}")]
    GoOracleSourceDirectory(#[source] std::io::Error),
    /// An embedded Go oracle file could not be written to its private workspace.
    #[error("Go oracle source file could not be materialized at {path}: {source}")]
    GoOracleSourceFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The content-addressed offline helper cache could not be validated or installed.
    #[error("Go oracle helper cache failed: {detail}")]
    GoOracleHelperCache { detail: String },
    /// The selected Go executable or GOROOT changed after host admission.
    #[error("Go toolchain contents changed after host admission")]
    GoToolchainIdentityChanged,
    /// Package authority needs a selected Go toolchain, cache roots, and
    /// isolated child environment before it may start an oracle.
    #[error("Go package authority has no explicit isolated child environment")]
    MissingChildEnvironment,
    /// A prebuilt oracle cannot attest that cgo-disabled package loading uses
    /// the admitted active source set.
    #[error("Go oracle binary cannot prove a complete cgo-disabled package closure")]
    UnsupportedCgoOracleBinary,
    /// A helper reported a cgo source omission that cannot be admitted.
    #[error("Go package authority requires an admitted C toolchain for cgo: {detail}")]
    UnsupportedCgo { detail: String },
    /// The selected package root's nearest Go workspace could not be captured
    /// or revalidated before oracle execution.
    #[error("Go workspace authority witness failed: {0}")]
    WorkspaceWitness(String),
    /// A selected module/workspace manifest or local filesystem closure could
    /// not be captured or revalidated before oracle execution.
    #[error(transparent)]
    PackageAuthorityWitness(#[from] GoPackageAuthorityWitnessError),
    /// The package workspace changed after its request was admitted.
    #[error("Go package workspace changed after authority admission")]
    WorkspaceWitnessChanged,
    /// A selected module/workspace/local-target witness changed before spawn.
    #[error("Go package authority witness changed before oracle execution")]
    PackageAuthorityWitnessChanged,
    /// Offline Go package selection did not admit a complete dependency graph.
    #[error("Go dependency closure is unavailable ({failure:?}); run `go mod download` in the project and retry")]
    DependencyClosureUnavailable { failure: GoDependencyClosureFailure },
    /// The configured oracle tool is unavailable.
    #[error("Go oracle tooling unavailable ({tool}): {source}")]
    ToolingUnavailable {
        tool: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The child exited unsuccessfully, retaining its diagnostic tail.
    #[error("Go oracle exited with {status}; stderr tail: {stderr}")]
    Exit { status: String, stderr: String },
    /// The child emitted malformed or incomplete JSON.
    #[error("Go oracle JSON decode failed: {message}; transcript prefix: {transcript}")]
    Decode { message: String, transcript: String },
    /// The output bound was exceeded and the child was reaped.
    #[error(
        "Go oracle output limit exceeded during {phase} on {stream}: observed {observed}, limit {limit}"
    )]
    OutputLimit {
        phase: &'static str,
        stream: &'static str,
        observed: usize,
        limit: usize,
    },
    /// The child did not complete before the deadline and was reaped.
    #[error("Go oracle timed out during {phase} after {milliseconds}ms")]
    Timeout {
        phase: &'static str,
        milliseconds: u64,
    },
    /// Caller cancellation interrupted dependency admission and reaped its child.
    #[error("Go dependency admission was cancelled")]
    Cancelled,
    /// A schema cell was not the version this adapter understands.
    #[error("Go oracle schema is stale: found {found}, expected {expected}")]
    Staleness { found: u32, expected: u32 },
    /// A pipe could not be read or its reader thread failed to join.
    #[error("Go oracle pipe failed during {stream}: {source}")]
    Pipe {
        stream: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// A bounded stream reader panicked; its exact worker and supported
    /// payload facts survive the join boundary.
    #[error("Go oracle stream worker panicked: {cause}")]
    WorkerPanic {
        /// Bounded original join payload.
        #[source]
        cause: NativeWorkerPanic,
    },
}

/// Configurable subprocess adapter for the vendored Go oracle.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct GoOracle {
    /// Maximum bytes retained and accepted from each child stream.
    pub output_limit: usize,
    /// Maximum wall-clock duration for the child.
    pub timeout: std::time::Duration,
}

/// Immutable facts for one caller-selected Go executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOracleExecutable {
    view: GoOracleExecutableView,
}

/// Read-only view of an admitted Go executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOracleExecutableView {
    /// Caller-supplied absolute executable path.
    pub executable: PathBuf,
}

impl core::ops::Deref for GoOracleExecutable {
    type Target = GoOracleExecutableView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl AsRef<Path> for GoOracleExecutable {
    fn as_ref(&self) -> &Path {
        &self.view.executable
    }
}

/// Rejection while admitting an explicit Go executable.
#[derive(Debug, thiserror::Error)]
pub enum GoOracleConfigurationError {
    /// A relative path would defer oracle selection to ambient process state.
    #[error("Go oracle executable is not absolute: {executable:?}")]
    RelativeExecutable {
        /// Caller-supplied relative executable path.
        executable: PathBuf,
    },
    /// A required Go environment root is not absolute.
    #[error("Go authority path is not absolute: {role}={path:?}")]
    RelativeAuthorityPath {
        /// Rejected authority path role.
        role: &'static str,
        /// Caller-supplied relative path.
        path: PathBuf,
    },
    /// The admitted Go executable is not a regular file.
    #[error("configured Go executable is not a file: {path:?}")]
    InvalidGoExecutable {
        /// Rejected Go executable path.
        path: PathBuf,
    },
    /// A required Go environment root is not a directory.
    #[error("configured Go {role} is not a directory: {path:?}")]
    InvalidAuthorityDirectory {
        /// Rejected authority directory role.
        role: &'static str,
        /// Rejected directory path.
        path: PathBuf,
    },
    /// The selected Go compiler installation could not be hashed for the helper cache identity.
    #[error("configured Go toolchain identity could not be captured at {path:?}: {source}")]
    ToolchainIdentity {
        /// Selected GOROOT or compiler path.
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The Go executable configured for `go/packages` differs from the Go
    /// executable used by `go run` to launch the vendored oracle.
    #[error("Go oracle and isolated environment select different Go executables")]
    GoToolchainEnvironmentMismatch,
}

impl GoOracleExecutable {
    /// Admits one absolute executable path.
    pub fn new(executable: PathBuf) -> Result<Self, GoOracleConfigurationError> {
        if !executable.is_absolute() {
            return Err(GoOracleConfigurationError::RelativeExecutable { executable });
        }
        Ok(Self {
            view: GoOracleExecutableView { executable },
        })
    }
}

/// Closed explicit command configuration for the Go oracle.
///
/// An oracle binary directly speaks the extractor protocol.  A Go toolchain
/// runs the vendored extractor source.  Both alternatives retain an explicit
/// absolute executable and neither reads environment variables or `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoOracleConfiguration {
    /// A binary implementing the vendored oracle protocol.
    OracleBinary(GoOracleExecutable),
    /// A Go compiler used to run the vendored oracle source.
    GoToolchain(GoOracleExecutable),
}

/// Complete, explicit environment for the Go oracle and its `go/packages`
/// subprocesses. The selected build policy disables cgo; Go's active file set
/// and excluded build-constraint declarations describe that exact configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoOracleChildEnvironment {
    go_executable: PathBuf,
    goroot: PathBuf,
    module_cache: PathBuf,
    build_cache: PathBuf,
    cgo: GoCgoPolicy,
    path: String,
    toolchain_identity: [u8; 32],
}

/// Explicit cgo execution policy. `Disabled` means the authority uses the
/// package files selected by Go with cgo turned off and records excluded files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoCgoPolicy {
    /// Do not run cgo or use a C compiler; use the files selected with cgo disabled.
    Disabled,
}

impl GoOracleChildEnvironment {
    /// Admits the selected Go executable and semantic/cache roots. The cache
    /// path may be a not-yet-created leaf under an existing work directory.
    pub fn new(
        go_executable: PathBuf,
        goroot: PathBuf,
        module_cache: PathBuf,
        build_cache: PathBuf,
    ) -> Result<Self, GoOracleConfigurationError> {
        if !go_executable.is_absolute() {
            return Err(GoOracleConfigurationError::RelativeAuthorityPath {
                role: "executable",
                path: go_executable,
            });
        }
        if !go_executable.is_file() {
            return Err(GoOracleConfigurationError::InvalidGoExecutable {
                path: go_executable,
            });
        }
        for (role, path) in [("GOROOT", &goroot), ("GOMODCACHE", &module_cache)] {
            if !path.is_absolute() {
                return Err(GoOracleConfigurationError::RelativeAuthorityPath {
                    role,
                    path: path.clone(),
                });
            }
            if !path.is_dir() {
                return Err(GoOracleConfigurationError::InvalidAuthorityDirectory {
                    role,
                    path: path.clone(),
                });
            }
        }
        if !build_cache.is_absolute() {
            return Err(GoOracleConfigurationError::RelativeAuthorityPath {
                role: "GOCACHE",
                path: build_cache,
            });
        }
        let go_directory = go_executable
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf();
        let path = std::env::join_paths([go_directory])
            .map_err(|_| GoOracleConfigurationError::InvalidAuthorityDirectory {
                role: "PATH",
                path: go_executable.clone(),
            })?
            .to_string_lossy()
            .into_owned();
        let toolchain_identity =
            hash_toolchain_identity(&go_executable, &goroot).map_err(|source| {
                GoOracleConfigurationError::ToolchainIdentity {
                    path: goroot.clone(),
                    source,
                }
            })?;
        Ok(Self {
            go_executable,
            goroot,
            module_cache,
            build_cache,
            cgo: GoCgoPolicy::Disabled,
            path,
            toolchain_identity,
        })
    }

    /// Returns the selected Go compiler executable.
    #[must_use]
    pub fn go_executable(&self) -> &Path {
        &self.go_executable
    }

    /// Returns the selected Go installation root.
    #[must_use]
    pub fn goroot(&self) -> &Path {
        &self.goroot
    }

    /// Returns the admitted module-cache root.
    #[must_use]
    pub fn module_cache(&self) -> &Path {
        &self.module_cache
    }

    /// Returns the transient Go build-cache path.
    #[must_use]
    pub fn build_cache(&self) -> &Path {
        &self.build_cache
    }

    /// Returns the content identity of the selected compiler executable and complete GOROOT.
    #[must_use]
    pub const fn toolchain_identity(&self) -> [u8; 32] {
        self.toolchain_identity
    }

    /// Revalidates the full bounded Go executable and GOROOT closure once at
    /// the package boundary before helper preparation or package dispatch.
    fn revalidate_toolchain(&self) -> Result<(), OracleError> {
        let observed =
            hash_toolchain_identity(&self.go_executable, &self.goroot).map_err(|source| {
                OracleError::ToolingUnavailable {
                    tool: "Go toolchain identity",
                    source,
                }
            })?;
        if observed != self.toolchain_identity {
            return Err(OracleError::GoToolchainIdentityChanged);
        }
        Ok(())
    }

    /// Returns the explicit cgo policy.
    #[must_use]
    pub const fn cgo_policy(&self) -> GoCgoPolicy {
        self.cgo
    }

    fn apply_to(
        &self,
        command: &mut std::process::Command,
        work: &GoWorkWitness,
        toolchain_mode: bool,
    ) {
        command.env_clear();
        command
            .env("PATH", &self.path)
            .env("GOROOT", &self.goroot)
            .env("GOMODCACHE", &self.module_cache)
            .env("GOCACHE", &self.build_cache)
            .env("GOENV", "off")
            .env("GOTOOLCHAIN", "local")
            .env("GOPROXY", "off")
            .env("GOSUMDB", "off")
            .env("GOPACKAGESDRIVER", "off")
            .env("CGO_ENABLED", "0");
        // `go run` builds the vendored helper outside the selected project
        // workspace. The helper receives a separate private value and applies
        // it to `packages.Config.Env` after it starts. A prebuilt oracle can
        // receive the selected GOWORK directly.
        if toolchain_mode {
            command
                .env("GOWORK", "off")
                .env("NUDOX_GO_AUTHORITY_GOWORK", work.go_work_value());
        } else {
            command.env("GOWORK", work.go_work_value());
        }
        #[cfg(windows)]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
    }
}

/// Path-independent mode of one explicit Go oracle invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GoOracleInvocationModeV1 {
    /// A selected binary directly implements the oracle protocol.
    OracleBinary,
    /// The selected Go compiler builds and runs the vendored oracle source.
    GoToolchain,
}

/// Portable Go oracle options that omit host-local executable paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GoOracleInvocationOptionsV1 {
    mode: GoOracleInvocationModeV1,
    helper_source_identity: Option<[u8; 32]>,
}

impl GoOracleInvocationOptionsV1 {
    /// Returns the exact closed command mode.
    #[must_use]
    pub const fn mode(self) -> GoOracleInvocationModeV1 {
        self.mode
    }

    /// Returns the content identity of the vendored helper source in Go-toolchain mode.
    #[must_use]
    pub const fn helper_source_identity(self) -> Option<[u8; 32]> {
        self.helper_source_identity
    }
}

impl GoOracleConfiguration {
    /// Configures a direct oracle binary.
    pub fn oracle_binary(executable: PathBuf) -> Result<Self, GoOracleConfigurationError> {
        Ok(Self::OracleBinary(GoOracleExecutable::new(executable)?))
    }

    /// Configures an explicit Go toolchain for the vendored oracle source.
    pub fn go_toolchain(executable: PathBuf) -> Result<Self, GoOracleConfigurationError> {
        Ok(Self::GoToolchain(GoOracleExecutable::new(executable)?))
    }
}

/// Bounded Go oracle capability with caller-selected execution authority.
#[derive(Debug, Clone)]
pub struct ConfiguredGoOracle {
    oracle: GoOracle,
    configuration: GoOracleConfiguration,
    child_environment: Option<GoOracleChildEnvironment>,
}

impl Default for GoOracle {
    fn default() -> Self {
        Self {
            output_limit: 16 * 1024 * 1024,
            timeout: std::time::Duration::from_secs(60),
        }
    }
}

impl GoOracle {
    /// Binds this bounded adapter to one caller-selected executable
    /// configuration.  The returned capability never consults environment
    /// variables or `PATH`.
    #[must_use]
    pub fn with_configuration(self, configuration: GoOracleConfiguration) -> ConfiguredGoOracle {
        ConfiguredGoOracle {
            oracle: self,
            configuration,
            child_environment: None,
        }
    }

    /// Decodes a transcript without starting a process.
    pub fn decode(&self, bytes: &[u8]) -> Result<Output, OracleError> {
        let output: Output =
            serde_json::from_slice(bytes).map_err(|error| OracleError::Decode {
                message: error.to_string(),
                transcript: transcript_prefix(bytes),
            })?;
        if output.schema_version != Output::REQUIRED_SCHEMA_VERSION {
            return Err(OracleError::Staleness {
                found: output.schema_version,
                expected: Output::REQUIRED_SCHEMA_VERSION,
            });
        }
        Ok(output)
    }

    /// Runs the override executable, or `go run . <module>` from an embedded private helper workspace.
    pub fn run(&self, module: &Path) -> Result<Output, OracleError> {
        let mut command = if let Ok(binary) = std::env::var("NUDOX_GO_ORACLE_BIN") {
            let mut command = std::process::Command::new(binary);
            command.arg(module);
            GoOracleCommand::binary(command)
        } else {
            let compiler = match std::env::var("COMPILER_GO_COMPILER") {
                Ok(path) => path,
                Err(_) => "go".to_owned(),
            };
            GoOracleCommand::toolchain(
                compiler,
                [
                    std::ffi::OsString::from("run"),
                    std::ffi::OsString::from("-mod=vendor"),
                    std::ffi::OsString::from("."),
                    module.as_os_str().to_owned(),
                ],
            )?
        };
        let stdout = self.execute(command.command_mut())?;
        self.decode(&stdout)
    }

    /// Runs the authority-image producer for one caller-selected source
    /// file, returning the exact binary image bytes bound to that source's
    /// SHA-256 digest. Uses the same override executable as [`GoOracle::run`]
    /// (invoked as `<bin> --authority-image <source> <module>`) or
    /// `go run . --authority-image <source> <module>` from an embedded
    /// private helper workspace.
    pub fn authority_image(&self, source: &Path, module: &Path) -> Result<Vec<u8>, OracleError> {
        self.authority_image_with_mode("--authority-image", source, module)
    }

    /// Runs the authority-image producer for exactly the package that owns
    /// `source`, resolving imports from the module rooted at `module`. Unlike
    /// [`GoOracle::authority_image`], sibling packages are import context only
    /// and are never serialized, so a module whose subpackages share a
    /// declaration spelling cannot inject a coordinate-free collision into the
    /// selected package's image.
    pub fn authority_image_for_package(
        &self,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        self.authority_image_with_mode("--authority-image-package", source, module)
    }

    fn authority_image_with_mode(
        &self,
        mode: &str,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let mut command = if let Ok(binary) = std::env::var("NUDOX_GO_ORACLE_BIN") {
            let mut command = std::process::Command::new(binary);
            command.arg(mode).arg(source).arg(module);
            GoOracleCommand::binary(command)
        } else {
            let compiler = match std::env::var("COMPILER_GO_COMPILER") {
                Ok(path) => path,
                Err(_) => "go".to_owned(),
            };
            GoOracleCommand::toolchain(
                compiler,
                [
                    std::ffi::OsString::from("run"),
                    std::ffi::OsString::from("-mod=vendor"),
                    std::ffi::OsString::from("."),
                    std::ffi::OsString::from(mode),
                    source.as_os_str().to_owned(),
                    module.as_os_str().to_owned(),
                ],
            )?
        };
        self.execute(command.command_mut())
    }

    fn configured_command(
        configuration: &GoOracleConfiguration,
        helper_binary: Option<&Path>,
        authority_source: Option<(&str, &Path)>,
        module: &Path,
    ) -> Result<GoOracleCommand, OracleError> {
        if let Some(helper_binary) = helper_binary {
            let mut command = std::process::Command::new(helper_binary);
            if let Some((mode, source)) = authority_source {
                command.arg(mode).arg(source);
            }
            command.arg(module);
            return Ok(GoOracleCommand::binary(command));
        }
        match configuration {
            GoOracleConfiguration::OracleBinary(executable) => {
                let mut command = std::process::Command::new(executable.as_ref());
                if let Some((mode, source)) = authority_source {
                    command.arg(mode).arg(source);
                }
                command.arg(module);
                Ok(GoOracleCommand::binary(command))
            }
            GoOracleConfiguration::GoToolchain(executable) => {
                let mut args = vec![
                    std::ffi::OsString::from("run"),
                    std::ffi::OsString::from("-mod=vendor"),
                    std::ffi::OsString::from("."),
                ];
                if let Some((mode, source)) = authority_source {
                    args.push(std::ffi::OsString::from(mode));
                    args.push(source.as_os_str().to_owned());
                }
                args.push(module.as_os_str().to_owned());
                GoOracleCommand::toolchain(executable.as_ref(), args)
            }
        }
    }

    fn run_configured(
        &self,
        configuration: &GoOracleConfiguration,
        environment: Option<&GoOracleChildEnvironment>,
        work: Option<&GoWorkWitness>,
        helper_binary: Option<&Path>,
        module: &Path,
    ) -> Result<Output, OracleError> {
        let mut command = Self::configured_command(configuration, helper_binary, None, module)?;
        if let (Some(environment), Some(work)) = (environment, work) {
            environment.apply_to(
                command.command_mut(),
                work,
                matches!(configuration, GoOracleConfiguration::GoToolchain(_)),
            );
        }
        let stdout = self.execute_configured(command.command_mut())?;
        self.decode(&stdout)
    }

    fn authority_image_configured(
        &self,
        configuration: &GoOracleConfiguration,
        environment: &GoOracleChildEnvironment,
        work: &GoWorkWitness,
        helper_binary: Option<&Path>,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let mut command = Self::configured_command(
            configuration,
            helper_binary,
            Some(("--authority-image", source)),
            module,
        )?;
        environment.apply_to(
            command.command_mut(),
            work,
            matches!(configuration, GoOracleConfiguration::GoToolchain(_)),
        );
        self.execute_configured(command.command_mut())
    }

    fn authority_image_for_package_configured(
        &self,
        configuration: &GoOracleConfiguration,
        environment: &GoOracleChildEnvironment,
        work: &GoWorkWitness,
        helper_binary: Option<&Path>,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let mut command = Self::configured_command(
            configuration,
            helper_binary,
            Some(("--authority-image-package", source)),
            module,
        )?;
        environment.apply_to(
            command.command_mut(),
            work,
            matches!(configuration, GoOracleConfiguration::GoToolchain(_)),
        );
        self.execute_configured(command.command_mut())
    }

    /// Spawns one bounded oracle child and collects its standard output.
    /// The child runs in its own process group; oversized output, deadlines,
    /// and pipe faults all reap the child and fold into typed rejections.
    fn execute(&self, command: &mut std::process::Command) -> Result<Vec<u8>, OracleError> {
        self.execute_with_origin(command, false, None)
    }

    fn execute_configured(
        &self,
        command: &mut std::process::Command,
    ) -> Result<Vec<u8>, OracleError> {
        self.execute_with_origin(command, true, None)
    }

    fn execute_with_origin(
        &self,
        command: &mut std::process::Command,
        configured: bool,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Vec<u8>, OracleError> {
        if cancelled.is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(OracleError::Cancelled);
        }
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| {
            if configured {
                OracleError::Spawn {
                    program: command.get_program().to_string_lossy().into_owned(),
                    source,
                }
            } else {
                OracleError::ToolingUnavailable {
                    tool: "NUDOX_GO_ORACLE_BIN or go run",
                    source,
                }
            }
        })?;
        let stdout = child.stdout.take().ok_or_else(|| OracleError::Pipe {
            stream: "stdout",
            source: std::io::Error::other("stdout was not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| OracleError::Pipe {
            stream: "stderr",
            source: std::io::Error::other("stderr was not piped"),
        })?;
        let limit = self.output_limit;
        let (limit_tx, limit_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let out_tx = limit_tx.clone();
        let out_done = done_tx.clone();
        let out_thread = std::thread::spawn(move || {
            let result = read_bounded(stdout, limit, "stdout", out_tx);
            let _ = out_done.send("stdout");
            result
        });
        let err_thread = std::thread::spawn(move || {
            let result = read_bounded(stderr, limit, "stderr", limit_tx);
            let _ = done_tx.send("stderr");
            result
        });
        let started = std::time::Instant::now();
        let mut terminal = None;
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            if cancelled.is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire)) {
                terminate_child(&mut child);
                drop(out_thread);
                drop(err_thread);
                return Err(OracleError::Cancelled);
            }
            if let Ok((stream, observed)) = limit_rx.try_recv() {
                terminate_child(&mut child);
                // A descendant may maliciously retain one of the inherited
                // pipes even after the direct child is reaped. Detach the
                // readers on this closed terminal instead of turning a
                // bounded output rejection into an unbounded join.
                drop(out_thread);
                drop(err_thread);
                return Err(OracleError::OutputLimit {
                    phase: "collect",
                    stream,
                    observed,
                    limit,
                });
            }
            while let Ok(stream) = done_rx.try_recv() {
                match stream {
                    "stdout" => stdout_done = true,
                    "stderr" => stderr_done = true,
                    _ => unreachable!("reader completion lane is closed"),
                }
            }
            if terminal.is_none() {
                terminal = child.try_wait().map_err(|source| OracleError::Pipe {
                    stream: "process",
                    source,
                })?;
            }
            if terminal.is_some() && stdout_done && stderr_done {
                break;
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                drop(out_thread);
                drop(err_thread);
                return Err(OracleError::Timeout {
                    phase: "collect",
                    milliseconds: self.timeout.as_millis() as u64,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let stdout = out_thread
            .join()
            .map_err(|payload| OracleError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardOutputReader,
                    payload.as_ref(),
                ),
            })?;
        let stderr = err_thread
            .join()
            .map_err(|payload| OracleError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardErrorReader,
                    payload.as_ref(),
                ),
            })?;
        if let Some((stream, source)) = stdout.error.or(stderr.error) {
            return Err(OracleError::Pipe { stream, source });
        }
        if let Some((stream, observed)) = stdout.exceeded.or(stderr.exceeded) {
            return Err(OracleError::OutputLimit {
                phase: "collect",
                stream,
                observed,
                limit,
            });
        }
        let status = terminal.ok_or_else(|| OracleError::Pipe {
            stream: "process",
            source: std::io::Error::other("missing child status"),
        })?;
        if !status.success() {
            let diagnostic = tail(&stderr.bytes);
            if diagnostic.contains(UNSUPPORTED_CGO_SENTINEL) {
                return Err(OracleError::UnsupportedCgo { detail: diagnostic });
            }
            return Err(OracleError::Exit {
                status: status.to_string(),
                stderr: diagnostic,
            });
        }
        Ok(stdout.bytes)
    }
}

impl ConfiguredGoOracle {
    /// Binds the configured oracle to exact Go executable and cache roots.
    /// Go-toolchain mode must use the same executable for the helper build;
    /// oracle-binary mode still uses it for `go/packages` subprocesses.
    pub fn with_child_environment(
        mut self,
        environment: GoOracleChildEnvironment,
    ) -> Result<Self, GoOracleConfigurationError> {
        if matches!(
            &self.configuration,
            GoOracleConfiguration::GoToolchain(configured)
                if configured.as_ref() != environment.go_executable()
        ) {
            return Err(GoOracleConfigurationError::GoToolchainEnvironmentMismatch);
        }
        self.child_environment = Some(environment);
        Ok(self)
    }

    fn cached_helper_binary(&self) -> Result<(Option<PathBuf>, bool), OracleError> {
        let GoOracleConfiguration::GoToolchain(executable) = &self.configuration else {
            return Ok((None, false));
        };
        let Some(environment) = &self.child_environment else {
            return Ok((None, false));
        };
        self.prepare_cached_helper(executable.as_ref(), environment)
            .map(|(path, revalidated_after_build)| (Some(path), revalidated_after_build))
    }

    fn prepare_cached_helper(
        &self,
        executable: &Path,
        environment: &GoOracleChildEnvironment,
    ) -> Result<(PathBuf, bool), OracleError> {
        use fs4::fs_std::FileExt;
        use std::time::Instant;

        let source_identity = helper_source_identity();
        let toolchain_identity = environment.toolchain_identity();
        let key = helper_cache_key(&source_identity, &toolchain_identity);
        let cache_root_path = environment.build_cache().join("nudox-go-oracle-v1");
        let cache_root = open_private_helper_cache(&cache_root_path).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("open private cache root {cache_root_path:?}: {error}"),
            }
        })?;
        let lock_file = cache_root
            .open_private_file_read_write("build.lock", true)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("open private cache lock in {cache_root_path:?}: {error}"),
            })?;
        let started = Instant::now();
        loop {
            let acquired = match FileExt::try_lock_exclusive(&lock_file) {
                Ok(acquired) => acquired,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => {
                    return Err(OracleError::GoOracleHelperCache {
                        detail: format!("lock cache {cache_root_path:?}: {error}"),
                    });
                }
            };
            if acquired {
                break;
            }
            if started.elapsed() >= self.oracle.timeout {
                return Err(OracleError::GoOracleHelperCache {
                    detail: format!("timed out waiting for cache lock {cache_root_path:?}"),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        clean_abandoned_helper_staging(&cache_root).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("clean bounded cache staging in {cache_root_path:?}: {error}"),
            }
        })?;
        let binary_name = if cfg!(windows) {
            "oracle.exe"
        } else {
            "oracle"
        };
        let manifest_prefix = format!(
            "schema=1\nsource={}\ntoolchain={}\n",
            digest_hex(&source_identity),
            digest_hex(&toolchain_identity),
        );
        let binary_path = cache_root_path.join(&key).join(binary_name);
        if validate_helper_cache_entry(&cache_root, &key, binary_name, &manifest_prefix).map_err(
            |error| OracleError::GoOracleHelperCache {
                detail: format!("validate bounded cache entry {binary_path:?}: {error}"),
            },
        )? {
            cache_root.verify_path(&cache_root_path).map_err(|error| {
                OracleError::GoOracleHelperCache {
                    detail: format!("cache root identity changed at {cache_root_path:?}: {error}"),
                }
            })?;
            return Ok((binary_path, false));
        }
        remove_helper_cache_entry(&cache_root, &key).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("remove invalid bounded cache entry {binary_path:?}: {error}"),
            }
        })?;

        let staging = tempfile::Builder::new()
            .prefix("go-oracle-build-")
            .tempdir_in(&cache_root_path)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("create staging directory in {cache_root_path:?}: {error}"),
            })?;
        let staging_path = staging.path().to_path_buf();
        let staging_name = staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| OracleError::GoOracleHelperCache {
                detail: "helper staging directory name is not UTF-8".to_owned(),
            })?
            .to_owned();
        let staging_capability = cache_root
            .open_dir(&staging_name)
            .and_then(|directory| {
                directory.restrict_private()?;
                directory.validate_private()?;
                directory.verify_path(&staging_path)?;
                Ok(directory)
            })
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("open pinned helper staging directory {staging_path:?}: {error}"),
            })?;
        cache_root.verify_path(&cache_root_path).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("cache root identity changed before helper build: {error}"),
            }
        })?;
        let source = EmbeddedGoOracleSource::materialize()?;
        let staged_binary = staging_path.join(binary_name);
        let mut build = std::process::Command::new(executable);
        build
            .args([
                std::ffi::OsStr::new("build"),
                std::ffi::OsStr::new("-mod=vendor"),
                std::ffi::OsStr::new("-trimpath"),
                std::ffi::OsStr::new("-buildvcs=false"),
                std::ffi::OsStr::new("-o"),
            ])
            .arg(&staged_binary)
            .arg(".")
            .current_dir(source.path());
        environment.apply_to(&mut build, &GoWorkWitness::Disabled, true);
        self.oracle.execute_configured(&mut build)?;
        // Reject toolchain drift caused by the helper build before publishing
        // a manifest that would bind its output to the old identity.
        environment.revalidate_toolchain()?;
        cache_root
            .verify_path(&cache_root_path)
            .and_then(|()| staging_capability.verify_path(&staging_path))
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("helper staging identity changed during build: {error}"),
            })?;
        let mut binary_file = staging_capability
            .open_file_read_write(binary_name, false)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("open staged helper binary {staged_binary:?}: {error}"),
            })?;
        let mut binary_digest = Sha256::new();
        hash_regular_file_handle(
            &mut binary_file,
            MAX_HELPER_BINARY_BYTES,
            RegularFileRole::PrivateHelperBinary,
            &mut binary_digest,
        )
        .map_err(|error| OracleError::GoOracleHelperCache {
            detail: format!("hash bounded staged helper binary {staged_binary:?}: {error}"),
        })?;
        binary_file
            .sync_all()
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("sync staged helper binary {staged_binary:?}: {error}"),
            })?;
        let binary_hash = digest_hex(&binary_digest.finalize());
        let manifest = format!("{manifest_prefix}binary={binary_hash}\n");
        if manifest.len() as u64 > MAX_HELPER_MANIFEST_BYTES {
            return Err(OracleError::GoOracleHelperCache {
                detail: "generated helper cache manifest exceeds its byte limit".to_owned(),
            });
        }
        let mut manifest_file = staging_capability
            .create_file_exclusive("manifest.txt")
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("create staged helper manifest: {error}"),
            })?;
        use std::io::Write;
        manifest_file
            .write_all(manifest.as_bytes())
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("write staged helper manifest: {error}"),
            })?;
        manifest_file
            .sync_all()
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("sync staged helper manifest: {error}"),
            })?;
        staging_capability
            .sync_all()
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("sync staged helper generation: {error}"),
            })?;
        let _staging_path = staging.keep();
        cache_root
            .rename(&staging_name, &key, false)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("atomically publish helper generation {binary_path:?}: {error}"),
            })?;
        if !validate_helper_cache_entry(&cache_root, &key, binary_name, &manifest_prefix).map_err(
            |error| OracleError::GoOracleHelperCache {
                detail: format!("verify published helper generation {binary_path:?}: {error}"),
            },
        )? {
            return Err(OracleError::GoOracleHelperCache {
                detail: format!(
                    "published helper generation failed bounded validation: {binary_path:?}"
                ),
            });
        }
        cache_root.verify_path(&cache_root_path).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("cache root identity changed after publish: {error}"),
            }
        })?;
        Ok((binary_path, true))
    }

    /// Captures the nearest selected `go.work` for a package module root.
    pub fn go_work_witness(
        &self,
        module_root: impl AsRef<Path>,
    ) -> Result<GoWorkWitness, GoWorkWitnessError> {
        GoWorkWitness::capture(module_root)
    }

    /// Captures the selected module/workspace manifests and every bounded
    /// local filesystem target that can affect package loading.
    pub fn package_authority_witness(
        &self,
        package_root: impl AsRef<Path>,
    ) -> Result<GoPackageAuthorityWitness, GoPackageAuthorityWitnessError> {
        self.package_authority_witness_cancellable(package_root, None)
    }

    /// Captures fresh Go-selected dependency files on the requesting worker,
    /// with caller cancellation reaping the bounded listing child.
    pub fn package_authority_witness_cancellable(
        &self,
        package_root: impl AsRef<Path>,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<GoPackageAuthorityWitness, GoPackageAuthorityWitnessError> {
        let mut witness = GoPackageAuthorityWitness::capture(package_root)?;
        if let Some(environment) = &self.child_environment {
            witness.capture_dependency_closure(environment, self.oracle, cancelled);
        }
        Ok(witness)
    }

    /// Returns path-independent invocation mode and helper-source identity.
    #[must_use]
    pub fn portable_invocation_options(&self) -> GoOracleInvocationOptionsV1 {
        let (mode, helper_source_identity) = match &self.configuration {
            GoOracleConfiguration::OracleBinary(_) => {
                (GoOracleInvocationModeV1::OracleBinary, None)
            }
            GoOracleConfiguration::GoToolchain(_) => (
                GoOracleInvocationModeV1::GoToolchain,
                Some(helper_source_identity()),
            ),
        };
        GoOracleInvocationOptionsV1 {
            mode,
            helper_source_identity,
        }
    }

    /// Reports whether Go-toolchain mode uses the exact executable selected as the native Go
    /// toolchain. Oracle-binary mode has a separate, unversioned executable authority.
    #[must_use]
    pub fn uses_toolchain_executable(&self, executable: &Path) -> bool {
        matches!(
            &self.configuration,
            GoOracleConfiguration::GoToolchain(configured) if configured.as_ref() == executable
        )
    }

    /// Returns a host-local fingerprint of the explicit oracle command,
    /// bundled Go producer closure, and process bounds. Path bytes make this
    /// a drift detector rather than a cross-host closure identity.
    #[must_use]
    pub fn local_configuration_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"compiler.go.package-authority.v1\0");
        digest.update(self.oracle.output_limit.to_be_bytes());
        digest.update(self.oracle.timeout.as_secs().to_be_bytes());
        digest.update(self.oracle.timeout.subsec_nanos().to_be_bytes());
        match &self.configuration {
            GoOracleConfiguration::OracleBinary(executable) => {
                digest.update([0]);
                update_path_digest(&mut digest, executable.as_ref());
            }
            GoOracleConfiguration::GoToolchain(executable) => {
                digest.update([1]);
                update_path_digest(&mut digest, executable.as_ref());
                digest.update(helper_source_identity());
            }
        }
        if let Some(environment) = &self.child_environment {
            digest.update(GO_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1.as_bytes());
            digest.update([0]);
            update_path_digest(&mut digest, &environment.go_executable);
            update_path_digest(&mut digest, &environment.goroot);
            update_path_digest(&mut digest, &environment.module_cache);
            digest.update(environment.toolchain_identity());
            digest.update([match environment.cgo {
                GoCgoPolicy::Disabled => 0,
            }]);
        } else {
            digest.update(b"go-child-environment.missing\0");
        }
        digest.finalize().into()
    }

    /// Returns the package-root-specific identity, including the exact
    /// nearest `go.work` path and contents. The transient GOCACHE is omitted.
    pub fn local_configuration_fingerprint_for_module(
        &self,
        module_root: impl AsRef<Path>,
    ) -> Result<[u8; 32], GoWorkWitnessError> {
        let mut digest = Sha256::new();
        digest.update(self.local_configuration_fingerprint());
        digest.update(self.go_work_witness(module_root)?.identity());
        Ok(digest.finalize().into())
    }

    /// Returns the host-local oracle fingerprint bound to the package's Go
    /// manifests and bounded local-target witness. Incomplete witnesses stay
    /// local-only and must not authorize semantic reuse.
    pub fn local_configuration_fingerprint_for_package(
        &self,
        package_root: impl AsRef<Path>,
    ) -> Result<[u8; 32], GoPackageAuthorityWitnessError> {
        let witness = self.package_authority_witness(package_root)?;
        let mut digest = Sha256::new();
        digest.update(b"compiler.go.package-authority-local.v1\0");
        digest.update(self.local_configuration_fingerprint());
        digest.update(witness.identity());
        Ok(digest.finalize().into())
    }

    /// Runs the selected oracle configuration over one module.
    pub fn run(&self, module: &Path) -> Result<Output, OracleError> {
        let work = self
            .child_environment
            .as_ref()
            .map(|_| GoWorkWitness::capture(module))
            .transpose()
            .map_err(|error| OracleError::WorkspaceWitness(error.to_string()))?;
        if let Some(environment) = &self.child_environment {
            environment.revalidate_toolchain()?;
        }
        let (helper_binary, revalidated_after_build) = self.cached_helper_binary()?;
        if !revalidated_after_build {
            if let Some(environment) = &self.child_environment {
                environment.revalidate_toolchain()?;
            }
        }
        let output = self.oracle.run_configured(
            &self.configuration,
            self.child_environment.as_ref(),
            work.as_ref(),
            helper_binary.as_deref(),
            module,
        )?;
        if let Some(environment) = &self.child_environment {
            environment.revalidate_toolchain()?;
        }
        Ok(output)
    }

    /// Produces the authority image for one selected source file and module.
    pub fn authority_image(&self, source: &Path, module: &Path) -> Result<Vec<u8>, OracleError> {
        let (environment, work) = self.authority_child_context(module, None)?;
        environment.revalidate_toolchain()?;
        let (helper_binary, revalidated_after_build) = self.cached_helper_binary()?;
        if !revalidated_after_build {
            environment.revalidate_toolchain()?;
        }
        let image = self.oracle.authority_image_configured(
            &self.configuration,
            environment,
            &work,
            helper_binary.as_deref(),
            source,
            module,
        )?;
        environment.revalidate_toolchain()?;
        Ok(image)
    }

    /// Produces the authority image for exactly the package that owns
    /// `source`, resolving imports from the module rooted at `module`. Unlike
    /// [`ConfiguredGoOracle::authority_image`], sibling packages are import
    /// context only and are never serialized, so a module whose subpackages
    /// share a declaration spelling (e.g. `errgroup.Group` next to
    /// `singleflight.Group` in `golang.org/x/sync`) cannot inject a
    /// coordinate-free collision into the selected package's image.
    pub fn authority_image_for_package(
        &self,
        source: &Path,
        module: &Path,
    ) -> Result<Vec<u8>, OracleError> {
        let witness = GoPackageAuthorityWitness::capture(module)?;
        self.authority_image_for_package_with_authority_witness(source, module, &witness)
    }

    /// Produces the selected package image under the exact workspace witness
    /// captured when the package request was admitted. The witness is
    /// revalidated immediately before spawning the oracle.
    pub fn authority_image_for_package_with_witness(
        &self,
        source: &Path,
        module: &Path,
        witness: &GoWorkWitness,
    ) -> Result<Vec<u8>, OracleError> {
        let (environment, work) = self.authority_child_context(module, Some(witness))?;
        environment.revalidate_toolchain()?;
        let (helper_binary, revalidated_after_build) = self.cached_helper_binary()?;
        if !revalidated_after_build {
            environment.revalidate_toolchain()?;
        }
        let image = self.oracle.authority_image_for_package_configured(
            &self.configuration,
            environment,
            &work,
            helper_binary.as_deref(),
            source,
            module,
        )?;
        environment.revalidate_toolchain()?;
        Ok(image)
    }

    /// Produces the selected package image under the exact package-authority
    /// witness captured at admission. The complete bounded closure is
    /// re-captured after command construction and immediately before spawn.
    pub fn authority_image_for_package_with_authority_witness(
        &self,
        source: &Path,
        package_root: &Path,
        witness: &GoPackageAuthorityWitness,
    ) -> Result<Vec<u8>, OracleError> {
        let environment = self
            .child_environment
            .as_ref()
            .ok_or(OracleError::MissingChildEnvironment)?;
        if matches!(&self.configuration, GoOracleConfiguration::OracleBinary(_))
            && environment.cgo_policy() == GoCgoPolicy::Disabled
        {
            return Err(OracleError::UnsupportedCgoOracleBinary);
        }

        if !witness.matches_current(package_root)? {
            return Err(OracleError::PackageAuthorityWitnessChanged);
        }
        if let Some(failure) = witness.dependency_closure_failure() {
            return Err(OracleError::DependencyClosureUnavailable { failure });
        }
        environment.revalidate_toolchain()?;
        let (helper_binary, revalidated_after_build) = self.cached_helper_binary()?;
        if !revalidated_after_build {
            environment.revalidate_toolchain()?;
        }
        let mut command = GoOracle::configured_command(
            &self.configuration,
            helper_binary.as_deref(),
            Some(("--authority-image-package", source)),
            package_root,
        )?;
        environment.apply_to(
            command.command_mut(),
            witness.go_work_witness(),
            matches!(&self.configuration, GoOracleConfiguration::GoToolchain(_)),
        );
        if !witness.matches_current(package_root)? {
            return Err(OracleError::PackageAuthorityWitnessChanged);
        }
        let image = self.oracle.execute_configured(command.command_mut())?;
        if !witness.matches_current(package_root)? {
            return Err(OracleError::PackageAuthorityWitnessChanged);
        }
        environment.revalidate_toolchain()?;
        Ok(image)
    }

    fn authority_child_context(
        &self,
        module: &Path,
        captured: Option<&GoWorkWitness>,
    ) -> Result<(&GoOracleChildEnvironment, GoWorkWitness), OracleError> {
        let environment = self
            .child_environment
            .as_ref()
            .ok_or(OracleError::MissingChildEnvironment)?;
        if matches!(&self.configuration, GoOracleConfiguration::OracleBinary(_))
            && environment.cgo_policy() == GoCgoPolicy::Disabled
        {
            return Err(OracleError::UnsupportedCgoOracleBinary);
        }
        let work = if let Some(captured) = captured {
            if !captured
                .matches_current(module)
                .map_err(|error| OracleError::WorkspaceWitness(error.to_string()))?
            {
                return Err(OracleError::WorkspaceWitnessChanged);
            }
            captured.clone()
        } else {
            GoWorkWitness::capture(module)
                .map_err(|error| OracleError::WorkspaceWitness(error.to_string()))?
        };
        Ok((environment, work))
    }
}

fn cache_entry_is_valid(
    cache_root: &DirectoryCapability,
    entry_name: &str,
    binary_name: &str,
    manifest_prefix: &str,
) -> bool {
    validate_helper_cache_entry(cache_root, entry_name, binary_name, manifest_prefix)
        .unwrap_or(false)
}

fn validate_helper_cache_entry(
    cache_root: &DirectoryCapability,
    entry_name: &str,
    binary_name: &str,
    manifest_prefix: &str,
) -> std::io::Result<bool> {
    let entry = match cache_root.open_dir(entry_name) {
        Ok(entry) => entry,
        Err(_) => return Ok(false),
    };
    if entry.validate_private().is_err() {
        return Ok(false);
    }
    let entries = entry.entries(MAX_HELPER_CACHE_ENTRY_ENTRIES)?;
    if entries.len() != 2
        || entries.iter().any(|item| item.kind != EntryKind::File)
        || !entries.iter().any(|item| item.name == binary_name)
        || !entries.iter().any(|item| item.name == "manifest.txt")
    {
        return Ok(false);
    }
    let manifest =
        match read_bounded_regular_file(&entry, "manifest.txt", MAX_HELPER_MANIFEST_BYTES) {
            Ok(manifest) => manifest,
            Err(_) => return Ok(false),
        };
    let Ok(manifest_text) = std::str::from_utf8(&manifest) else {
        return Ok(false);
    };
    let Some(expected_binary_hash) = manifest_text
        .strip_prefix(manifest_prefix)
        .and_then(|rest| rest.strip_prefix("binary="))
        .and_then(|rest| rest.strip_suffix('\n'))
    else {
        return Ok(false);
    };
    if expected_binary_hash.len() != 64
        || !expected_binary_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Ok(false);
    }
    let mut binary = match entry.open_file_read(binary_name) {
        Ok(binary) => binary,
        Err(_) => return Ok(false),
    };
    let mut binary_digest = Sha256::new();
    if hash_regular_file_handle(
        &mut binary,
        MAX_HELPER_BINARY_BYTES,
        RegularFileRole::PrivateHelperBinary,
        &mut binary_digest,
    )
    .is_err()
    {
        return Ok(false);
    }
    Ok(digest_hex(&binary_digest.finalize()) == expected_binary_hash)
}

fn read_bounded_regular_file(
    directory: &DirectoryCapability,
    name: &str,
    maximum: u64,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut file = directory.open_file_read(name)?;
    let before = validate_regular_file_handle(&file, maximum, RegularFileRole::PrivateManifest)?;
    let before_modified = before.modified()?;
    let capacity = usize::try_from(before.len())
        .map_err(|_| std::io::Error::other("bounded manifest length does not fit memory"))?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 256];
    loop {
        let remaining = maximum.saturating_sub(bytes.len() as u64).saturating_add(1);
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| std::io::Error::other("manifest read bound does not fit memory"))?;
        let read = file.read(&mut buffer[..read_limit])?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) as u64 > maximum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "helper cache manifest exceeds its byte limit",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let after = validate_regular_file_handle(&file, maximum, RegularFileRole::PrivateManifest)?;
    if after.len() != before.len()
        || bytes.len() as u64 != before.len()
        || after.modified()? != before_modified
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "helper cache manifest changed while it was read",
        ));
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum RegularFileRole {
    PrivateManifest,
    PrivateHelperBinary,
    ExternalToolchainExecutable,
    ExternalToolchainMember,
}

impl RegularFileRole {
    const fn require_executable(self) -> bool {
        matches!(
            self,
            Self::PrivateHelperBinary | Self::ExternalToolchainExecutable
        )
    }

    const fn require_single_link(self) -> bool {
        matches!(self, Self::PrivateManifest | Self::PrivateHelperBinary)
    }
}

fn validate_regular_file_handle(
    file: &std::fs::File,
    maximum: u64,
    role: RegularFileRole,
) -> std::io::Result<std::fs::Metadata> {
    use std::io::ErrorKind;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "opened helper/toolchain object is not a regular file",
        ));
    }
    if metadata.len() > maximum {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("opened helper/toolchain file exceeds {maximum} bytes"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if role.require_single_link() && metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "opened private helper artifact has multiple hard links",
            ));
        }
        if role.require_executable() && metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "opened helper/toolchain binary is not executable",
            ));
        }
    }
    #[cfg(windows)]
    {
        if role.require_single_link() && backend_platform::file_identity::number_of_links(file)? != 1 {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "opened private helper artifact has multiple hard links",
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = role;
        return Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "helper/toolchain file identity is unsupported on this platform",
        ));
    }
    Ok(metadata)
}

fn hash_regular_file_handle(
    file: &mut std::fs::File,
    maximum: u64,
    role: RegularFileRole,
    digest: &mut Sha256,
) -> std::io::Result<u64> {
    use std::io::Read;

    let before = validate_regular_file_handle(file, maximum, role)?;
    let before_modified = before.modified()?;
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let remaining = maximum.saturating_sub(total).saturating_add(1);
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| std::io::Error::other("bounded file read does not fit memory"))?;
        let read = file.read(&mut buffer[..read_limit])?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("bounded file byte count overflow"))?;
        if total > maximum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("opened helper/toolchain file exceeds {maximum} bytes"),
            ));
        }
        digest.update(&buffer[..read]);
    }
    let after = validate_regular_file_handle(file, maximum, role)?;
    if after.len() != before.len() || total != before.len() || after.modified()? != before_modified
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "helper/toolchain file changed while it was hashed",
        ));
    }
    Ok(total)
}

fn hash_regular_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "regular file has no parent",
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "regular file name is not UTF-8",
            )
        })?;
    let directory = DirectoryCapability::open_read_only_source(parent)?;
    let mut file = directory.open_file_read(name)?;
    let mut digest = Sha256::new();
    hash_regular_file_handle(
        &mut file,
        MAX_HELPER_BINARY_BYTES,
        RegularFileRole::PrivateHelperBinary,
        &mut digest,
    )?;
    Ok(digest.finalize().into())
}

fn open_directory_chain(path: &Path) -> std::io::Result<DirectoryCapability> {
    use std::path::Component;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "helper cache path must be absolute",
        ));
    }
    let mut current = PathBuf::new();
    let mut directory: Option<DirectoryCapability> = None;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                directory = Some(DirectoryCapability::open(&current)?);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "helper cache path may not contain parent components",
                ));
            }
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "helper cache path component is not UTF-8",
                    )
                })?;
                let parent = directory.as_ref().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "absolute helper cache path has no root directory",
                    )
                })?;
                let child = match parent.open_dir(name) {
                    Ok(child) => child,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        parent.create_private_dir(name)?
                    }
                    Err(error) => return Err(error),
                };
                current.push(name);
                directory = Some(child);
            }
        }
    }
    directory.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "absolute helper cache path has no root directory",
        )
    })
}

fn open_private_helper_cache(path: &Path) -> std::io::Result<DirectoryCapability> {
    let parent_path = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "cache root has no parent")
    })?;
    let parent = open_directory_chain(parent_path)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache root name is not UTF-8",
            )
        })?;
    match parent.open_dir(name) {
        Ok(cache) => {
            cache.restrict_private()?;
            cache.validate_private()?;
            Ok(cache)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            parent.create_private_dir(name)
        }
        Err(error) => Err(error),
    }
}

fn clean_abandoned_helper_staging(cache_root: &DirectoryCapability) -> std::io::Result<()> {
    let entries = cache_root.entries(MAX_HELPER_CACHE_ROOT_ENTRIES)?;
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "helper cache contains a non-UTF-8 entry",
            ));
        };
        if name.starts_with("go-oracle-build-") {
            if entry.kind != EntryKind::Directory {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "helper cache staging name is not a regular directory",
                ));
            }
            cache_root.remove_dir_all(name, MAX_HELPER_CACHE_ENTRY_ENTRIES)?;
        }
    }
    Ok(())
}

fn remove_helper_cache_entry(cache_root: &DirectoryCapability, name: &str) -> std::io::Result<()> {
    let entries = cache_root.entries(MAX_HELPER_CACHE_ROOT_ENTRIES)?;
    let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
        return Ok(());
    };
    match entry.kind {
        EntryKind::Directory => cache_root.remove_dir_all(name, MAX_HELPER_CACHE_ENTRY_ENTRIES),
        EntryKind::File | EntryKind::Link | EntryKind::Special => cache_root.remove_file(name),
    }
}

fn helper_cache_key(source_identity: &[u8; 32], toolchain_identity: &[u8; 32]) -> String {
    let mut key_digest = Sha256::new();
    key_digest.update(b"nudox.go-oracle-compiled-helper.v2\0");
    key_digest.update(source_identity);
    key_digest.update(toolchain_identity);
    digest_hex(&key_digest.finalize())
}

fn helper_source_identity() -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"nudox.go-oracle-helper-source.v2\0");
    for (name, contents) in GO_ORACLE_SOURCE_FILES {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((contents.len() as u64).to_be_bytes());
        digest.update(contents);
    }
    digest.finalize().into()
}

fn hash_toolchain_identity(executable: &Path, goroot: &Path) -> std::io::Result<[u8; 32]> {
    use std::io::ErrorKind;

    struct WalkBudget {
        entries: usize,
        bytes: u64,
    }

    fn path_bytes(path: &Path) -> std::io::Result<&[u8]> {
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.len() > MAX_GO_TOOLCHAIN_PATH_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "Go toolchain path exceeds the identity limit",
            ));
        }
        Ok(bytes)
    }

    fn debian_shared_go_root(goroot: &Path) -> Option<(PathBuf, String)> {
        let root_name = goroot.file_name()?.to_str()?;
        let version = root_name.strip_prefix("go-")?;
        if version.split('.').count() < 2
            || version.split('.').any(|component| {
                component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return None;
        }
        let lib = goroot.parent()?;
        if lib.file_name()? != "lib" {
            return None;
        }
        let prefix = lib.parent()?;
        Some((prefix.join("share").join(root_name), root_name.to_owned()))
    }

    fn approved_debian_shared_link(logical: &Path, root_name: &str) -> Option<(PathBuf, PathBuf)> {
        let relative = logical.strip_prefix("GOROOT").ok()?;
        let approved = ["api", "lib", "misc", "src", "test", "pkg/include"]
            .iter()
            .any(|path| relative == Path::new(path));
        if !approved {
            return None;
        }
        let parent_depth = relative
            .parent()
            .map_or(0, |parent| parent.components().count());
        let target = format!(
            "{}share/{root_name}/{}",
            "../".repeat(parent_depth + 2),
            relative.to_str()?
        );
        Some((PathBuf::from(target), relative.to_path_buf()))
    }

    fn validate_shared_directory(
        root: &DirectoryCapability,
        relative: &Path,
    ) -> std::io::Result<()> {
        let mut directory = root.clone();
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    "Debian Go shared-root link has an invalid destination",
                ));
            };
            let name = name.to_str().ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "Debian Go shared-root link has a non-UTF-8 destination",
                )
            })?;
            directory = directory.open_dir(name)?;
        }
        Ok(())
    }

    fn add_open_file(
        file: &mut std::fs::File,
        logical: &Path,
        maximum: u64,
        role: RegularFileRole,
        budget: &mut WalkBudget,
        digest: &mut Sha256,
    ) -> std::io::Result<()> {
        let metadata = validate_regular_file_handle(file, maximum, role)?;
        budget.bytes = budget
            .bytes
            .checked_add(metadata.len())
            .ok_or_else(|| std::io::Error::other("Go toolchain byte count overflow"))?;
        if budget.bytes > MAX_GO_TOOLCHAIN_TOTAL_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "Go toolchain exceeds the total identity byte limit",
            ));
        }
        let path = path_bytes(logical)?;
        digest.update(b"file\0");
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update(metadata.len().to_be_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update(metadata.permissions().mode().to_be_bytes());
        }
        hash_regular_file_handle(file, maximum, role, digest)?;
        Ok(())
    }

    fn walk(
        directory: &DirectoryCapability,
        logical: &Path,
        depth: usize,
        shared_root_path: Option<&Path>,
        shared_root_name: Option<&str>,
        shared_root: &mut Option<DirectoryCapability>,
        shared_root_hashed: &mut bool,
        allow_debian_shared_links: bool,
        budget: &mut WalkBudget,
        digest: &mut Sha256,
    ) -> std::io::Result<()> {
        if depth > MAX_GO_TOOLCHAIN_DEPTH {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "Go toolchain exceeds the identity directory depth limit",
            ));
        }
        let logical_bytes = path_bytes(logical)?;
        digest.update(b"directory\0");
        digest.update((logical_bytes.len() as u64).to_be_bytes());
        digest.update(logical_bytes);
        let entries = directory.entries(MAX_GO_TOOLCHAIN_ENTRIES)?;
        for entry in entries {
            budget.entries = budget
                .entries
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("Go toolchain entry count overflow"))?;
            if budget.entries > MAX_GO_TOOLCHAIN_ENTRIES {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    "Go toolchain exceeds the identity entry limit",
                ));
            }
            let name = entry.name.to_str().ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "Go toolchain contains a non-UTF-8 entry name",
                )
            })?;
            let child_logical = logical.join(name);
            let child_bytes = path_bytes(&child_logical)?;
            match entry.kind {
                EntryKind::Directory => {
                    let child = directory.open_dir(name)?;
                    walk(
                        &child,
                        &child_logical,
                        depth + 1,
                        shared_root_path,
                        shared_root_name,
                        shared_root,
                        shared_root_hashed,
                        allow_debian_shared_links,
                        budget,
                        digest,
                    )?;
                }
                EntryKind::File => {
                    let mut file = directory.open_file_read(name)?;
                    add_open_file(
                        &mut file,
                        &child_logical,
                        MAX_GO_TOOLCHAIN_FILE_BYTES,
                        RegularFileRole::ExternalToolchainMember,
                        budget,
                        digest,
                    )?;
                }
                EntryKind::Link => {
                    let Some(root_path) = shared_root_path.filter(|_| allow_debian_shared_links)
                    else {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!("Go toolchain contains a symbolic link at {child_logical:?}"),
                        ));
                    };
                    let Some(root_name) = shared_root_name else {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "Debian Go shared-root identity is incomplete",
                        ));
                    };
                    let Some((expected_target, relative_target)) =
                        approved_debian_shared_link(&child_logical, root_name)
                    else {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "Go toolchain contains an unapproved symbolic link at {child_logical:?}"
                            ),
                        ));
                    };
                    let observed_target =
                        directory.read_link_target(name, MAX_GO_TOOLCHAIN_PATH_BYTES)?;
                    if observed_target.as_os_str().as_encoded_bytes()
                        != expected_target.as_os_str().as_encoded_bytes()
                    {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "Go toolchain symbolic link has an unexpected target at {child_logical:?}"
                            ),
                        ));
                    }
                    if observed_target.as_os_str().as_encoded_bytes().len()
                        > MAX_GO_TOOLCHAIN_PATH_BYTES
                    {
                        return Err(std::io::Error::new(
                            ErrorKind::InvalidData,
                            "Go toolchain symbolic link target exceeds the identity limit",
                        ));
                    }
                    if shared_root.is_none() {
                        *shared_root = Some(DirectoryCapability::open_read_only_source(root_path)?);
                    }
                    let paired_root = shared_root.as_ref().expect("paired root was opened");
                    validate_shared_directory(paired_root, &relative_target)?;
                    if !*shared_root_hashed {
                        let shared_logical = Path::new("GOROOT-shared").join(root_name);
                        let mut nested_shared_root = None;
                        let mut nested_shared_root_hashed = false;
                        walk(
                            paired_root,
                            &shared_logical,
                            0,
                            None,
                            None,
                            &mut nested_shared_root,
                            &mut nested_shared_root_hashed,
                            false,
                            budget,
                            digest,
                        )?;
                        *shared_root_hashed = true;
                    }
                    digest.update(b"debian-go-shared-link\0");
                    digest.update((child_bytes.len() as u64).to_be_bytes());
                    digest.update(child_bytes);
                    let target_bytes = observed_target.as_os_str().as_encoded_bytes();
                    digest.update((target_bytes.len() as u64).to_be_bytes());
                    digest.update(target_bytes);
                }
                EntryKind::Special => {
                    return Err(std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("Go toolchain contains a special file at {child_logical:?}"),
                    ));
                }
            }
            digest.update((child_bytes.len() as u64).to_be_bytes());
            digest.update(child_bytes);
        }
        Ok(())
    }

    let goroot_cap = DirectoryCapability::open_read_only_source(goroot)?;
    let shared_layout = debian_shared_go_root(goroot);
    let (shared_root_path, shared_root_name) =
        shared_layout.as_ref().map_or((None, None), |(path, name)| {
            (Some(path.as_path()), Some(name.as_str()))
        });
    let mut shared_root = None;
    let mut shared_root_hashed = false;
    let executable_parent = executable.parent().ok_or_else(|| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            "Go executable has no parent directory",
        )
    })?;
    let executable_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(ErrorKind::InvalidInput, "Go executable name is not UTF-8")
        })?;
    let executable_parent = DirectoryCapability::open_read_only_source(executable_parent)?;
    let mut executable_file = executable_parent.open_file_read(executable_name)?;
    let mut digest = Sha256::new();
    digest.update(b"nudox.go-toolchain-identity.v3\0");
    let mut budget = WalkBudget {
        entries: 0,
        bytes: 0,
    };
    add_open_file(
        &mut executable_file,
        Path::new("selected-go-executable"),
        MAX_GO_TOOLCHAIN_FILE_BYTES,
        RegularFileRole::ExternalToolchainExecutable,
        &mut budget,
        &mut digest,
    )?;
    walk(
        &goroot_cap,
        Path::new("GOROOT"),
        0,
        shared_root_path,
        shared_root_name,
        &mut shared_root,
        &mut shared_root_hashed,
        true,
        &mut budget,
        &mut digest,
    )?;
    Ok(digest.finalize().into())
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn update_path_digest(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    digest.update(bytes.len().to_be_bytes());
    digest.update(bytes);
}

struct BoundedBytes {
    bytes: Vec<u8>,
    exceeded: Option<(&'static str, usize)>,
    error: Option<(&'static str, std::io::Error)>,
}

fn read_bounded(
    mut reader: impl std::io::Read,
    limit: usize,
    stream: &'static str,
    sender: std::sync::mpsc::Sender<(&'static str, usize)>,
) -> BoundedBytes {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => {
                return BoundedBytes {
                    bytes,
                    exceeded: None,
                    error: None,
                };
            }
            Ok(count) if bytes.len().saturating_add(count) > limit => {
                let observed = bytes.len().saturating_add(count);
                if sender.send((stream, observed)).is_err() {
                    // The returned `exceeded` fact remains authoritative if the
                    // coordinator has already stopped receiving notifications.
                }
                return BoundedBytes {
                    bytes,
                    exceeded: Some((stream, observed)),
                    error: None,
                };
            }
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return BoundedBytes {
                    bytes,
                    exceeded: None,
                    error: Some((stream, error)),
                };
            }
        }
    }
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let killed_group = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|group| {
                rustix::process::kill_process_group(group, rustix::process::Signal::KILL).is_ok()
            });
        if !killed_group {
            drop(child.kill());
        }
    }
    #[cfg(not(unix))]
    {
        drop(child.kill());
    }
    drop(child.wait());
}

fn transcript_prefix(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(DIAGNOSTIC_PREFIX_LIMIT)]).into_owned()
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(DIAGNOSTIC_TAIL_LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod read_tests {
    use super::{
        DirectoryCapability, GoCgoPolicy, GoOracle, GoOracleChildEnvironment,
        GoOracleConfiguration, GoOracleConfigurationError, GoOracleInvocationModeV1,
        GoPackageAuthorityWitness, GoWorkWitness, MAX_GO_TOOLCHAIN_DEPTH,
        MAX_GO_TOOLCHAIN_FILE_BYTES, MAX_HELPER_MANIFEST_BYTES, cache_entry_is_valid, digest_hex,
        hash_regular_file, hash_toolchain_identity, read_bounded,
    };
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct FaultyReader {
        interrupted: bool,
    }

    impl Read for FaultyReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Err(io::Error::other("injected pipe fault"))
            }
        }
    }

    #[test]
    fn interrupted_read_retries_then_preserves_pipe_fault() {
        let (sender, _receiver) = std::sync::mpsc::channel();
        let result = read_bounded(FaultyReader { interrupted: false }, 8, "stdout", sender);
        assert!(result.bytes.is_empty());
        assert_eq!(result.exceeded, None);
        let (stream, error) = result.error.expect("pipe fault retained");
        assert_eq!(stream, "stdout");
        assert_eq!(error.to_string(), "injected pipe fault");
    }

    #[test]
    fn explicit_go_toolchain_rejects_relative_executable_before_child_work() {
        let error = GoOracleConfiguration::go_toolchain(PathBuf::from("go"))
            .expect_err("relative Go toolchain must not enter authority configuration");
        assert!(matches!(
            error,
            GoOracleConfigurationError::RelativeExecutable { executable }
                if executable == PathBuf::from("go")
        ));
    }

    #[test]
    fn portable_invocation_snapshot_ignores_tool_path_and_binds_mode() {
        let first = GoOracle::default().with_configuration(
            GoOracleConfiguration::go_toolchain(PathBuf::from("/host-a/bin/go"))
                .expect("absolute Go executable"),
        );
        let relocated = GoOracle::default().with_configuration(
            GoOracleConfiguration::go_toolchain(PathBuf::from("/host-b/sdk/go"))
                .expect("absolute Go executable"),
        );
        let binary = GoOracle::default().with_configuration(
            GoOracleConfiguration::oracle_binary(PathBuf::from("/host-c/bin/go-oracle"))
                .expect("absolute oracle executable"),
        );

        let first_options = first.portable_invocation_options();
        let relocated_options = relocated.portable_invocation_options();
        let binary_options = binary.portable_invocation_options();
        assert_eq!(first_options, relocated_options);
        assert_eq!(first_options.mode(), GoOracleInvocationModeV1::GoToolchain);
        assert!(first_options.helper_source_identity().is_some());
        assert_ne!(first_options, binary_options);
        assert_eq!(
            binary_options.mode(),
            GoOracleInvocationModeV1::OracleBinary
        );
        assert!(binary_options.helper_source_identity().is_none());
        assert!(first.uses_toolchain_executable(Path::new("/host-a/bin/go")));
        assert!(!first.uses_toolchain_executable(Path::new("/host-b/bin/go")));
    }

    #[test]
    fn go_workspace_witness_tracks_nearest_file_contents_and_selection() {
        let root = std::env::temp_dir().join(format!(
            "go-work-witness-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let package = root.join("module");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&package).expect("module fixture directory");
        let workspace = root.join("go.work");
        std::fs::write(&workspace, b"go 1.23\nuse ./module\n").expect("workspace fixture");

        let captured = GoWorkWitness::capture(&package).expect("capture nearest workspace");
        let canonical_workspace = workspace.canonicalize().expect("canonical workspace");
        assert_eq!(captured.path(), Some(canonical_workspace.as_path()));
        assert!(captured.content_digest().is_some());
        assert!(
            captured
                .matches_current(&package)
                .expect("same workspace remains current")
        );

        std::fs::write(&workspace, b"go 1.23\nuse ./module\n\n").expect("change workspace");
        assert!(
            !captured
                .matches_current(&package)
                .expect("re-read workspace")
        );

        std::fs::remove_file(&workspace).expect("remove workspace");
        let disabled = GoWorkWitness::capture(&package).expect("capture workspace absence");
        assert!(matches!(&disabled, GoWorkWitness::Disabled));
        assert_ne!(captured.identity(), disabled.identity());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn go_module_local_replace_witness_tracks_target_changes() {
        let root = std::env::temp_dir().join(format!(
            "go-module-replace-witness-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let module = root.join("module");
        let shared = root.join("shared");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&module).expect("module fixture directory");
        std::fs::create_dir_all(&shared).expect("replacement fixture directory");
        std::fs::write(
            module.join("go.mod"),
            "module example.com/root\n\ngo 1.23\n\nreplace example.com/shared => ../shared\n",
        )
        .expect("module manifest");
        std::fs::write(
            shared.join("go.mod"),
            "module example.com/shared\n\ngo 1.23\n",
        )
        .expect("replacement module manifest");
        std::fs::write(
            shared.join("shared.go"),
            "package shared\nconst Value = 1\n",
        )
        .expect("replacement package source");

        let witness = GoPackageAuthorityWitness::capture(&module)
            .expect("capture package-local replacement closure");
        assert!(witness.is_complete());
        assert!(witness.requires_local_execution());
        assert!(
            witness
                .local_only_reasons()
                .iter()
                .any(|reason| { reason.code() == "go.module_replace.local.v1" })
        );
        assert!(
            witness
                .matches_current(&module)
                .expect("same closure remains current")
        );

        std::fs::write(
            shared.join("shared.go"),
            "package shared\nconst Value = 2\n",
        )
        .expect("change replacement source");
        assert!(
            !witness
                .matches_current(&module)
                .expect("replacement tree can be revalidated")
        );
        let updated = GoPackageAuthorityWitness::capture(&module)
            .expect("capture changed replacement closure");
        assert_ne!(witness.identity(), updated.identity());

        std::fs::remove_dir_all(&shared).expect("remove local replacement target");
        assert!(
            !updated
                .matches_current(&module)
                .expect("deleted replacement target can be revalidated")
        );

        let missing_module = root.join("missing-module");
        let appearing_target = root.join("appearing-target");
        std::fs::create_dir_all(&missing_module).expect("second module fixture directory");
        std::fs::write(
            missing_module.join("go.mod"),
            "module example.com/missing\n\ngo 1.23\n\nreplace example.com/appearing => ../appearing-target\n",
        )
        .expect("second module manifest");
        let missing = GoPackageAuthorityWitness::capture(&missing_module)
            .expect("capture absent local replacement target");
        assert!(
            missing
                .local_only_reasons()
                .iter()
                .any(|reason| { reason.code() == "go.authority_witness.unsupported_target.v1" })
        );
        std::fs::create_dir_all(&appearing_target).expect("create replacement target");
        std::fs::write(
            appearing_target.join("go.mod"),
            "module example.com/appearing\n\ngo 1.23\n",
        )
        .expect("appearing replacement module manifest");
        std::fs::write(
            appearing_target.join("appearing.go"),
            "package appearing\nconst Value = 1\n",
        )
        .expect("appearing replacement source");
        assert!(
            !missing
                .matches_current(&missing_module)
                .expect("appearing replacement target can be revalidated")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn go_local_replacement_witness_tracks_nested_symlink_target_drift() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "go-replace-symlink-witness-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let module = root.join("module");
        let shared = root.join("shared");
        let first = root.join("first");
        let second = root.join("second");
        let _ = std::fs::remove_dir_all(&root);
        for directory in [&module, &shared, &first, &second] {
            std::fs::create_dir_all(directory).expect("symlink fixture directory");
        }
        std::fs::write(
            module.join("go.mod"),
            "module example.com/root\n\ngo 1.23\n\nreplace example.com/shared => ../shared\n",
        )
        .expect("module manifest");
        std::fs::write(
            shared.join("go.mod"),
            "module example.com/shared\n\ngo 1.23\n",
        )
        .expect("replacement module manifest");
        std::fs::write(first.join("value.go"), "package value\nconst Value = 1\n")
            .expect("first symlink target");
        std::fs::write(second.join("value.go"), "package value\nconst Value = 2\n")
            .expect("second symlink target");
        symlink(&first, shared.join("link")).expect("create directory symlink");

        let witness = GoPackageAuthorityWitness::capture(&module)
            .expect("capture replacement containing a directory symlink");
        assert!(witness.requires_local_execution());
        assert!(!witness.is_complete());
        assert!(
            witness
                .matches_current(&module)
                .expect("symlink remains current")
        );

        std::fs::remove_file(shared.join("link")).expect("remove directory symlink");
        symlink(&second, shared.join("link")).expect("retarget directory symlink");
        assert!(
            !witness
                .matches_current(&module)
                .expect("directory symlink target drift is revalidated")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn go_workspace_use_replace_and_sum_changes_are_witnessed() {
        let root = std::env::temp_dir().join(format!(
            "go-workspace-closure-witness-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let module = root.join("module");
        let other = root.join("other");
        let replacement = root.join("replacement");
        let _ = std::fs::remove_dir_all(&root);
        for directory in [&module, &other, &replacement] {
            std::fs::create_dir_all(directory).expect("workspace module directory");
        }
        std::fs::write(
            root.join("go.work"),
            "go 1.23\n\nuse (\n ./module\n ./other\n)\n\nreplace example.com/replacement => ./replacement\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            module.join("go.mod"),
            "module example.com/module\n\ngo 1.23\n",
        )
        .expect("selected module manifest");
        std::fs::write(
            other.join("go.mod"),
            "module example.com/other\n\ngo 1.23\n",
        )
        .expect("workspace use module manifest");
        std::fs::write(other.join("other.go"), "package other\nconst Value = 1\n")
            .expect("workspace use package source");
        std::fs::write(
            replacement.join("go.mod"),
            "module example.com/replacement\n\ngo 1.23\n",
        )
        .expect("workspace replacement module manifest");
        std::fs::write(
            replacement.join("replacement.go"),
            "package replacement\nconst Value = 1\n",
        )
        .expect("workspace replacement source");
        let workspace_sum = root.join("go.work.sum");
        std::fs::write(&workspace_sum, "example.com/sum v1.2.3 h1:one\n")
            .expect("workspace checksum manifest");

        let witness = GoPackageAuthorityWitness::capture(&module)
            .expect("capture workspace module and filesystem closure");
        assert!(witness.is_complete());
        assert!(witness.requires_local_execution());
        let codes = witness
            .local_only_reasons()
            .iter()
            .map(|reason| reason.code())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"go.workspace.local.v1"));
        assert!(codes.contains(&"go.workspace_use.local.v1"));
        assert!(codes.contains(&"go.workspace_replace.local.v1"));
        assert!(
            witness
                .matches_current(&module)
                .expect("same closure remains current")
        );

        std::fs::write(
            replacement.join("replacement.go"),
            "package replacement\nconst Value = 2\n",
        )
        .expect("change workspace replacement source");
        assert!(
            !witness
                .matches_current(&module)
                .expect("workspace replacement target can be revalidated")
        );

        let with_workspace_replace_change = GoPackageAuthorityWitness::capture(&module)
            .expect("capture updated workspace replacement target");
        std::fs::write(other.join("other.go"), "package other\nconst Value = 2\n")
            .expect("change workspace use source");
        assert!(
            !with_workspace_replace_change
                .matches_current(&module)
                .expect("workspace use target can be revalidated")
        );

        let with_target_change = GoPackageAuthorityWitness::capture(&module)
            .expect("capture updated workspace use target");
        std::fs::write(&workspace_sum, "example.com/sum v1.2.3 h1:two\n")
            .expect("change workspace checksum manifest");
        assert!(
            !with_target_change
                .matches_current(&module)
                .expect("workspace checksum can be revalidated")
        );

        std::fs::remove_file(&workspace_sum).expect("remove workspace checksum manifest");
        let absent_sum = GoPackageAuthorityWitness::capture(&module)
            .expect("capture negative workspace checksum observation");
        std::fs::write(&workspace_sum, "example.com/sum v1.2.3 h1:three\n")
            .expect("create workspace checksum manifest");
        assert!(
            !absent_sum
                .matches_current(&module)
                .expect("negative workspace checksum observation can be revalidated")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn go_environment_keeps_build_cache_leaf_uncreated_for_inspection() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("go-child-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let go_dir = root.join("go/bin");
        let goroot = root.join("go/root");
        let module_cache = root.join("modules");
        std::fs::create_dir_all(&go_dir).expect("Go executable directory");
        std::fs::create_dir_all(&goroot).expect("GOROOT fixture");
        std::fs::create_dir_all(&module_cache).expect("module cache fixture");
        let go = go_dir.join("go");
        std::fs::write(&go, b"fixture").expect("Go executable fixture");
        #[cfg(unix)]
        std::fs::set_permissions(&go, std::fs::Permissions::from_mode(0o700))
            .expect("mark Go executable fixture executable");
        let build_cache = root.join("native-work/go-oracle-cache");
        let environment =
            GoOracleChildEnvironment::new(go, goroot, module_cache, build_cache.clone())
                .expect("cache leaf is an inspection-safe typed path");
        assert_eq!(environment.cgo_policy(), GoCgoPolicy::Disabled);
        assert!(!build_cache.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn retained_go_environment_detects_toolchain_mutation_at_package_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let go_dir = root.path().join("go/bin");
        let goroot = root.path().join("go/root");
        let module_cache = root.path().join("modules");
        std::fs::create_dir_all(&go_dir)?;
        std::fs::create_dir_all(&goroot)?;
        std::fs::create_dir_all(&module_cache)?;
        let go = go_dir.join("go");
        std::fs::write(&go, b"selected executable")?;
        #[cfg(unix)]
        std::fs::set_permissions(&go, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(goroot.join("VERSION"), b"go1.27.1\n")?;
        let environment = GoOracleChildEnvironment::new(
            go,
            goroot.clone(),
            module_cache,
            root.path().join("cache"),
        )?;
        environment.revalidate_toolchain()?;
        std::fs::write(goroot.join("VERSION"), b"go1.27.2\n")?;
        assert!(matches!(
            environment.revalidate_toolchain(),
            Err(super::OracleError::GoToolchainIdentityChanged)
        ));
        Ok(())
    }

    #[test]
    fn helper_cache_requires_an_exact_bounded_private_regular_manifest() -> io::Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let entry = root.path().join("entry");
        std::fs::create_dir(&entry)?;
        #[cfg(unix)]
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o700))?;
        let binary = entry.join("oracle");
        std::fs::write(&binary, b"compiled helper bytes")?;
        #[cfg(unix)]
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        let binary_hash = hash_regular_file(&binary)?;
        let manifest = entry.join("manifest.txt");
        let prefix = format!(
            "schema=1\nsource={}\ntoolchain={}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        let valid = format!("{prefix}binary={}\n", digest_hex(&binary_hash));
        std::fs::write(&manifest, &valid)?;
        let root_cap = DirectoryCapability::open(root.path())?;
        assert!(cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));

        std::fs::write(
            &manifest,
            format!(
                "{prefix}binary={}\nunexpected=yes\n",
                digest_hex(&binary_hash)
            ),
        )?;
        assert!(!cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));
        std::fs::write(
            &manifest,
            format!("{prefix}binary={}\n\n", digest_hex(&binary_hash)),
        )?;
        assert!(!cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));
        std::fs::write(
            &manifest,
            vec![
                b'x';
                usize::try_from(MAX_HELPER_MANIFEST_BYTES + 1).expect("small manifest bound")
            ],
        )?;
        assert!(!cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));
        std::fs::write(&manifest, &valid)?;

        #[cfg(unix)]
        {
            let outside = root.path().join("outside-hardlink");
            std::fs::hard_link(&binary, &outside)?;
            assert!(!cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));
            std::fs::remove_file(&outside)?;

            let target = root.path().join("manifest-target.txt");
            std::fs::write(&target, &valid)?;
            std::fs::remove_file(&manifest)?;
            std::os::unix::fs::symlink(&target, &manifest)?;
            assert!(!cache_entry_is_valid(&root_cap, "entry", "oracle", &prefix));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn go_toolchain_identity_is_bounded_and_refuses_links_and_specials_but_accepts_external_hardlinks()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        fn fixture(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
            let executable = root.join("bin/go");
            let goroot = root.join("goroot");
            std::fs::create_dir_all(executable.parent().expect("bin parent"))?;
            std::fs::create_dir_all(&goroot)?;
            std::fs::write(&executable, b"selected go executable")?;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
            Ok((executable, goroot))
        }

        let root = tempfile::tempdir()?;
        let (executable, goroot) = fixture(root.path())?;
        std::fs::write(goroot.join("VERSION"), "go1.27.1\n")?;
        let first = hash_toolchain_identity(&executable, &goroot)?;
        let second = hash_toolchain_identity(&executable, &goroot)?;
        assert_eq!(first, second);
        std::fs::write(goroot.join("VERSION"), "go1.27.2\n")?;
        assert_ne!(first, hash_toolchain_identity(&executable, &goroot)?);

        let symlink_root = root.path().join("symlink-root");
        let (symlink_go, symlink_goroot) = fixture(&symlink_root)?;
        std::os::unix::fs::symlink("/etc/passwd", symlink_goroot.join("outside"))?;
        assert!(hash_toolchain_identity(&symlink_go, &symlink_goroot).is_err());

        let cycle_root = root.path().join("cycle-root");
        let (cycle_go, cycle_goroot) = fixture(&cycle_root)?;
        std::os::unix::fs::symlink("cycle", cycle_goroot.join("cycle"))?;
        assert!(hash_toolchain_identity(&cycle_go, &cycle_goroot).is_err());

        let hardlink_root = root.path().join("hardlink-root");
        let (hardlink_go, hardlink_goroot) = fixture(&hardlink_root)?;
        std::fs::write(hardlink_goroot.join("one"), b"shared")?;
        std::fs::hard_link(hardlink_goroot.join("one"), hardlink_goroot.join("two"))?;
        let hardlinked_identity = hash_toolchain_identity(&hardlink_go, &hardlink_goroot)?;
        assert_eq!(std::fs::metadata(hardlink_goroot.join("one"))?.nlink(), 2);
        std::fs::write(hardlink_goroot.join("two"), b"changed shared contents")?;
        assert_ne!(
            hardlinked_identity,
            hash_toolchain_identity(&hardlink_go, &hardlink_goroot)?,
            "external aliases are accepted only with all logical contents hashed",
        );

        let special_root = root.path().join("special-root");
        let (special_go, special_goroot) = fixture(&special_root)?;
        let _listener = std::os::unix::net::UnixListener::bind(special_goroot.join("socket"))?;
        assert!(hash_toolchain_identity(&special_go, &special_goroot).is_err());

        let oversized_root = root.path().join("oversized-root");
        let (oversized_go, oversized_goroot) = fixture(&oversized_root)?;
        let oversized = std::fs::File::create(oversized_goroot.join("sparse"))?;
        oversized.set_len(MAX_GO_TOOLCHAIN_FILE_BYTES + 1)?;
        drop(oversized);
        assert!(hash_toolchain_identity(&oversized_go, &oversized_goroot).is_err());

        let deep_root = root.path().join("deep-root");
        let (deep_go, deep_goroot) = fixture(&deep_root)?;
        let mut deep = deep_goroot.clone();
        for _ in 0..=MAX_GO_TOOLCHAIN_DEPTH {
            deep.push("d");
            std::fs::create_dir(&deep)?;
        }
        assert!(hash_toolchain_identity(&deep_go, &deep_goroot).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn go_toolchain_identity_accepts_only_the_paired_debian_shared_root_layout()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        fn fixture(root: &Path) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
            let prefix = root.join("usr");
            let goroot = prefix.join("lib/go-1.24");
            let shared = prefix.join("share/go-1.24");
            let executable = goroot.join("bin/go");
            std::fs::create_dir_all(executable.parent().expect("Go bin directory"))?;
            std::fs::create_dir_all(goroot.join("pkg"))?;
            std::fs::create_dir_all(shared.join("pkg/include"))?;
            for name in ["api", "lib", "misc", "src", "test"] {
                std::fs::create_dir_all(shared.join(name))?;
                std::fs::write(shared.join(name).join("marker"), name.as_bytes())?;
                symlink(format!("../../share/go-1.24/{name}"), goroot.join(name))?;
            }
            std::fs::write(shared.join("pkg/include/marker"), b"include")?;
            symlink(
                "../../../share/go-1.24/pkg/include",
                goroot.join("pkg/include"),
            )?;
            std::fs::write(goroot.join("VERSION"), b"go1.24.4\n")?;
            std::fs::write(&executable, b"selected Go executable")?;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
            Ok((executable, goroot, shared))
        }

        let root = tempfile::tempdir()?;
        let (executable, goroot, shared) = fixture(root.path())?;
        let initial = hash_toolchain_identity(&executable, &goroot)?;
        assert_eq!(initial, hash_toolchain_identity(&executable, &goroot)?);
        std::fs::write(shared.join("src/marker"), b"changed source")?;
        assert_ne!(initial, hash_toolchain_identity(&executable, &goroot)?);

        let unexpected_root = tempfile::tempdir()?;
        let (executable, goroot, _) = fixture(unexpected_root.path())?;
        symlink("../../share/go-1.24/src", goroot.join("unexpected"))?;
        assert!(hash_toolchain_identity(&executable, &goroot).is_err());

        let wrong_target_root = tempfile::tempdir()?;
        let (executable, goroot, _) = fixture(wrong_target_root.path())?;
        std::fs::remove_file(goroot.join("api"))?;
        symlink("/etc/passwd", goroot.join("api"))?;
        assert!(hash_toolchain_identity(&executable, &goroot).is_err());

        let mismatched_target_root = tempfile::tempdir()?;
        let (executable, goroot, _) = fixture(mismatched_target_root.path())?;
        std::fs::remove_file(goroot.join("api"))?;
        symlink("../../share/go-1.24/src", goroot.join("api"))?;
        assert!(hash_toolchain_identity(&executable, &goroot).is_err());

        let nested_link_root = tempfile::tempdir()?;
        let (executable, goroot, shared) = fixture(nested_link_root.path())?;
        symlink("cycle", shared.join("src/cycle"))?;
        assert!(hash_toolchain_identity(&executable, &goroot).is_err());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a privately extracted official Debian Go package tree"]
    fn debian_packaged_go_root_identity_is_admitted_and_revalidates_on_private_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture_root = std::env::var_os("NUDOX_GO_DEBIAN_FIXTURE_ROOT")
            .expect("explicit Debian Go fixture root is required for this ignored test");
        let fixture_root = PathBuf::from(fixture_root);
        if !fixture_root.is_absolute() {
            return Err(io::Error::other("Debian Go fixture root must be absolute").into());
        }

        let goroot = fixture_root.join("usr/lib/go-1.24");
        let go = goroot.join("bin/go");
        let module_cache = tempfile::tempdir()?;
        let cache_root = tempfile::tempdir()?;
        let environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            module_cache.path().to_path_buf(),
            cache_root.path().join("go-build"),
        )?;
        let identity = environment.toolchain_identity();
        assert_ne!(
            identity, [0; 32],
            "the package must have a captured identity"
        );

        let configured = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(environment)?;
        configured
            .child_environment
            .as_ref()
            .expect("the exact admitted environment remains attached")
            .revalidate_toolchain()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relocated_go_toolchain_builds_one_offline_cached_embedded_helper()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let go = root.path().join("fake-go");
        let helper_cwd = root.path().join("helper-cwd.txt");
        let build_count = root.path().join("build-count.txt");
        let build_started = root.path().join("build-started.txt");
        let mutate_toolchain = root.path().join("mutate-toolchain.txt");
        let host_path =
            std::env::var_os("PATH").ok_or_else(|| io::Error::other("host PATH is unavailable"))?;
        let chmod = std::env::split_paths(&host_path)
            .map(|directory| directory.join("chmod"))
            .find(|path| path.is_file())
            .ok_or_else(|| io::Error::other("host PATH has no chmod executable"))?;
        let sleeper = std::env::split_paths(&host_path)
            .map(|directory| directory.join("sleep"))
            .find(|path| path.is_file())
            .ok_or_else(|| io::Error::other("host PATH has no sleep executable"))?;
        let program = format!(
            r#"#!/bin/sh
set -eu
[ "$1" = build ]
[ "$2" = -mod=vendor ]
[ "$3" = -trimpath ]
[ "$4" = -buildvcs=false ]
[ "$GOPROXY" = off ]
[ "$GOSUMDB" = off ]
[ "$GOTOOLCHAIN" = local ]
for file in go.mod go.sum main.go docs.go image.go serialize.go vendor/modules.txt vendor/golang.org/x/tools/go/packages/packages.go; do [ -f "$file" ]; done
printf '%s\n' "$PWD" > '{}'
count=0
if [ -f '{}' ]; then IFS= read -r count < '{}'; fi
printf '%s\n' "$((count + 1))" > '{}'
[ "$5" = -o ]
out="$6"
[ "$7" = . ]
printf '%s\n' '#!/bin/sh' 'set -eu' "printf '%s\n' '{{\"schemaVersion\":5}}'" > "$out"
: > '{}'
if [ -f '{}' ]; then printf '%s\n' changed > "$GOROOT/VERSION"; fi
{} 1
{} 700 "$out"
"#,
            helper_cwd.display(),
            build_count.display(),
            build_count.display(),
            build_count.display(),
            build_started.display(),
            mutate_toolchain.display(),
            sleeper.display(),
            chmod.display(),
        );
        std::fs::write(&go, program)?;
        std::fs::set_permissions(&go, std::fs::Permissions::from_mode(0o700))?;

        let goroot = root.path().join("goroot");
        let module_cache = root.path().join("modules");
        let module = root.path().join("target");
        std::fs::create_dir_all(&goroot)?;
        std::fs::write(goroot.join("VERSION"), "go-fixture\n")?;
        std::fs::create_dir_all(&module_cache)?;
        std::fs::create_dir_all(&module)?;
        std::fs::write(
            module.join("go.mod"),
            "module example.com/target\n\ngo 1.23\n",
        )?;
        std::fs::write(
            module.join("target.go"),
            "package target\nconst Value = 1\n",
        )?;

        let child_environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            module_cache,
            root.path().join("native-work/go-oracle-cache"),
        )?;
        let oracle = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(child_environment)?;
        let source_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/legacy/oracle");
        let unavailable_directory =
            source_directory.with_file_name(format!("oracle-unavailable-{}", std::process::id()));
        assert!(!unavailable_directory.exists());
        let key = super::helper_cache_key(
            &super::helper_source_identity(),
            &oracle
                .child_environment
                .as_ref()
                .expect("configured child environment")
                .toolchain_identity(),
        );
        let published_entry = root
            .path()
            .join("native-work/go-oracle-cache/nudox-go-oracle-v1")
            .join(&key);
        std::fs::rename(&source_directory, &unavailable_directory)?;
        let first_oracle = oracle.clone();
        let first_module = module.clone();
        let first_thread = std::thread::spawn(move || first_oracle.run(&first_module));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !build_started.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let build_started_in_time = build_started.exists();
        let absent_while_compiling = !published_entry.exists();
        let cache_root_path = published_entry
            .parent()
            .expect("published helper entry has a cache root");
        let has_unpublished_partial_stage = std::fs::read_dir(cache_root_path)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("go-oracle-build-")
            })
            .any(|entry| {
                let staged_binary = entry.path().join("oracle");
                staged_binary.is_file() && !entry.path().join("manifest.txt").exists()
            });
        let second_oracle = oracle.clone();
        let second_module = module.clone();
        let second_thread = std::thread::spawn(move || second_oracle.run(&second_module));
        let first_result = first_thread
            .join()
            .map_err(|_| io::Error::other("first helper build thread panicked"))?;
        let second_result = second_thread
            .join()
            .map_err(|_| io::Error::other("second helper build thread panicked"))?;
        std::fs::rename(&unavailable_directory, &source_directory)?;
        let first = first_result?;
        let second = second_result?;
        assert!(build_started_in_time, "fake helper build must begin");
        assert!(
            absent_while_compiling,
            "partial staging must never be published"
        );
        assert!(
            has_unpublished_partial_stage,
            "the compiled file must remain in a manifest-free staging directory until publication"
        );
        assert_eq!(first.schema_version, super::Output::REQUIRED_SCHEMA_VERSION);
        assert_eq!(
            second.schema_version,
            super::Output::REQUIRED_SCHEMA_VERSION
        );
        assert_eq!(std::fs::read_to_string(&build_count)?.trim(), "1");

        let cached_binary = published_entry.join("oracle");
        std::fs::write(&cached_binary, b"mutated cached helper")?;
        let after_mutation = oracle.run(&module)?;
        assert_eq!(
            after_mutation.schema_version,
            super::Output::REQUIRED_SCHEMA_VERSION
        );
        assert_eq!(
            std::fs::read_to_string(&build_count)?.trim(),
            "2",
            "a cached binary changed after first use must be rejected and rebuilt"
        );

        // A selected toolchain that changes while the helper is being rebuilt
        // must not publish a cache entry carrying the previously admitted key.
        std::fs::write(&cached_binary, b"force helper rebuild")?;
        std::fs::write(&mutate_toolchain, b"mutate after compiler starts")?;
        assert!(matches!(
            oracle.run(&module),
            Err(super::OracleError::GoToolchainIdentityChanged)
        ));
        assert!(!published_entry.exists());
        assert_eq!(std::fs::read_to_string(&build_count)?.trim(), "3");

        let helper_cwd = PathBuf::from(std::fs::read_to_string(&helper_cwd)?.trim());
        assert!(helper_cwd.is_absolute());
        assert!(!helper_cwd.starts_with(env!("CARGO_MANIFEST_DIR")));
        assert!(
            !helper_cwd.exists(),
            "the private helper source workspace is removed after the one-time build"
        );
        Ok(())
    }

    #[test]
    fn cgo_disabled_keeps_active_files_and_records_excluded_cgo_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "go-cgo-exclusion-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let go = std::env::var_os("COMPILER_GO_COMPILER")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::other("COMPILER_GO_COMPILER is required for Go authority tests")
            })?
            .canonicalize()?;
        let (goroot, _) = explicit_go_roots(&go)?;
        let module_cache = root.join("empty-module-cache");
        std::fs::create_dir(&module_cache)?;
        let environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            module_cache.clone(),
            root.join("native-work/go-oracle-cache"),
        )?;
        let oracle = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(environment)?;
        std::fs::write(
            root.join("go.mod"),
            "module example.com/cgo-selection\n\ngo 1.23\n",
        )?;
        std::fs::write(
            root.join("active.go"),
            "package cgo_selection\nimport \"fmt\"\nfunc Active() string { return fmt.Sprint(1) }\n",
        )?;
        std::fs::write(
            root.join("cgo.go"),
            "package cgo_selection\nimport \"C\"\nfunc CgoOnly() C.int { return 1 }\n",
        )?;
        let output = oracle.run(&root)?;
        let package = output
            .packages
            .iter()
            .find(|package| package.import_path == "example.com/cgo-selection")
            .ok_or_else(|| io::Error::other("Go oracle omitted selected package"))?;
        assert!(package.files.iter().any(|file| file.ends_with("active.go")));
        assert!(package.files.iter().all(|file| !file.ends_with("cgo.go")));
        assert!(
            package
                .cgo_excluded_files
                .iter()
                .any(|file| file.ends_with("cgo.go"))
        );
        assert!(package.build_constraints.iter().any(|file| {
            file.file.ends_with("cgo.go") && file.excluded_reason == "cgo-disabled-import-C"
        }));
        assert!(!module_cache.join("golang.org/x/tools@v0.30.0").exists());
        assert!(
            !module_cache
                .join("cache/download/golang.org/x/tools")
                .exists()
        );
        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }

    #[test]
    fn go_toolchain_accepts_the_selected_non_cgo_files_in_root_and_dependency()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "go-cgo-authority-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;

        let go = std::env::var_os("COMPILER_GO_COMPILER")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::other("COMPILER_GO_COMPILER is required for Go authority tests")
            })?
            .canonicalize()?;
        let (goroot, module_cache) = explicit_go_roots(&go)?;
        let child_environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            module_cache,
            root.join("native-work/go-oracle-cache"),
        )?;
        let oracle = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(child_environment)?;

        let root_module = root.join("root-module");
        std::fs::create_dir_all(&root_module)?;
        std::fs::write(
            root_module.join("go.mod"),
            "module example.com/rootcgo\n\ngo 1.23\n",
        )?;
        std::fs::write(
            root_module.join("root.go"),
            "package rootcgo\nconst Value = 1\n",
        )?;
        std::fs::write(
            root_module.join("cgo.go"),
            r#"package rootcgo
import "C"
func CgoOnly() C.int { return 1 }
"#,
        )?;
        assert_go_authority_succeeds(&oracle, &root_module, &root_module.join("root.go"))?;

        let dependency_module = root.join("dependency-module");
        let dependency_package = dependency_module.join("dep");
        std::fs::create_dir_all(&dependency_package)?;
        std::fs::write(
            dependency_module.join("go.mod"),
            "module example.com/dependencycgo\n\ngo 1.23\n",
        )?;
        std::fs::write(
            dependency_module.join("root.go"),
            r#"package dependencycgo
import "example.com/dependencycgo/dep"
const Value = dep.Value
"#,
        )?;
        std::fs::write(
            dependency_package.join("dep.go"),
            "package dep\nconst Value = 1\n",
        )?;
        std::fs::write(
            dependency_package.join("cgo.go"),
            r#"package dep
import "C"
func CgoOnly() C.int { return 1 }
"#,
        )?;
        assert_go_authority_succeeds(
            &oracle,
            &dependency_module,
            &dependency_module.join("root.go"),
        )?;

        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the retained origin-bound github.com/gorilla/mux v1.8.1 fixture"]
    fn real_gorilla_mux_builds_offline_with_the_embedded_helper()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::legacy::GoImage;

        let source_root = std::env::var_os("NUDOX_GO_REAL_GORILLA_SOURCE")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("NUDOX_GO_REAL_GORILLA_SOURCE is required"))?
            .canonicalize()?;
        let module_owner = tempfile::tempdir()?;
        let module = module_owner.path().join("gorilla-mux");
        std::fs::create_dir(&module)?;
        let mut copied = 0usize;
        for entry in std::fs::read_dir(&source_root)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| io::Error::other("Gorilla source filename is not UTF-8"))?;
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            if kind.is_symlink() {
                return Err(io::Error::other("Gorilla fixture contains a symlink").into());
            }
            if kind.is_file() && (name == "go.mod" || name == "go.sum" || name.ends_with(".go")) {
                std::fs::copy(path, module.join(name))?;
                copied += 1;
            }
        }
        assert!(
            copied >= 10,
            "expected the complete root Go package and tests"
        );
        let go_mod = std::fs::read_to_string(module.join("go.mod"))?;
        assert!(go_mod.starts_with("module github.com/gorilla/mux\n"));

        let go = std::env::var_os("COMPILER_GO_COMPILER")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("COMPILER_GO_COMPILER is required"))?
            .canonicalize()?;
        let (goroot, _) = explicit_go_roots(&go)?;
        if let Some(expected_version) = std::env::var_os("NUDOX_GO_EXPECTED_VERSION") {
            let output = Command::new(&go).arg("version").output()?;
            assert!(output.status.success(), "selected Go executable must run");
            let version = String::from_utf8(output.stdout)?;
            assert!(
                version.contains(&expected_version.to_string_lossy().into_owned()),
                "expected selected Go version {expected_version:?}, got {version:?}",
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert!(
                    std::fs::metadata(&go)?.nlink() > 1,
                    "stock Nix toolchain fixture should retain its shared hardlinks",
                );
            }
        }
        let module_cache = module_owner.path().join("empty-module-cache");
        std::fs::create_dir(&module_cache)?;
        let build_cache = module_owner.path().join("private-build-cache");
        let child_environment = GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            module_cache.clone(),
            build_cache.clone(),
        )?;
        let toolchain_identity = child_environment.toolchain_identity();
        let oracle = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(child_environment)?;
        let witness = oracle.package_authority_witness(&module)?;

        // This exact test file failed with authority_runtime/open in the ordinary
        // cold-I/O acceptance. Keep it as the first real helper invocation.
        let bench_source = module.join("bench_test.go");
        let bench_bytes = oracle.authority_image_for_package_with_authority_witness(
            &bench_source,
            &module,
            &witness,
        )?;
        let bench_image = GoImage::open(&bench_bytes)?;
        let bench_packages = bench_image.packages().collect::<Result<Vec<_>, _>>()?;
        assert!(
            bench_packages
                .iter()
                .any(|package| package.import_path == b"github.com/gorilla/mux")
        );

        let mux_source = module.join("mux.go");
        let mux_bytes = oracle.authority_image_for_package_with_authority_witness(
            &mux_source,
            &module,
            &witness,
        )?;
        let mux_image = GoImage::open(&mux_bytes)?;
        let mut saw_new_route = false;
        for method in mux_image.methods() {
            let method = method?;
            if method.name == b"NewRoute" {
                let owner = mux_image.declaration(usize::try_from(method.owner)?)?;
                assert_eq!(owner.name, b"Router");
                assert_eq!(owner.package, b"github.com/gorilla/mux");
                assert!(method.file.ends_with(b"mux.go"));
                assert!(method.bound);
                assert!(method.span.is_some());
                saw_new_route = true;
            }
        }
        assert!(saw_new_route, "the image must carry Router.NewRoute");

        let cache_root = build_cache.join("nudox-go-oracle-v1");
        let entries = std::fs::read_dir(&cache_root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        let directories = entries
            .into_iter()
            .filter(|path| std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()))
            .collect::<Vec<_>>();
        assert_eq!(
            directories.len(),
            1,
            "one helper build must serve both requests"
        );
        let helper_entry = &directories[0];
        let helper = helper_entry.join("oracle");
        let manifest = helper_entry.join("manifest.txt");
        let source_identity = super::helper_source_identity();
        let prefix = format!(
            "schema=1\nsource={}\ntoolchain={}\n",
            digest_hex(&source_identity),
            digest_hex(&toolchain_identity),
        );
        let cache_root_cap = DirectoryCapability::open(&cache_root)?;
        let helper_entry_name = helper_entry
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("helper key is not UTF-8"))?;
        assert!(cache_entry_is_valid(
            &cache_root_cap,
            helper_entry_name,
            "oracle",
            &prefix
        ));
        assert_eq!(
            std::fs::read_to_string(&manifest)?,
            format!(
                "{prefix}binary={}\n",
                digest_hex(&hash_regular_file(&helper)?)
            ),
        );
        assert!(!module_cache.join("golang.org/x/tools@v0.30.0").exists());
        assert!(
            !module_cache
                .join("cache/download/golang.org/x/tools")
                .exists()
        );

        // Prove that the production helper cache is checked on every launch,
        // including after a prior successful real Go invocation.
        std::fs::write(&helper, b"mutated real Go helper")?;
        let refreshed_bytes = oracle.authority_image_for_package_with_authority_witness(
            &mux_source,
            &module,
            &witness,
        )?;
        let refreshed_image = GoImage::open(&refreshed_bytes)?;
        assert!(
            refreshed_image
                .methods()
                .any(|method| method.is_ok_and(|method| method.name == b"NewRoute"))
        );
        assert!(cache_entry_is_valid(
            &cache_root_cap,
            helper_entry_name,
            "oracle",
            &prefix
        ));
        assert_ne!(std::fs::read(&helper)?, b"mutated real Go helper");
        Ok(())
    }

    fn explicit_go_roots(go: &Path) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let go_directory = go
            .parent()
            .ok_or_else(|| io::Error::other("Go executable has no parent"))?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("HOME is required to locate the Go module cache"))?
            .canonicalize()?;
        let mut command = Command::new(go);
        command
            .args(["env", "GOROOT", "GOMODCACHE"])
            .env_clear()
            .env("PATH", go_directory)
            .env("HOME", &home)
            .env("GOENV", "off")
            .env("GOTOOLCHAIN", "local")
            .env("GOPROXY", "off")
            .env("GOSUMDB", "off");
        let output = command.output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "selected Go compiler could not report its roots: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        let roots = String::from_utf8(output.stdout)?;
        let mut lines = roots.lines();
        let goroot = PathBuf::from(
            lines
                .next()
                .ok_or_else(|| io::Error::other("go env omitted GOROOT"))?,
        );
        let module_cache = PathBuf::from(
            lines
                .next()
                .ok_or_else(|| io::Error::other("go env omitted GOMODCACHE"))?,
        );
        let goroot = goroot.canonicalize()?;
        let module_cache = std::env::var_os("GOMODCACHE")
            .map(PathBuf::from)
            .unwrap_or(module_cache)
            .canonicalize()?;
        if !module_cache.is_dir() {
            return Err(io::Error::other(format!(
                "Go module cache {} is not a directory",
                module_cache.display()
            ))
            .into());
        }
        Ok((goroot, module_cache))
    }

    fn assert_go_authority_succeeds(
        oracle: &super::ConfiguredGoOracle,
        module: &Path,
        source: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let witness = GoWorkWitness::capture(module)?;
        oracle.authority_image_for_package_with_witness(source, module, &witness)?;
        Ok(())
    }
}
