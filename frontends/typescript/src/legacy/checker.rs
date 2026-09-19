//! Owns the first-class TypeScript checker authority: a typed transaction
//! against the real `tsc` type checker, exposed as a peer surface of this
//! crate beside the OXC syntax-and-binding authority.
//!
//! The checker runs in a bounded subprocess exactly like the vendored Go
//! oracle (`compiler/languages/go/oracle.rs`): a vendored driver script
//! (`checker/main.cjs`) drives the TypeScript compiler API over one source
//! file, prints one JSON report, and every child stream is bounded by an
//! output limit and a wall-clock deadline. The report binds its exact source
//! bytes by SHA-256, so a report can never be admitted for different source.
//!
//! This module owns the wire schema, the typed rejection causes, and the
//! UTF-16 → UTF-8 span binding that projects checker coordinates onto OXC
//! byte coordinates. It contains no lowering policy: consuming the facts is
//! the caller's boundary.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use backend_semantic::vocabulary::{NativeWorker, NativeWorkerPanic, TypeScriptSource};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::legacy::{CoordinateError, Utf8Span, Utf16Span};

/// Digest width of one SHA-256 source binding.
const DIGEST_BYTES: usize = 32;
/// Diagnostic transcript prefix retained by decode rejections.
const TRANSCRIPT_PREFIX_LIMIT: usize = 4096;
/// Diagnostic transcript tail retained by child rejections.
const TRANSCRIPT_TAIL_LIMIT: usize = 4096;
/// Exit code the vendored driver uses when `typescript` is not resolvable.
const MODULE_MISSING_EXIT: i32 = 3;
/// Maximum number of files copied for one package-context run.
const PACKAGE_FILE_LIMIT: usize = 4096;
/// Maximum total bytes copied for one package-context run.
const PACKAGE_BYTE_LIMIT: usize = 64 * 1024 * 1024;

/// The payload schema this build reads; a differing schema is typed staleness.
pub(crate) const REQUIRED_SCHEMA_VERSION: u32 = 1;

/// Exact rejection at the TypeScript checker authority boundary.
#[derive(Debug, thiserror::Error)]
pub enum CheckerError {
    /// The configured checker tool could not be started.
    #[error("TypeScript checker could not be started ({program}): {source}")]
    Spawn {
        /// The rejected program spelling.
        program: String,
        /// The operating system's spawn failure.
        #[source]
        source: std::io::Error,
    },
    /// A bounded stream reader panicked; its exact worker and supported
    /// payload facts survive the join boundary.
    #[error("TypeScript checker stream worker panicked: {cause}")]
    WorkerPanic {
        /// Bounded original join payload.
        #[source]
        cause: NativeWorkerPanic,
    },
    /// The checker tool itself is unavailable on this machine.
    #[error("TypeScript checker tooling unavailable ({tool}): {source}")]
    ToolingUnavailable {
        /// The resolution path that failed.
        tool: &'static str,
        /// The operating system's resolution failure.
        #[source]
        source: std::io::Error,
    },
    /// The child started but the vendored `typescript` module was missing.
    #[error("TypeScript checker module unavailable; stderr tail: {stderr}")]
    ModuleUnavailable {
        /// The child's retained stderr tail.
        stderr: String,
    },
    /// The child exited unsuccessfully, retaining its diagnostic tail.
    #[error("TypeScript checker exited with {status}; stderr tail: {stderr}")]
    Exit {
        /// The child's terminal status.
        status: String,
        /// The child's retained stderr tail.
        stderr: String,
    },
    /// The child emitted malformed or incomplete JSON.
    #[error("TypeScript checker JSON decode failed: {message}; transcript prefix: {transcript}")]
    Decode {
        /// The serde rejection message.
        message: String,
        /// The retained transcript prefix.
        transcript: String,
    },
    /// The output bound was exceeded and the child was reaped.
    #[error(
        "TypeScript checker output limit exceeded during {phase} on {stream}: observed {observed}, limit {limit}"
    )]
    OutputLimit {
        /// The run phase that observed the overrun.
        phase: &'static str,
        /// The child stream that overran.
        stream: &'static str,
        /// Observed byte count.
        observed: usize,
        /// Configured byte bound.
        limit: usize,
    },
    /// The child did not complete before the deadline and was reaped.
    #[error("TypeScript checker timed out during {phase} after {milliseconds}ms")]
    Timeout {
        /// The run phase that timed out.
        phase: &'static str,
        /// Configured deadline in milliseconds.
        milliseconds: u128,
    },
    /// The report's schema cell was not the version this adapter understands.
    #[error("TypeScript checker schema is stale: found {found}, expected {expected}")]
    Staleness {
        /// Observed schema version.
        found: u32,
        /// Required schema version.
        expected: u32,
    },
    /// The report was produced for different source bytes.
    #[error("TypeScript checker report is bound to different source bytes")]
    SourceBinding {
        /// SHA-256 of the exact caller source bytes.
        expected: [u8; DIGEST_BYTES],
        /// SHA-256 lent by the report header.
        observed: [u8; DIGEST_BYTES],
    },
    /// A child pipe could not be read or its reader thread failed to join.
    #[error("TypeScript checker pipe failed during {stream}: {source}")]
    Pipe {
        /// The failing stream.
        stream: &'static str,
        /// The pipe failure.
        #[source]
        source: std::io::Error,
    },
    /// The checker work directory could not be prepared or removed.
    #[error("TypeScript checker work directory failed during {phase}: {source}")]
    Work {
        /// The failing work phase.
        phase: &'static str,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The package staging file count exceeded its bound.
    #[error("TypeScript checker package file limit exceeded: observed {observed}, limit {limit}")]
    PackageFileLimit {
        /// Number of package files observed before rejection.
        observed: usize,
        /// Maximum number of package files accepted.
        limit: usize,
    },
    /// The package staging byte count exceeded its bound.
    #[error("TypeScript checker package byte limit exceeded: observed {observed}, limit {limit}")]
    PackageByteLimit {
        /// Total package bytes observed before rejection.
        observed: usize,
        /// Maximum total package bytes accepted.
        limit: usize,
    },
    /// Package staging did not complete before the checker deadline.
    #[error("TypeScript checker package staging timed out after {milliseconds}ms")]
    PackageTimeout {
        /// Configured deadline in milliseconds.
        milliseconds: u128,
    },
    /// A checker UTF-16 span could not bind to the exact UTF-8 source.
    #[error("TypeScript checker span {start}..{end} does not bind to the source")]
    SpanBinding {
        /// Observed first UTF-16 bound.
        start: u32,
        /// Observed second UTF-16 bound.
        end: u32,
    },
}

/// One checker-computed or checker-declared type tree.
///
/// The tree mirrors the vendored driver's closed JSON grammar. Unknown
/// checker types travel as [`TypeTree::Other`] with their exact spelling
/// instead of being silently dropped.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TypeTree {
    /// A language-level primitive (`number`, `string`, `void`, ...).
    Primitive {
        /// The primitive spelling.
        name: String,
    },
    /// A literal type (`7`, `"x"`, `true`).
    Literal {
        /// The literal's base class.
        #[serde(rename = "literal")]
        base: LiteralBase,
        /// The exact literal spelling.
        text: String,
    },
    /// A reference to a named type, possibly owned by a foreign module.
    Reference {
        /// The referenced type name.
        name: String,
        /// The foreign module origin, when resolved outside the source.
        module: Option<String>,
        /// The generic application arguments, in declared order.
        #[serde(default)]
        args: Vec<TypeTree>,
    },
    /// An untagged union of members.
    Union {
        /// The union members, in declared order.
        members: Vec<TypeTree>,
    },
    /// An intersection of members.
    Intersection {
        /// The intersection members, in declared order.
        members: Vec<TypeTree>,
    },
    /// A conditional type with its four source-level operands.
    Conditional {
        check: Box<TypeTree>,
        extends: Box<TypeTree>,
        #[serde(rename = "thenType")]
        then_type: Box<TypeTree>,
        #[serde(rename = "elseType")]
        else_type: Box<TypeTree>,
    },
    /// A mapped type with its key constraint, optional `as` remap, and value type.
    Mapped {
        parameter: String,
        constraint: Box<TypeTree>,
        /// The optional key remap written after `as`.
        #[serde(rename = "nameAs", default)]
        name_as: Option<Box<TypeTree>>,
        value: Box<TypeTree>,
        readonly: MappedModifier,
        optional: MappedModifier,
    },
    /// One part of a template-literal type, in source order.
    TemplateLiteral { parts: Vec<TemplatePart> },
    /// A tuple with positional elements.
    Tuple {
        /// The tuple elements, in declared order.
        elements: Vec<TypeTree>,
    },
    /// An array of one element type.
    Array {
        /// The element type.
        element: Box<TypeTree>,
    },
    /// A callable signature.
    Function {
        /// The parameter types, in declared order.
        parameters: Vec<TypeTree>,
        /// The result type.
        result: Box<TypeTree>,
    },
    /// The `this` type.
    This,
    /// A use of a generic type parameter.
    TypeParameter {
        /// The parameter's declared name.
        name: String,
    },
    /// An anonymous structural record.
    Object {
        /// The members, in declared order.
        members: Vec<ObjectMember>,
    },
    /// A checker type with no closed form; the exact spelling is retained.
    Other {
        /// The checker's exact printed spelling.
        text: String,
    },
}

/// The modifier applied by one mapped type member.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MappedModifier {
    /// No modifier was written.
    Preserve,
    /// The modifier is added.
    Add,
    /// The modifier is removed.
    Remove,
}

/// One ordered template-literal part.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TemplatePart {
    /// Literal source text.
    Text { text: String },
    /// A placeholder type.
    Type { r#type: Box<TypeTree> },
}

/// The base class of one [`TypeTree::Literal`].
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LiteralBase {
    /// A numeric literal type.
    Number,
    /// A string literal type.
    String,
    /// A boolean literal type.
    Boolean,
    /// A bigint literal type.
    Bigint,
}

/// One member of an anonymous structural record.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectMember {
    /// The member's declared key.
    pub name: String,
    /// Whether the member is optional.
    #[serde(default)]
    pub optional: bool,
    /// Whether the member is readonly.
    #[serde(default)]
    pub readonly: bool,
    /// The member's type.
    #[serde(rename = "type")]
    pub member_type: TypeTree,
}

/// The provenance of one checker type fact: what the source wrote versus
/// what the checker derived. This distinction is the checker authority's
/// declared-versus-computed contract.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    /// The type came from a written annotation.
    Declared,
    /// The type was computed by the checker.
    Computed,
}

/// One checker type fact at one declaration name span.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    /// First UTF-16 bound of the declaration name.
    pub name_start: u32,
    /// Second UTF-16 bound of the declaration name.
    pub name_end: u32,
    /// Whether the type was written or computed.
    pub origin: Origin,
    /// The exact overload member index within its same-name group.
    #[serde(default)]
    pub overload_index: Option<u32>,
    /// The checker's type tree for this declaration.
    #[serde(default)]
    pub r#type: Option<TypeTree>,
}

/// One resolved reference at one use-site span.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reference {
    /// First UTF-16 bound of the use site.
    pub start: u32,
    /// Second UTF-16 bound of the use site.
    pub end: u32,
    /// First UTF-16 bound of the same-file declaration name.
    #[serde(default)]
    pub target_start: Option<u32>,
    /// Second UTF-16 bound of the same-file declaration name.
    #[serde(default)]
    pub target_end: Option<u32>,
    /// The foreign module origin, when resolved outside the source.
    #[serde(default)]
    pub module: Option<String>,
    /// The foreign declaration name.
    #[serde(default)]
    pub name: Option<String>,
    /// The exact chosen overload member index at a resolved call site.
    #[serde(default)]
    pub overload_index: Option<u32>,
}

/// One control-flow-sensitive checker type observed at one plain
/// `target = value` assignment site.
///
/// `name_start`/`name_end` bind the *declaration* name of the assigned
/// symbol so the consumer can own the row by its declared fact;
/// `start`/`end` bound the exact assignment site whose program-point type
/// the checker reported.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Narrowing {
    /// First UTF-16 bound of the assigned symbol's same-file declaration name.
    pub name_start: u32,
    /// Second UTF-16 bound of the assigned symbol's same-file declaration name.
    pub name_end: u32,
    /// First UTF-16 bound of the assignment site.
    pub start: u32,
    /// Second UTF-16 bound of the assignment site.
    pub end: u32,
    /// The checker's type of the assigned value at this site.
    #[serde(default)]
    pub r#type: Option<TypeTree>,
}

/// The complete checker report for exactly one source file.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    /// The payload schema the child spoke.
    #[serde(default)]
    pub schema_version: u32,
    /// SHA-256 hex digest of the exact checked source bytes.
    pub source_digest: String,
    /// The checker's own diagnostics, best-effort and non-fatal.
    #[serde(default)]
    pub diagnostics: Box<[String]>,
    /// One entry per checker-typed declaration, in source order.
    #[serde(default)]
    pub declarations: Box<[Declaration]>,
    /// One entry per resolved reference, in source order.
    #[serde(default)]
    pub references: Box<[Reference]>,
    /// One entry per reported assignment narrowing, in source order.
    ///
    /// The cell is additive and optional: transcripts emitted before the
    /// narrowing lane existed decode with an empty set, and the schema
    /// version stays `1` because the closed grammar only ever grew
    /// defaulted cells.
    #[serde(default)]
    pub narrowings: Box<[Narrowing]>,
}

/// One span-bound declaration fact borrowed from a validated report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundDeclaration<'report> {
    /// The declaration name span in UTF-8 source bytes.
    pub name: Utf8Span,
    /// Whether the type was written or computed.
    pub origin: Origin,
    /// The exact overload member index within its same-name group.
    pub overload_index: Option<u32>,
    /// The checker's type tree.
    pub r#type: Option<&'report TypeTree>,
}

/// One span-bound reference fact borrowed from a validated report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundReference<'report> {
    /// The use-site span in UTF-8 source bytes.
    pub span: Utf8Span,
    /// The same-file declaration name span in UTF-8 source bytes.
    pub target: Option<Utf8Span>,
    /// The foreign module origin.
    pub module: Option<&'report str>,
    /// The foreign declaration name.
    pub name: Option<&'report str>,
    /// The exact chosen overload member index.
    pub overload_index: Option<u32>,
}

/// One span-bound assignment narrowing borrowed from a validated report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundNarrowing<'report> {
    /// The assigned symbol's declaration name span in UTF-8 source bytes.
    pub name: Utf8Span,
    /// The exact assignment site span in UTF-8 source bytes.
    pub site: Utf8Span,
    /// The checker's type of the assigned value at this site.
    pub r#type: Option<&'report TypeTree>,
}

/// A validated report bound to exact UTF-8 source bytes.
///
/// Every checker UTF-16 coordinate is projected onto the OXC byte grid at
/// construction; a projection fault is a typed rejection, never a silent
/// misattribution.
#[derive(Clone, Debug)]
pub struct CheckerIndex<'report> {
    report: &'report Report,
    declarations: Box<[BoundDeclaration<'report>]>,
    references: Box<[BoundReference<'report>]>,
    /// Reference ordinals sorted by exact UTF-8 use span, then report order.
    /// Iteration remains report-ordered while point queries are logarithmic.
    reference_index: Box<[usize]>,
    narrowings: Box<[BoundNarrowing<'report>]>,
}

impl<'report> CheckerIndex<'report> {
    /// Binds a decoded report to exact UTF-8 source bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CheckerError::SourceBinding`] when the report was produced
    /// for different source, and [`CheckerError::SpanBinding`] when any
    /// checker span fails the UTF-16 to UTF-8 projection.
    pub fn bind(report: &'report Report, source: &str) -> Result<Self, CheckerError> {
        let expected = source_digest(source.as_bytes());
        let observed = hex_digest(&report.source_digest);
        match observed {
            Some(observed) if observed == expected => {}
            Some(observed) => {
                return Err(CheckerError::SourceBinding { expected, observed });
            }
            None => {
                return Err(CheckerError::Decode {
                    message: "source digest is not 64 hex digits".to_owned(),
                    transcript: String::new(),
                });
            }
        }
        let declarations = report
            .declarations
            .iter()
            .map(
                |declaration| -> Result<BoundDeclaration<'report>, CheckerError> {
                    let name = bind_span(source, declaration.name_start, declaration.name_end)?;
                    Ok(BoundDeclaration {
                        name,
                        origin: declaration.origin,
                        overload_index: declaration.overload_index,
                        r#type: declaration.r#type.as_ref(),
                    })
                },
            )
            .collect::<Result<Box<[_]>, CheckerError>>()?;
        let references = report
            .references
            .iter()
            .map(
                |reference| -> Result<BoundReference<'report>, CheckerError> {
                    let span = bind_span(source, reference.start, reference.end)?;
                    let target = match (reference.target_start, reference.target_end) {
                        (Some(start), Some(end)) => Some(bind_span(source, start, end)?),
                        _ => None,
                    };
                    Ok(BoundReference {
                        span,
                        target,
                        module: reference.module.as_deref(),
                        name: reference.name.as_deref(),
                        overload_index: reference.overload_index,
                    })
                },
            )
            .collect::<Result<Box<[_]>, CheckerError>>()?;
        let mut reference_index: Vec<usize> = (0..references.len()).collect();
        reference_index.sort_unstable_by_key(|ordinal| {
            let reference = &references[*ordinal];
            (reference.span.start, reference.span.end, *ordinal)
        });
        let narrowings = report
            .narrowings
            .iter()
            .map(
                |narrowing| -> Result<BoundNarrowing<'report>, CheckerError> {
                    let name = bind_span(source, narrowing.name_start, narrowing.name_end)?;
                    let site = bind_span(source, narrowing.start, narrowing.end)?;
                    Ok(BoundNarrowing {
                        name,
                        site,
                        r#type: narrowing.r#type.as_ref(),
                    })
                },
            )
            .collect::<Result<Box<[_]>, CheckerError>>()?;
        Ok(Self {
            report,
            declarations,
            references,
            reference_index: reference_index.into_boxed_slice(),
            narrowings,
        })
    }

    /// Borrows the underlying validated report.
    #[must_use]
    pub const fn report(&self) -> &'report Report {
        self.report
    }

    /// Streams every span-bound declaration in report order.
    pub fn declarations(&self) -> impl Iterator<Item = &BoundDeclaration<'report>> {
        self.declarations.iter()
    }

    /// Streams every span-bound reference in report order.
    pub fn references(&self) -> impl Iterator<Item = &BoundReference<'report>> {
        self.references.iter()
    }

    /// Borrows the first report-ordered reference at one exact UTF-8 span.
    ///
    /// Duplicate rows retain their producer order through the ordinal tie
    /// breaker, matching the historical iterator `find` semantics without a
    /// complete scan for every syntax reference.
    #[must_use]
    pub fn reference_at(&self, span: Utf8Span) -> Option<&BoundReference<'report>> {
        let position = self.reference_index.partition_point(|ordinal| {
            let reference = &self.references[*ordinal];
            (reference.span.start, reference.span.end) < (span.start, span.end)
        });
        let ordinal = *self.reference_index.get(position)?;
        let reference = self.references.get(ordinal)?;
        (reference.span == span).then_some(reference)
    }

    /// Streams every span-bound assignment narrowing in report order.
    pub fn narrowings(&self) -> impl Iterator<Item = &BoundNarrowing<'report>> {
        self.narrowings.iter()
    }
}

fn bind_span(source: &str, start: u32, end: u32) -> Result<Utf8Span, CheckerError> {
    let span =
        Utf16Span::try_from(start..end).map_err(|_| CheckerError::SpanBinding { start, end })?;
    span.to_utf8(source)
        .map_err(|cause| span_fault(cause, start, end))
}

fn span_fault(cause: CoordinateError, start: u32, end: u32) -> CheckerError {
    let _ = cause;
    CheckerError::SpanBinding { start, end }
}

/// Computes the report source binding of exact source bytes.
#[must_use]
pub fn source_digest(source: &[u8]) -> [u8; DIGEST_BYTES] {
    Sha256::digest(source).into()
}

fn hex_digest(hex: &str) -> Option<[u8; DIGEST_BYTES]> {
    let bytes = hex.as_bytes();
    if bytes.len() != DIGEST_BYTES * 2 {
        return None;
    }
    let mut digest = [0_u8; DIGEST_BYTES];
    for (index, byte) in digest.iter_mut().enumerate() {
        let high = hex_digit(bytes.get(index.checked_mul(2)?)?)?;
        let low = hex_digit(bytes.get(index.checked_mul(2)?.checked_add(1)?)?)?;
        *byte = high.checked_mul(16)?.checked_add(low)?;
    }
    Some(digest)
}

fn hex_digit(byte: &u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(byte.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Configurable bounded subprocess adapter for the vendored checker driver.
#[derive(Debug, Clone, Copy)]
pub struct Checker {
    /// Maximum bytes retained and accepted from each child stream.
    pub output_limit: usize,
    /// Maximum wall-clock duration for the child.
    pub timeout: Duration,
}

/// Immutable facts for one caller-selected TypeScript checker executable.
///
/// The executable receives the staged source path as its only argument and
/// emits the vendored checker's report schema on standard output.  This is a
/// distinct authority mode from [`Checker`]'s compatibility entry points,
/// which may still resolve the historical environment override or `node`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeScriptCheckerProgram {
    view: TypeScriptCheckerProgramView,
}

/// Read-only view of a validated checker executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeScriptCheckerProgramView {
    /// Absolute program path supplied by the caller.
    pub executable: PathBuf,
}

/// Immutable facts for the caller-selected Node module search root that owns
/// the `typescript` package used by the vendored authority driver.
///
/// Keeping this path beside the Node executable makes checker authority
/// independent of the parent process's ambient `NODE_PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeScriptModuleRoot {
    view: TypeScriptModuleRootView,
}

/// Read-only view of one validated absolute Node module root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeScriptModuleRootView {
    /// Absolute directory whose `typescript` child is the selected compiler
    /// API implementation.
    pub directory: PathBuf,
}

impl core::ops::Deref for TypeScriptCheckerProgram {
    type Target = TypeScriptCheckerProgramView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl AsRef<Path> for TypeScriptCheckerProgram {
    fn as_ref(&self) -> &Path {
        &self.view.executable
    }
}

impl core::ops::Deref for TypeScriptModuleRoot {
    type Target = TypeScriptModuleRootView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl AsRef<Path> for TypeScriptModuleRoot {
    fn as_ref(&self) -> &Path {
        &self.view.directory
    }
}

/// Rejection while admitting an explicit TypeScript checker executable.
#[derive(Debug, thiserror::Error)]
pub enum TypeScriptCheckerProgramError {
    /// A relative path would defer authority selection to ambient search
    /// state, so it cannot enter a retained package authority transaction.
    #[error("TypeScript checker executable is not absolute: {executable:?}")]
    RelativeExecutable {
        /// Caller-supplied relative executable path.
        executable: PathBuf,
    },
    /// A relative module root would defer compiler-API selection to the
    /// child's working directory.
    #[error("TypeScript Node module root is not absolute: {directory:?}")]
    RelativeModuleRoot {
        /// Caller-supplied relative module root.
        directory: PathBuf,
    },
}

impl TypeScriptCheckerProgram {
    /// Admits one caller-selected absolute checker executable.
    pub fn new(executable: PathBuf) -> Result<Self, TypeScriptCheckerProgramError> {
        if !executable.is_absolute() {
            return Err(TypeScriptCheckerProgramError::RelativeExecutable { executable });
        }
        Ok(Self {
            view: TypeScriptCheckerProgramView { executable },
        })
    }
}

impl TypeScriptModuleRoot {
    /// Admits one caller-selected absolute Node module root.
    pub fn new(directory: PathBuf) -> Result<Self, TypeScriptCheckerProgramError> {
        if !directory.is_absolute() {
            return Err(TypeScriptCheckerProgramError::RelativeModuleRoot { directory });
        }
        Ok(Self {
            view: TypeScriptModuleRootView { directory },
        })
    }
}

/// A bounded TypeScript checker transaction whose executable was selected by
/// the caller before collection begins.
#[derive(Debug, Clone)]
pub struct ExplicitTypeScriptChecker {
    checker: Checker,
    invocation: ExplicitCheckerInvocation,
}

/// Closed explicit checker command grammar.
#[derive(Debug, Clone)]
enum ExplicitCheckerInvocation {
    /// A program that directly writes the checker report schema.
    ReportProgram(TypeScriptCheckerProgram),
    /// An explicit Node runtime that executes the vendored checker driver.
    Node {
        /// Exact Node runtime selected by the caller.
        program: TypeScriptCheckerProgram,
        /// Exact module root containing the selected `typescript` package.
        module_root: TypeScriptModuleRoot,
    },
}

impl ExplicitTypeScriptChecker {
    /// Returns a host-local fingerprint of the explicit checker command,
    /// package resolver root, bundled driver, and process bounds. Path bytes
    /// make this a drift detector rather than a cross-host closure identity.
    #[must_use]
    pub fn local_configuration_fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"compiler.typescript.package-authority.v1\0");
        digest.update(self.checker.output_limit.to_be_bytes());
        digest.update(self.checker.timeout.as_secs().to_be_bytes());
        digest.update(self.checker.timeout.subsec_nanos().to_be_bytes());
        match &self.invocation {
            ExplicitCheckerInvocation::ReportProgram(program) => {
                digest.update([0]);
                update_path_digest(&mut digest, program.as_ref());
            }
            ExplicitCheckerInvocation::Node {
                program,
                module_root,
            } => {
                digest.update([1]);
                update_path_digest(&mut digest, program.as_ref());
                update_path_digest(&mut digest, module_root.as_ref());
                digest.update(include_bytes!("checker/main.cjs"));
            }
        }
        digest.finalize().into()
    }

    /// Runs the explicit checker over one exact source file.
    pub fn run(&self, profile: TypeScriptSource, source: &[u8]) -> Result<Report, CheckerError> {
        self.checker
            .run_with_explicit_invocation(&self.invocation, profile, source)
    }

    /// Runs the explicit checker against a bounded, read-only package staging
    /// tree without consulting environment variables or `PATH`.
    pub fn run_in_package(
        &self,
        profile: TypeScriptSource,
        source: &[u8],
        package_root: &Path,
    ) -> Result<Report, CheckerError> {
        self.checker
            .run_in_package_with_invocation(&self.invocation, profile, source, package_root)
    }
}

fn update_path_digest(digest: &mut Sha256, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    digest.update(bytes.len().to_be_bytes());
    digest.update(bytes);
}

impl Default for Checker {
    fn default() -> Self {
        Self {
            output_limit: 16 * 1024 * 1024,
            timeout: Duration::from_secs(60),
        }
    }
}

impl Checker {
    /// Binds this bounded checker configuration to one caller-selected
    /// absolute checker executable.
    pub fn with_program(
        self,
        executable: PathBuf,
    ) -> Result<ExplicitTypeScriptChecker, TypeScriptCheckerProgramError> {
        Ok(ExplicitTypeScriptChecker {
            checker: self,
            invocation: ExplicitCheckerInvocation::ReportProgram(TypeScriptCheckerProgram::new(
                executable,
            )?),
        })
    }

    /// Binds this checker to a caller-selected absolute Node runtime that
    /// executes the vendored driver without an ambient `node` lookup.
    pub fn with_node(
        self,
        executable: PathBuf,
        module_root: PathBuf,
    ) -> Result<ExplicitTypeScriptChecker, TypeScriptCheckerProgramError> {
        Ok(ExplicitTypeScriptChecker {
            checker: self,
            invocation: ExplicitCheckerInvocation::Node {
                program: TypeScriptCheckerProgram::new(executable)?,
                module_root: TypeScriptModuleRoot::new(module_root)?,
            },
        })
    }

    /// Decodes one report transcript without starting a process.
    ///
    /// # Errors
    ///
    /// Returns [`CheckerError::Decode`] for malformed transcripts and
    /// [`CheckerError::Staleness`] for a foreign schema version.
    pub fn decode(&self, bytes: &[u8]) -> Result<Report, CheckerError> {
        let report: Report =
            serde_json::from_slice(bytes).map_err(|error| CheckerError::Decode {
                message: error.to_string(),
                transcript: transcript_prefix(bytes),
            })?;
        if report.schema_version != REQUIRED_SCHEMA_VERSION {
            return Err(CheckerError::Staleness {
                found: report.schema_version,
                expected: REQUIRED_SCHEMA_VERSION,
            });
        }
        Ok(report)
    }

    /// Runs the vendored checker over one exact source under one profile.
    ///
    /// The source is written to one bounded work directory as the checker's
    /// single input file, the child is driven with bounded output and a
    /// deadline, and the report is decoded and bound to the exact source.
    ///
    /// # Errors
    ///
    /// Returns every [`CheckerError`] cause; tool and module unavailability
    /// are distinct typed causes so callers can report the exact rejection.
    pub fn run(&self, profile: TypeScriptSource, source: &[u8]) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = self.run_in(&work, profile, source);
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs the checker against a read-only staged copy of a package tree.
    ///
    /// The source is installed as the package root's `index.ts` (or `index.tsx`)
    /// in the staged tree. The caller's tree is only read, so module resolution
    /// sees its relative imports and package-local `node_modules` without
    /// granting the child write access to caller-owned files.
    pub fn run_in_package(
        &self,
        profile: TypeScriptSource,
        source: &[u8],
        package_root: &Path,
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let started = Instant::now();
        let run = (|| {
            let staged = work.join("package");
            let mut budget = PackageBudget::new(started, self.timeout);
            stage_package(package_root, &staged, &mut budget)?;
            let file = staged.join(package_entry_file(profile));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            match std::env::var("NUDOX_TYPESCRIPT_CHECKER_BIN") {
                Ok(binary) => self.run_child(&work, Path::new(&binary), &file),
                Err(std::env::VarError::NotPresent) => {
                    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/legacy/checker")
                        .join("main.cjs");
                    let mut command = Command::new("node");
                    command.arg(driver).arg(&file);
                    self.run_child_prepared(&work, command, &file)
                }
                Err(cause) => Err(CheckerError::ToolingUnavailable {
                    tool: "NUDOX_TYPESCRIPT_CHECKER_BIN",
                    source: std::io::Error::other(cause),
                }),
            }
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs a package-staged authority transaction through one already
    /// admitted explicit command. The public explicit capability calls this
    /// rather than the compatibility environment/PATH resolution above.
    fn run_in_package_with_invocation(
        &self,
        invocation: &ExplicitCheckerInvocation,
        profile: TypeScriptSource,
        source: &[u8],
        package_root: &Path,
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let started = Instant::now();
        let run = (|| {
            let staged = work.join("package");
            let mut budget = PackageBudget::new(started, self.timeout);
            stage_package(package_root, &staged, &mut budget)?;
            let file = staged.join(package_entry_file(profile));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            self.run_explicit_file(invocation, &work, &file)
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    /// Runs the vendored checker through one explicit child program.
    ///
    /// The program receives the work source file as its single argument and
    /// must print the report on stdout. This is the typed resolution seam
    /// used by the vendored driver tests; production runs go through
    /// [`Checker::run`], which resolves `NUDOX_TYPESCRIPT_CHECKER_BIN` or
    /// `node` with the vendored driver.
    ///
    /// # Errors
    ///
    /// Returns the same typed causes as [`Checker::run`].
    pub fn run_with_program(
        &self,
        program: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = self.run_program_in(&work, program, profile, source);
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    fn run_in(
        &self,
        work: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let file = work.join(source_file(profile));
        std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        match std::env::var("NUDOX_TYPESCRIPT_CHECKER_BIN") {
            Ok(binary) => self.run_child(work, Path::new(&binary), &file),
            Err(std::env::VarError::NotPresent) => {
                let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/legacy/checker")
                    .join("main.cjs");
                let mut command = Command::new("node");
                command.arg(driver).arg(&file);
                self.run_child_prepared(work, command, &file)
            }
            Err(cause) => Err(CheckerError::ToolingUnavailable {
                tool: "NUDOX_TYPESCRIPT_CHECKER_BIN",
                source: std::io::Error::other(cause),
            }),
        }
    }

    fn run_program_in(
        &self,
        work: &Path,
        program: &Path,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let file = work.join(source_file(profile));
        std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        self.run_child(work, program, &file)
    }

    fn run_with_explicit_invocation(
        &self,
        invocation: &ExplicitCheckerInvocation,
        profile: TypeScriptSource,
        source: &[u8],
    ) -> Result<Report, CheckerError> {
        let work = work_directory();
        std::fs::create_dir(&work).map_err(|cause| CheckerError::Work {
            phase: "prepare",
            source: cause,
        })?;
        let run = (|| {
            let file = work.join(source_file(profile));
            std::fs::write(&file, source).map_err(|cause| CheckerError::Work {
                phase: "prepare",
                source: cause,
            })?;
            self.run_explicit_file(invocation, &work, &file)
        })();
        match std::fs::remove_dir_all(&work) {
            Ok(()) => run,
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => run,
            Err(cause) => match run {
                Ok(_) => Err(CheckerError::Work {
                    phase: "cleanup",
                    source: cause,
                }),
                Err(primary) => Err(primary),
            },
        }
    }

    fn run_explicit_file(
        &self,
        invocation: &ExplicitCheckerInvocation,
        work: &Path,
        file: &Path,
    ) -> Result<Report, CheckerError> {
        match invocation {
            ExplicitCheckerInvocation::ReportProgram(program) => {
                self.run_child(work, program.as_ref(), file)
            }
            ExplicitCheckerInvocation::Node {
                program,
                module_root,
            } => {
                let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/legacy/checker")
                    .join("main.cjs");
                let mut command = Command::new(program.as_ref());
                command.env("NODE_PATH", module_root.as_ref());
                command.arg(driver).arg(file);
                self.run_child_prepared(work, command, file)
            }
        }
    }

    fn run_child(&self, work: &Path, program: &Path, file: &Path) -> Result<Report, CheckerError> {
        let mut command = Command::new(program);
        command.arg(file);
        self.run_child_prepared(work, command, file)
    }

    fn run_child_prepared(
        &self,
        work: &Path,
        mut command: Command,
        file: &Path,
    ) -> Result<Report, CheckerError> {
        let _ = work;
        let _ = file;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().map_err(|source| CheckerError::Spawn {
            program: format!("{:?}", command.get_program()),
            source,
        })?;
        let stdout = child.stdout.take().ok_or_else(|| CheckerError::Pipe {
            stream: "stdout",
            source: std::io::Error::other("stdout was not piped"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| CheckerError::Pipe {
            stream: "stderr",
            source: std::io::Error::other("stderr was not piped"),
        })?;
        let limit = self.output_limit;
        let (limit_sender, limit_receiver) = std::sync::mpsc::channel();
        let out_sender = limit_sender.clone();
        let out_thread =
            std::thread::spawn(move || read_bounded(stdout, limit, "stdout", out_sender));
        let err_thread =
            std::thread::spawn(move || read_bounded(stderr, limit, "stderr", limit_sender));
        let started = Instant::now();
        let mut limit_failure = None;
        let terminal = loop {
            if let Ok((stream, observed)) = limit_receiver.try_recv() {
                limit_failure = Some((stream, observed));
                terminate_child(&mut child);
                break None;
            }
            if let Some(status) = child.try_wait().map_err(|cause| CheckerError::Pipe {
                stream: "process",
                source: cause,
            })? {
                break Some(status);
            }
            if started.elapsed() >= self.timeout {
                terminate_child(&mut child);
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let out_bytes = out_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardOutputReader,
                    payload.as_ref(),
                ),
            })?;
        let err_bytes = err_thread
            .join()
            .map_err(|payload| CheckerError::WorkerPanic {
                cause: NativeWorkerPanic::capture(
                    NativeWorker::StandardErrorReader,
                    payload.as_ref(),
                ),
            })?;
        if let Some((stream, cause)) = out_bytes.error.or(err_bytes.error) {
            return Err(CheckerError::Pipe {
                stream,
                source: cause,
            });
        }
        if let Some((stream, observed)) =
            limit_failure.or(out_bytes.exceeded).or(err_bytes.exceeded)
        {
            return Err(CheckerError::OutputLimit {
                phase: "collect",
                stream,
                observed,
                limit,
            });
        }
        let Some(status) = terminal else {
            return Err(CheckerError::Timeout {
                phase: "collect",
                milliseconds: self.timeout.as_millis(),
            });
        };
        let stderr_tail = tail(&err_bytes.bytes);
        if !status.success() {
            if status.code() == Some(MODULE_MISSING_EXIT) {
                return Err(CheckerError::ModuleUnavailable {
                    stderr: stderr_tail,
                });
            }
            return Err(CheckerError::Exit {
                status: status.to_string(),
                stderr: stderr_tail,
            });
        }
        self.decode(&out_bytes.bytes)
    }
}

/// The bounded work-file name of one grammar profile.
#[must_use]
pub(crate) const fn source_file(profile: TypeScriptSource) -> &'static str {
    match profile {
        TypeScriptSource::TypeScript => "compiler-probe.ts",
        TypeScriptSource::Tsx => "compiler-probe.tsx",
    }
}

const fn package_entry_file(profile: TypeScriptSource) -> &'static str {
    match profile {
        TypeScriptSource::TypeScript => "index.ts",
        TypeScriptSource::Tsx => "index.tsx",
    }
}

struct PackageBudget {
    started: Instant,
    timeout: Duration,
    files: usize,
    bytes: usize,
}

impl PackageBudget {
    fn new(started: Instant, timeout: Duration) -> Self {
        Self {
            started,
            timeout,
            files: 0,
            bytes: 0,
        }
    }

    fn check(&mut self, size: usize) -> Result<(), CheckerError> {
        if self.started.elapsed() >= self.timeout {
            return Err(CheckerError::PackageTimeout {
                milliseconds: self.timeout.as_millis(),
            });
        }
        let files = self.files.saturating_add(1);
        if files > PACKAGE_FILE_LIMIT {
            return Err(CheckerError::PackageFileLimit {
                observed: files,
                limit: PACKAGE_FILE_LIMIT,
            });
        }
        let bytes = self.bytes.saturating_add(size);
        if bytes > PACKAGE_BYTE_LIMIT {
            return Err(CheckerError::PackageByteLimit {
                observed: bytes,
                limit: PACKAGE_BYTE_LIMIT,
            });
        }
        self.files = files;
        self.bytes = bytes;
        Ok(())
    }
}

fn stage_package(
    source: &Path,
    destination: &Path,
    budget: &mut PackageBudget,
) -> Result<(), CheckerError> {
    let metadata = std::fs::symlink_metadata(source).map_err(|cause| CheckerError::Work {
        phase: "stage metadata",
        source: cause,
    })?;
    if !metadata.is_dir() {
        return Err(CheckerError::Work {
            phase: "stage root",
            source: std::io::Error::other("package root is not a directory"),
        });
    }
    std::fs::create_dir_all(destination).map_err(|cause| CheckerError::Work {
        phase: "stage directory",
        source: cause,
    })?;
    for entry in std::fs::read_dir(source).map_err(|cause| CheckerError::Work {
        phase: "stage directory",
        source: cause,
    })? {
        if budget.started.elapsed() >= budget.timeout {
            return Err(CheckerError::PackageTimeout {
                milliseconds: budget.timeout.as_millis(),
            });
        }
        let entry = entry.map_err(|cause| CheckerError::Work {
            phase: "stage directory",
            source: cause,
        })?;
        let child = entry.path();
        let target = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&child).map_err(|cause| CheckerError::Work {
            phase: "stage metadata",
            source: cause,
        })?;
        if metadata.is_dir() {
            stage_package(&child, &target, budget)?;
        } else if metadata.is_file() {
            let remaining = PACKAGE_BYTE_LIMIT.saturating_sub(budget.bytes);
            let mut bytes = Vec::new();
            std::fs::File::open(&child)
                .map_err(|cause| CheckerError::Work {
                    phase: "stage read",
                    source: cause,
                })?
                .take(remaining.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|cause| CheckerError::Work {
                    phase: "stage read",
                    source: cause,
                })?;
            if bytes.len() > remaining {
                return Err(CheckerError::PackageByteLimit {
                    observed: budget.bytes.saturating_add(bytes.len()),
                    limit: PACKAGE_BYTE_LIMIT,
                });
            }
            budget.check(bytes.len())?;
            std::fs::write(&target, bytes).map_err(|cause| CheckerError::Work {
                phase: "stage write",
                source: cause,
            })?;
        } else {
            return Err(CheckerError::Work {
                phase: "stage entry",
                source: std::io::Error::other("package entry is not a regular file or directory"),
            });
        }
    }
    Ok(())
}

fn work_directory() -> PathBuf {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "nudox-ts-checker-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
    ))
}

struct BoundedBytes {
    bytes: Vec<u8>,
    exceeded: Option<(&'static str, usize)>,
    error: Option<(&'static str, std::io::Error)>,
}

fn read_bounded(
    mut reader: impl Read,
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
                let _ = sender.send((stream, observed));
                return BoundedBytes {
                    bytes,
                    exceeded: Some((stream, observed)),
                    error: None,
                };
            }
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(cause) if cause.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(cause) => {
                return BoundedBytes {
                    bytes,
                    exceeded: None,
                    error: Some((stream, cause)),
                };
            }
        }
    }
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let group = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
        if !group.is_ok_and(|status| status.success()) {
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
    String::from_utf8_lossy(&bytes[..bytes.len().min(TRANSCRIPT_PREFIX_LIMIT)]).into_owned()
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(TRANSCRIPT_TAIL_LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod capability_tests {
    use std::path::PathBuf;

    use super::{Checker, TypeScriptCheckerProgramError};

    #[test]
    fn explicit_checker_rejects_relative_executable_before_child_work() {
        let error = Checker::default()
            .with_program(PathBuf::from("checker"))
            .expect_err("relative executable must not enter authority configuration");
        assert!(matches!(
            error,
            TypeScriptCheckerProgramError::RelativeExecutable { executable }
                if executable == PathBuf::from("checker")
        ));
    }

    #[test]
    fn explicit_node_checker_rejects_relative_module_root_before_child_work() {
        let error = Checker::default()
            .with_node(
                PathBuf::from("/configured/node"),
                PathBuf::from("node_modules"),
            )
            .expect_err("relative module root must not enter authority configuration");
        assert!(matches!(
            error,
            TypeScriptCheckerProgramError::RelativeModuleRoot { directory }
                if directory == PathBuf::from("node_modules")
        ));
    }
}
