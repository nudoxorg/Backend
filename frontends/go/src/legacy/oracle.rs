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
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use backend_semantic::vocabulary::{NativeWorker, NativeWorkerPanic};
use serde::Deserialize;
use sha2::{Digest, Sha256};

mod authority_witness;
pub use self::authority_witness::{
    GoFilesystemTargetKind, GoLocalOnlyReason, GoPackageAuthorityWitness,
    GoPackageAuthorityWitnessError, GoWorkFileWitness, GoWorkWitness, GoWorkWitnessError,
};

const DIAGNOSTIC_PREFIX_LIMIT: usize = 4096;
const DIAGNOSTIC_TAIL_LIMIT: usize = 4096;
const UNSUPPORTED_CGO_SENTINEL: &str = "NUDOX_GO_UNSUPPORTED_CGO_CLOSURE";

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
#[derive(Debug, Clone, Copy)]
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
    helper_binary_cache: std::sync::Arc<std::sync::OnceLock<PathBuf>>,
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
            helper_binary_cache: std::sync::Arc::new(std::sync::OnceLock::new()),
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
        self.execute_with_origin(command, false)
    }

    fn execute_configured(
        &self,
        command: &mut std::process::Command,
    ) -> Result<Vec<u8>, OracleError> {
        self.execute_with_origin(command, true)
    }

    fn execute_with_origin(
        &self,
        command: &mut std::process::Command,
        configured: bool,
    ) -> Result<Vec<u8>, OracleError> {
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

    fn cached_helper_binary(&self) -> Result<Option<PathBuf>, OracleError> {
        let GoOracleConfiguration::GoToolchain(executable) = &self.configuration else {
            return Ok(None);
        };
        let Some(environment) = &self.child_environment else {
            return Ok(None);
        };
        if let Some(path) = self.helper_binary_cache.get() {
            return Ok(Some(path.clone()));
        }
        let path = self.prepare_cached_helper(executable.as_ref(), environment)?;
        let _ = self.helper_binary_cache.set(path.clone());
        Ok(Some(
            self.helper_binary_cache.get().cloned().unwrap_or(path),
        ))
    }

    fn prepare_cached_helper(
        &self,
        executable: &Path,
        environment: &GoOracleChildEnvironment,
    ) -> Result<PathBuf, OracleError> {
        use fs4::fs_std::FileExt;
        use std::{fs, io::Write, time::Instant};

        let source_identity = helper_source_identity();
        let toolchain_identity = environment.toolchain_identity();
        let mut key_digest = Sha256::new();
        key_digest.update(b"nudox.go-oracle-compiled-helper.v1\0");
        key_digest.update(source_identity);
        key_digest.update(toolchain_identity);
        let key = digest_hex(&key_digest.finalize());
        let cache_root = environment.build_cache().join("nudox-go-oracle-v1");
        fs::create_dir_all(&cache_root).map_err(|error| OracleError::GoOracleHelperCache {
            detail: format!("create cache root {:?}: {error}", cache_root),
        })?;
        let lock_path = cache_root.join("build.lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("open cache lock {:?}: {error}", lock_path),
            })?;
        let started = Instant::now();
        loop {
            let acquired = match FileExt::try_lock_exclusive(&lock_file) {
                Ok(acquired) => acquired,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => {
                    return Err(OracleError::GoOracleHelperCache {
                        detail: format!("lock cache {:?}: {error}", lock_path),
                    });
                }
            };
            if acquired {
                break;
            }
            if started.elapsed() >= self.oracle.timeout {
                return Err(OracleError::GoOracleHelperCache {
                    detail: format!("timed out waiting for cache lock {:?}", lock_path),
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let entry = cache_root.join(&key);
        let binary_name = if cfg!(windows) {
            "oracle.exe"
        } else {
            "oracle"
        };
        let binary_path = entry.join(binary_name);
        let manifest_path = entry.join("manifest.txt");
        let manifest_prefix = format!(
            "schema=1\nsource={}\ntoolchain={}\n",
            digest_hex(&source_identity),
            digest_hex(&toolchain_identity),
        );
        if cache_entry_is_valid(&entry, &binary_path, &manifest_path, &manifest_prefix) {
            return Ok(binary_path);
        }
        if fs::symlink_metadata(&entry).is_ok() {
            if fs::symlink_metadata(&entry)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                fs::remove_file(&entry).map_err(|error| OracleError::GoOracleHelperCache {
                    detail: format!("remove linked cache entry {:?}: {error}", entry),
                })?;
            } else {
                fs::remove_dir_all(&entry).map_err(|error| OracleError::GoOracleHelperCache {
                    detail: format!("remove invalid cache entry {:?}: {error}", entry),
                })?;
            }
        }

        let staging = tempfile::Builder::new()
            .prefix("go-oracle-build-")
            .tempdir_in(&cache_root)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("create staging directory in {:?}: {error}", cache_root),
            })?;
        let source = EmbeddedGoOracleSource::materialize()?;
        let staged_binary = staging.path().join(binary_name);
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
        let binary_hash = hash_regular_file(&staged_binary).map_err(|error| {
            OracleError::GoOracleHelperCache {
                detail: format!("hash built oracle {:?}: {error}", staged_binary),
            }
        })?;
        let manifest = format!("{}binary={}\n", manifest_prefix, digest_hex(&binary_hash));
        let staged_manifest = staging.path().join("manifest.txt");
        let mut manifest_file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged_manifest)
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("create cache manifest {:?}: {error}", staged_manifest),
            })?;
        manifest_file
            .write_all(manifest.as_bytes())
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("write cache manifest {:?}: {error}", staged_manifest),
            })?;
        manifest_file
            .sync_all()
            .map_err(|error| OracleError::GoOracleHelperCache {
                detail: format!("sync cache manifest {:?}: {error}", staged_manifest),
            })?;
        let staging_path = staging.keep();
        fs::rename(&staging_path, &entry).map_err(|error| OracleError::GoOracleHelperCache {
            detail: format!("install helper cache entry {:?}: {error}", entry),
        })?;
        Ok(binary_path)
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
        GoPackageAuthorityWitness::capture(package_root)
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
        let helper_binary = self.cached_helper_binary()?;
        self.oracle.run_configured(
            &self.configuration,
            self.child_environment.as_ref(),
            work.as_ref(),
            helper_binary.as_deref(),
            module,
        )
    }

    /// Produces the authority image for one selected source file and module.
    pub fn authority_image(&self, source: &Path, module: &Path) -> Result<Vec<u8>, OracleError> {
        let (environment, work) = self.authority_child_context(module, None)?;
        let helper_binary = self.cached_helper_binary()?;
        self.oracle.authority_image_configured(
            &self.configuration,
            environment,
            &work,
            helper_binary.as_deref(),
            source,
            module,
        )
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
        let helper_binary = self.cached_helper_binary()?;
        self.oracle.authority_image_for_package_configured(
            &self.configuration,
            environment,
            &work,
            helper_binary.as_deref(),
            source,
            module,
        )
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
        let helper_binary = self.cached_helper_binary()?;
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
        self.oracle.execute_configured(command.command_mut())
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

fn cache_entry_is_valid(entry: &Path, binary: &Path, manifest: &Path, prefix: &str) -> bool {
    use std::fs;
    let Ok(entry_metadata) = fs::symlink_metadata(entry) else {
        return false;
    };
    if !entry_metadata.is_dir() || entry_metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(binary_metadata) = fs::symlink_metadata(binary) else {
        return false;
    };
    if !binary_metadata.is_file() || binary_metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(manifest_metadata) = fs::symlink_metadata(manifest) else {
        return false;
    };
    if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(manifest_text) = fs::read_to_string(manifest) else {
        return false;
    };
    let Some(expected_binary_hash) = manifest_text
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix("binary="))
        .and_then(|rest| rest.strip_suffix('\n'))
    else {
        return false;
    };
    if expected_binary_hash.len() != 64
        || !expected_binary_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    hash_regular_file(binary)
        .map(|hash| digest_hex(&hash) == expected_binary_hash)
        .unwrap_or(false)
}

fn hash_regular_file(path: &Path) -> std::io::Result<[u8; 32]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
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
    use std::{fs, io::Read};

    fn add_file(path: &Path, logical: &Path, digest: &mut Sha256) -> std::io::Result<()> {
        let metadata = fs::metadata(path)?;
        digest.update(b"file\0");
        digest.update((logical.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
        digest.update(logical.as_os_str().as_encoded_bytes());
        digest.update(metadata.len().to_be_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            digest.update(metadata.permissions().mode().to_be_bytes());
        }
        let mut file = fs::File::open(path)?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(())
    }

    fn walk(
        root: &Path,
        path: &Path,
        logical: &Path,
        digest: &mut Sha256,
        visited: &mut HashSet<PathBuf>,
    ) -> std::io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(path)?;
            digest.update(b"symlink\0");
            digest.update((logical.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
            digest.update(logical.as_os_str().as_encoded_bytes());
            digest.update((target.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
            digest.update(target.as_os_str().as_encoded_bytes());
            if !visited.insert(target.clone()) {
                digest.update(b"already-visited\0");
                return Ok(());
            }
            if target.is_dir() {
                let mut entries = fs::read_dir(&target)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()?;
                entries.sort();
                for entry in entries {
                    let name = entry.file_name().ok_or_else(|| {
                        std::io::Error::other("Go toolchain entry has no file name")
                    })?;
                    walk(root, &entry, &logical.join(name), digest, visited)?;
                }
            } else {
                add_file(&target, logical, digest)?;
            }
            return Ok(());
        }
        if metadata.is_dir() {
            let canonical = fs::canonicalize(path)?;
            if !visited.insert(canonical) {
                return Ok(());
            }
            digest.update(b"directory\0");
            digest.update((logical.as_os_str().as_encoded_bytes().len() as u64).to_be_bytes());
            digest.update(logical.as_os_str().as_encoded_bytes());
            let mut entries = fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort();
            for entry in entries {
                let name = entry
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("Go toolchain entry has no file name"))?;
                walk(root, &entry, &logical.join(name), digest, visited)?;
            }
            return Ok(());
        }
        if metadata.is_file() {
            add_file(path, logical, digest)?;
            return Ok(());
        }
        let relative = path.strip_prefix(root).unwrap_or(path);
        Err(std::io::Error::other(format!(
            "unsupported Go toolchain entry {relative:?}"
        )))
    }

    let canonical_root = fs::canonicalize(goroot)?;
    let mut digest = Sha256::new();
    digest.update(b"nudox.go-toolchain-identity.v1\0");
    add_file(executable, Path::new("selected-go-executable"), &mut digest)?;
    let mut visited = HashSet::new();
    walk(
        &canonical_root,
        &canonical_root,
        Path::new("GOROOT"),
        &mut digest,
        &mut visited,
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
        GoCgoPolicy, GoOracle, GoOracleChildEnvironment, GoOracleConfiguration,
        GoOracleConfigurationError, GoOracleInvocationModeV1, GoPackageAuthorityWitness,
        GoWorkWitness, cache_entry_is_valid, digest_hex, hash_regular_file, read_bounded,
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
        let build_cache = root.join("native-work/go-oracle-cache");
        let environment =
            GoOracleChildEnvironment::new(go, goroot, module_cache, build_cache.clone())
                .expect("cache leaf is an inspection-safe typed path");
        assert_eq!(environment.cgo_policy(), GoCgoPolicy::Disabled);
        assert!(!build_cache.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn helper_cache_requires_an_exact_regular_manifest() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let entry = root.path().join("entry");
        std::fs::create_dir(&entry)?;
        let binary = entry.join("oracle");
        std::fs::write(&binary, b"compiled helper bytes")?;
        let binary_hash = hash_regular_file(&binary)?;
        let manifest = entry.join("manifest.txt");
        let prefix = format!(
            "schema=1\nsource={}\ntoolchain={}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        std::fs::write(
            &manifest,
            format!("{prefix}binary={}\n", digest_hex(&binary_hash)),
        )?;
        assert!(cache_entry_is_valid(&entry, &binary, &manifest, &prefix));

        std::fs::write(
            &manifest,
            format!(
                "{prefix}binary={}\nunexpected=yes\n",
                digest_hex(&binary_hash)
            ),
        )?;
        assert!(!cache_entry_is_valid(&entry, &binary, &manifest, &prefix));
        std::fs::write(
            &manifest,
            format!("{prefix}binary={}\n\n", digest_hex(&binary_hash)),
        )?;
        assert!(!cache_entry_is_valid(&entry, &binary, &manifest, &prefix));

        #[cfg(unix)]
        {
            let target = root.path().join("manifest-target.txt");
            std::fs::write(
                &target,
                format!("{prefix}binary={}\n", digest_hex(&binary_hash)),
            )?;
            std::fs::remove_file(&manifest)?;
            std::os::unix::fs::symlink(&target, &manifest)?;
            assert!(!cache_entry_is_valid(&entry, &binary, &manifest, &prefix));
        }
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
        let host_path =
            std::env::var_os("PATH").ok_or_else(|| io::Error::other("host PATH is unavailable"))?;
        let chmod = std::env::split_paths(&host_path)
            .map(|directory| directory.join("chmod"))
            .find(|path| path.is_file())
            .ok_or_else(|| io::Error::other("host PATH has no chmod executable"))?;
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
{} 700 "$out"
"#,
            helper_cwd.display(),
            build_count.display(),
            build_count.display(),
            build_count.display(),
            chmod.display(),
        );
        std::fs::write(&go, program)?;
        std::fs::set_permissions(&go, std::fs::Permissions::from_mode(0o700))?;

        let goroot = root.path().join("goroot");
        let module_cache = root.path().join("modules");
        let module = root.path().join("target");
        std::fs::create_dir_all(&goroot)?;
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
        std::fs::rename(&source_directory, &unavailable_directory)?;
        let first_result = oracle.run(&module);
        let second_result = oracle.run(&module);
        std::fs::rename(&unavailable_directory, &source_directory)?;
        let first = first_result?;
        let second = second_result?;
        assert_eq!(first.schema_version, super::Output::REQUIRED_SCHEMA_VERSION);
        assert_eq!(
            second.schema_version,
            super::Output::REQUIRED_SCHEMA_VERSION
        );
        assert_eq!(std::fs::read_to_string(&build_count)?.trim(), "1");

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
