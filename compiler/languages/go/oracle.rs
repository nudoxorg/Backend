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

use std::collections::HashMap;

use serde::Deserialize;

const DIAGNOSTIC_PREFIX_LIMIT: usize = 4096;
const DIAGNOSTIC_TAIL_LIMIT: usize = 4096;

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
    pub const REQUIRED_SCHEMA_VERSION: u32 = 3;
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

    /// Go/types-resolved same-package function calls.
    #[serde(default)]
    pub references: Box<[Reference]>,
}

/// One resolved function-use edge in a package.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// The calling function or method's bare name.
    pub owner: String,

    /// The bare receiver type name when `owner` is a method (empty for a
    /// package-level function).
    #[serde(default)]
    pub owner_recv: String,

    /// The called function's bare name. Only free (non-method) package-level
    /// functions are recorded as targets.
    pub target: String,

    /// The target's defining package's import path, present only when it
    /// differs from the package this `Reference` was extracted from (empty
    /// for a same-package call). See `oracle/go/serialize.go`'s doc comment
    /// on the Go-side field for why routing (same-module `Intro`-eligible vs.
    /// genuinely foreign) is decided on this side, not the oracle's.
    #[serde(default)]
    pub target_pkg: String,

    pub file: String,
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

/// Typed failures at the subprocess and protocol boundary.
#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    /// The configured executable could not be started.
    #[error("Go oracle could not be started ({program}): {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
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
}

/// Configurable subprocess adapter for the vendored Go oracle.
#[derive(Debug, Clone, Copy)]
pub struct GoOracle {
    /// Maximum bytes retained and accepted from each child stream.
    pub output_limit: usize,
    /// Maximum wall-clock duration for the child.
    pub timeout: std::time::Duration,
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

    /// Runs the override executable, or `go run . <module>` in the vendored directory.
    pub fn run(&self, module: &std::path::Path) -> Result<Output, OracleError> {
        let oracle_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle");
        let override_bin = std::env::var("NUDOX_GO_ORACLE_BIN");
        let mut command = if let Ok(binary) = override_bin {
            let mut command = std::process::Command::new(binary);
            command.arg(module);
            command
        } else {
            let compiler = match std::env::var("COMPILER_GO_COMPILER") {
                Ok(path) => path,
                Err(_) => "go".to_owned(),
            };
            let mut command = std::process::Command::new(compiler);
            command
                .args(["run", "."])
                .arg(module)
                .current_dir(oracle_dir);
            command
        };
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|source| OracleError::ToolingUnavailable {
                tool: "NUDOX_GO_ORACLE_BIN or go run",
                source,
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
        let out_tx = limit_tx.clone();
        let out_thread = std::thread::spawn(move || read_bounded(stdout, limit, "stdout", out_tx));
        let err_thread =
            std::thread::spawn(move || read_bounded(stderr, limit, "stderr", limit_tx));
        let started = std::time::Instant::now();
        let mut limit_failure = None;
        let terminal = loop {
            if let Ok((stream, observed)) = limit_rx.try_recv() {
                limit_failure = Some((stream, observed));
                terminate_child(&mut child);
                break None;
            }
            if let Some(status) = child.try_wait().map_err(|source| OracleError::Pipe {
                stream: "process",
                source,
            })? {
                break Some(status);
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let stdout = out_thread.join().map_err(|_| OracleError::Pipe {
            stream: "stdout",
            source: std::io::Error::other("reader panicked"),
        })?;
        let stderr = err_thread.join().map_err(|_| OracleError::Pipe {
            stream: "stderr",
            source: std::io::Error::other("reader panicked"),
        })?;
        if let Some((stream, source)) = stdout.error.or(stderr.error) {
            return Err(OracleError::Pipe { stream, source });
        }
        if let Some((stream, observed)) = limit_failure.or(stdout.exceeded).or(stderr.exceeded) {
            return Err(OracleError::OutputLimit {
                phase: "collect",
                stream,
                observed,
                limit,
            });
        }
        if terminal.is_none() {
            return Err(OracleError::Timeout {
                phase: "collect",
                milliseconds: self.timeout.as_millis() as u64,
            });
        }
        let status = terminal.ok_or_else(|| OracleError::Pipe {
            stream: "process",
            source: std::io::Error::other("missing child status"),
        })?;
        if !status.success() {
            return Err(OracleError::Exit {
                status: status.to_string(),
                stderr: tail(&stderr.bytes),
            });
        }
        self.decode(&stdout.bytes)
    }
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
        let pid = child.id().to_string();
        let result = std::process::Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
        if !result.map(|status| status.success()).unwrap_or(false) {
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
    use super::read_bounded;
    use std::io::{self, Read};

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
}
