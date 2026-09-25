//! Pass-1 extraction: parse one module into owned [`ModuleFacts`].
//!
//! Each module gets its own `Allocator` scoped to the extraction block;
//! `ModuleFacts` contains no arena references. This is how the OXC lifetime
//! problem is solved: all data is owned before the arena is dropped.

use std::{collections::HashMap, path::PathBuf};

pub mod decl;
pub mod jsdoc;
pub mod types;

// ── Error types ───────────────────────────────────────────────────────────────

/// A failure during per-module extraction.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// An OXC AST construct that has no IR representation.
    #[error("unsupported construct")]
    UnsupportedConstruct { symbol: String, description: String },
    /// A type expression could not be lowered.
    #[error("type lowering failed")]
    TypeLowering { context: String, detail: String },
    /// Parser panic.
    #[error("parser panicked")]
    ParsePanic { path: PathBuf, detail: String },
}

/// Package-level errors (entry discovery, graph build, I/O).
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("i/o error")]
    Io(#[from] std::io::Error),
    #[error("serialization error")]
    Serialization(#[from] serde_json::Error),
    #[error("extraction error")]
    Extract(#[from] ExtractError),
    // No "entry point discovery failed" variant: `entry.rs::discover_entry_points_with`
    // used to construct one when it found zero entry points, but that made a
    // genuinely empty package indistinguishable from a broken walk — see the
    // doc comment at the end of that function for why it now returns `Ok(vec![])`
    // instead and lets `enforce_yield_contract` reject the package by name
    // (`NoDeclarationsContributed`).
    #[error("parse failed")]
    ParseFailed { path: PathBuf, detail: String },
}

pub type Result<T> = std::result::Result<T, ExtractError>;

// ── JSDoc facts ───────────────────────────────────────────────────────────────

/// A locally-owned clone-able deprecation note.
///
/// `nudox_ir::entry::Deprecation` does not derive `Clone`; this mirrors its
/// fields but adds `Clone` so `DocFacts` can derive it.
#[derive(Debug, Clone, Default)]
pub struct DeprecationOwned {
    pub note: Option<String>,
    pub since: Option<String>,
}

impl DeprecationOwned {
    /// Convert to the IR's `Deprecation` type (which does not clone).
    pub fn into_ir(self) -> nudox_ir::entry::Deprecation {
        nudox_ir::entry::Deprecation {
            note: self.note,
            since: self.since,
        }
    }
}

impl From<nudox_ir::entry::Deprecation> for DeprecationOwned {
    fn from(d: nudox_ir::entry::Deprecation) -> Self {
        DeprecationOwned {
            note: d.note,
            since: d.since,
        }
    }
}

/// Parsed JSDoc for one node.
#[derive(Debug, Clone, Default)]
pub struct DocFacts {
    pub doc: Option<String>,
    pub deprecation: Option<DeprecationOwned>,
    pub ignore: bool,
}

// ── Per-declaration data ──────────────────────────────────────────────────────

/// Typed modifiers on a TypeScript declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Accessibility {
    #[default]
    Public,
    Protected,
    Private,
    PrivateField,
}

/// Modifiers that can appear on class members.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemberModifiers {
    pub accessibility: Accessibility,
    pub is_static: bool,
    pub is_readonly: bool,
    pub is_optional: bool,
    pub is_abstract: bool,
}

// ── Import / export tables ─────────────────────────────────────────────────────

/// How a name was imported.
#[derive(Debug, Clone)]
pub enum ImportName {
    Named(String),
    Default,
    Namespace,
}

/// One import entry, fully owned.
#[derive(Debug, Clone)]
pub struct ImportFact {
    pub module_request: String,
    pub import_name: ImportName,
    pub local_name: String,
    pub is_type: bool,
}

/// A named re-export (`export { x } from "m"`).
#[derive(Debug, Clone)]
pub struct IndirectExport {
    pub module_request: String,
    pub import_name: String,
    pub export_name: String,
    /// Byte span of the whole `export { .. } from "m";` statement
    /// (`oxc_syntax::module_record::ExportEntry::statement_span`), used as
    /// the re-export's own declared span so it does not fall back to a
    /// degenerate `0..0` (see `emit.rs::emit_reexports`).
    pub span_start: u32,
    pub span_end: u32,
}

/// A star re-export (`export * from "m"`).
#[derive(Debug, Clone)]
pub struct StarExport {
    pub module_request: String,
    /// Byte span of the whole `export * from "m";` statement. Every name
    /// fanned out from one star export shares this span — they are distinct
    /// declarations (different names) sharing one real source location, not
    /// distinct locations, so a shared span cannot introduce a collision: the
    /// identity key includes the name.
    pub span_start: u32,
    pub span_end: u32,
}

/// How an externally-visible export name — one this module exports without
/// a `from` clause — resolves to something the emitter actually produced.
///
/// A module's export surface and what it `declare()`d as `TsId`s can diverge
/// in two ways real npm packages both exercise:
///
/// - A bare rename (`export { anyType as any }` — zod's `types.d.ts`;
///   `export { format as formatDate }` — date-fns's `format.d.ts`) declares
///   under the local name (`anyType`, `format`), never under the alias.
/// - A re-exported namespace import (`import * as z from "./external"; export
///   { z };` — zod's `index.d.ts`) has no `DeclFact` at all: `z` is only ever
///   an import binding, and `extract_statement` has no dispatch arm for
///   `Statement::ImportDeclaration`.
///
/// Any code that turns an export name into a `refer()`'d `TsId` — the named
/// indirect re-export path and the star-export fan-out, both in `emit.rs` —
/// must resolve through this first. Building the `TsId` directly from the
/// export name, as both paths used to, points at whatever nothing ever
/// `declare()`d and `Lowering::finish` rejects the whole package as
/// "referred but never declared".
#[derive(Debug, Clone)]
pub enum LocalExport {
    /// Target `TsId::new(<this module>, local_name, d)` for every
    /// `d in 0..overload_count` — this module's own emitter declared
    /// `overload_count` declaration(s) under `local_name`.
    ///
    /// `overload_count` is greater than 1 exactly when `local_name` is a
    /// top-level overloaded function (`bump_count`'s discriminants run
    /// `0..overload_count` for such a group; see `TsId`'s doc comment).
    /// Before this carried the count, `resolve_export_target` (emit.rs)
    /// hardcoded discriminant `0` for every named re-export target, so a
    /// re-export of an overloaded function could only ever reach its first
    /// overload — real cost on rxjs, whose public root re-exports
    /// `combineLatest` (13 real overloads in
    /// `internal/observable/combineLatest.d.ts`) by name.
    Named {
        local_name: String,
        overload_count: u32,
    },
    /// Target the *root* of the module reached from this module by the
    /// specifier `module_request` — the namespace object itself has no
    /// per-symbol `TsId`.
    NamespaceOf(String),
    /// This module exports something under this name with no nameable
    /// target at all (e.g. `export default "a literal";` — no identifier,
    /// so no `DeclFact`, so nothing to point a `TsId` at). Callers must skip
    /// emitting any reference for this name rather than falling back to the
    /// export name itself, which is exactly as undeclared.
    Unresolvable,
}

/// Export surface extracted from the module record.
#[derive(Debug, Clone, Default)]
pub struct ExportTable {
    pub exported_names: Vec<String>,
    pub indirect: Vec<IndirectExport>,
    pub star: Vec<StarExport>,
    pub default_local_name: Option<String>,
    /// Resolution for every name this module exports *without* a `from`
    /// clause (`export { x }`, `export { x as y }`, `export default ...`),
    /// keyed by the externally-visible name. A name reachable through this
    /// module (present in `exported_names`, or usable as an indirect
    /// re-export's source name elsewhere) but absent here is a genuine
    /// indirect re-export (`export { x } from "m"`) — already correctly
    /// targetable by name unchanged, because the *owning* module's own
    /// `emit_reexports` call declares it under that exact alias via
    /// `declare_ref`. See [`LocalExport`].
    pub locals: HashMap<String, LocalExport>,
}

// ── One lowered declaration ────────────────────────────────────────────────────

/// One declaration lowered from OXC AST into IR-ready owned data.
///
/// Because `Lowering` is order-independent, we do NOT need a path map or an
/// intermediate tree. We emit into `Lowering` directly during `lower()`.
/// `DeclFact` is the per-declaration carrier between extraction pass and emit.
#[derive(Debug)]
pub struct DeclFact {
    /// The declaration name.
    pub name: String,
    /// Visibility derived from export/ambient status.
    pub visibility: nudox_ir::entry::Visibility,
    /// JSDoc.
    pub doc: DocFacts,
    /// The kind-specific body.
    pub body: DeclBody,
    /// Absolute path of the source file.
    pub module: PathBuf,
    /// Byte span within the file.
    pub span_start: u32,
    pub span_end: u32,
    /// Whether this is `export default`.
    pub is_default: bool,
    /// Declaration index within same-named group (for discriminant).
    pub decl_index: u32,
}

/// The kind-specific payload for one declaration.
#[derive(Debug)]
pub enum DeclBody {
    Interface(InterfaceBody),
    Class(ClassBody),
    TypeAlias(TypeAliasBody),
    Enum(EnumBody),
    Namespace(NamespaceBody),
    Function(FunctionBody),
    Const(ConstBody),
    Static(StaticBody),
    /// A re-export: `export { name } from "mod"`.
    Reexport {
        module_request: String,
        import_name: String,
    },
}

#[derive(Debug)]
pub struct InterfaceBody {
    pub generics: Vec<GenericParamOwned>,
    pub extends: Vec<TypeOwned>,
    pub methods: Vec<MethodFact>,
    pub properties: Vec<PropertyFact>,
    pub call_signatures: Vec<FunctionBody>,
    /// `[k: string]: T` index signatures — emitted as synthetic `__index` methods.
    pub index_signatures: Vec<IndexSignatureFact>,
    /// `new (…): T` construct signatures — emitted as synthetic `new` methods.
    pub construct_signatures: Vec<FunctionBody>,
}

#[derive(Debug)]
pub struct ClassBody {
    pub generics: Vec<GenericParamOwned>,
    pub extends: Vec<TypeOwned>,
    pub implements: Vec<TypeOwned>,
    pub members: Vec<MemberFact>,
    pub index_signatures: Vec<IndexSignatureFact>,
    pub is_abstract: bool,
    /// Decorators on the class itself.
    pub decorators: Vec<AttrTok>,
}

#[derive(Debug)]
pub struct TypeAliasBody {
    pub generics: Vec<GenericParamOwned>,
    pub target: TypeOwned,
}

#[derive(Debug)]
pub struct EnumBody {
    pub is_const: bool,
    pub variants: Vec<VariantFact>,
}

#[derive(Debug)]
pub struct NamespaceBody {
    pub is_ambient: bool,
    pub children: Vec<DeclFact>,
}

#[derive(Debug, Clone)]
pub struct FunctionBody {
    pub generics: Vec<GenericParamOwned>,
    pub params: Vec<ParamFact>,
    pub return_type: Option<TypeOwned>,
    pub is_async: bool,
    pub is_generator: bool,
    /// True for the implementation signature; false for overload signatures.
    pub has_body: bool,
    pub receiver: ReceiverKind,
    /// Type of an explicit `this` parameter, when the source wrote one.
    pub this_ty: Option<TypeOwned>,
    /// `abstract new` constructor types. Ordinary functions leave this false.
    pub abstract_construct: bool,
    /// Source of the function body, when one was written.
    pub body_text: Option<String>,
    /// Comment attached to a call or construct signature.
    pub leading_doc: Option<String>,
    /// Byte span of the declaring node: the `Function` AST node for a
    /// top-level function or a class method/constructor's value, or the
    /// whole signature node (`TSMethodSignature`, `TSCallSignatureDeclaration`,
    /// `TSConstructSignatureDeclaration`) for an interface member. Real bytes
    /// from OXC, not a placeholder — see `emit.rs`'s `span: 0..0` removal.
    pub span_start: u32,
    pub span_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    None,
    SharedRef,
    MutRef,
}

#[derive(Debug)]
pub struct ConstBody {
    pub ty: Option<TypeOwned>,
    pub value: Option<String>,
}

#[derive(Debug)]
pub struct StaticBody {
    pub ty: Option<TypeOwned>,
    pub value: Option<String>,
    pub is_mutable: bool,
}

/// An index signature member: `[k: KeyName: KeyType]: ValueType`.
#[derive(Debug)]
pub struct IndexSignatureFact {
    /// The key parameter name (e.g. `"k"` in `[k: string]`).
    pub key_name: String,
    /// The key type.
    pub key_ty: TypeOwned,
    /// The value type.
    pub value_ty: TypeOwned,
    pub readonly: bool,
    pub is_static: bool,
    pub doc: Option<String>,
    /// Byte span of the `TSIndexSignature` node.
    pub span_start: u32,
    pub span_end: u32,
}

/// A decorator stored on a class or member.
#[derive(Debug, Clone)]
pub struct AttrTok {
    /// The raw source text of the decorator (e.g. `"@injectable"` or `"@MyDecorator(opts)"`).
    pub token: String,
}

#[derive(Debug)]
pub struct VariantFact {
    pub name: String,
    pub discriminant: Option<String>,
    /// Byte span of the `TSEnumMember` node.
    pub span_start: u32,
    pub span_end: u32,
}

#[derive(Debug)]
pub struct PropertyFact {
    pub name: String,
    pub ty: Option<TypeOwned>,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
    /// Byte span of the `TSPropertySignature` node.
    pub span_start: u32,
    pub span_end: u32,
}

#[derive(Debug)]
pub struct MethodFact {
    pub name: String,
    pub sig: FunctionBody,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
    /// True for overload signatures.
    pub is_overload: bool,
    /// `get` / `set` / method on an interface signature.
    pub signature_kind: SignatureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum SignatureKind {
    #[default]
    Method,
    Get,
    Set,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClassFlags {
    pub declare: bool,
    pub override_: bool,
    pub definite: bool,
}

#[derive(Debug)]
pub struct MemberFact {
    pub name: String,
    pub kind: MemberKind,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
    /// Decorators on the member (e.g. `@readonly`, `@Column()`).
    pub decorators: Vec<AttrTok>,
    pub class_flags: ClassFlags,
    pub initializer: Option<String>,
    pub signature_kind: SignatureKind,
    /// Byte span of the declaring class-element node (`MethodDefinition`,
    /// `PropertyDefinition`, `AccessorProperty`, `StaticBlock`), or of the
    /// synthesizing constructor-parameter / `this.x = ...` assignment for a
    /// synthesized field. Real bytes, not `0..0` — see `emit.rs`.
    pub span_start: u32,
    pub span_end: u32,
}

#[derive(Debug)]
pub enum MemberKind {
    Property {
        ty: Option<TypeOwned>,
    },
    Method(Vec<FunctionBody>),
    Constructor(FunctionBody),
    /// TC39 `accessor x: T` — auto-generates a getter/setter pair.
    Accessor {
        ty: Option<TypeOwned>,
    },
    /// `static { … }` initializer block — synthetic `__static[_N]` function.
    StaticBlock {
        name: String,
    },
}

#[derive(Debug, Clone)]
pub struct ParamFact {
    pub name: String,
    pub ty: Option<TypeOwned>,
    pub is_optional: bool,
    pub is_rest: bool,
    pub is_readonly: bool,
    pub initializer: Option<String>,
    pub decorators: Vec<AttrTok>,
    /// Byte span of the `FormalParameter` (or `BindingRestElement` for a
    /// rest parameter) node.
    pub span_start: u32,
    pub span_end: u32,
}

// ── Owned type expressions ────────────────────────────────────────────────────

/// A fully-owned type expression (no arena references).
///
/// This is the output of `types::lower_ts_type`; it maps 1:1 onto
/// `nudox_ir::kinds::Type` but is independently owned so it can cross the
/// arena boundary.
/// A fully-owned type expression (no arena references).
///
/// # TypeVar vs Nominal
///
/// `TypeVar(name)` represents a use of a *generic type parameter* — e.g. `T`
/// in `Array<T>`.  `Nominal(name)` represents a reference to a declared type
/// (class, interface, alias).  The distinction matters for the IR:
/// `Type::TypeVar` carries a plain string; `Type::Nominal` carries a `RawRef`
/// (content-addressed after sealing).
///
/// `nudox_ir::kinds::ty::Type::TypeVar(String)` exists and `emit::lower_type`
/// maps `TypeOwned::TypeVar` straight onto it.  `TypeOwned::Nominal` still
/// degrades to `Primitive::Builtin(name)` in that helper because constructing
/// `Type::Nominal(RawRef)` requires the `Lowering<TsId>` sink, which the pure
/// helper does not hold — an acquisition-boundary limitation, not an IR gap.
#[derive(Debug, Clone)]
pub enum TypeOwned {
    Any,
    Never,
    Unknown,
    Void,
    Undefined,
    Null,
    Bool,
    Number,
    BigInt,
    String,
    Symbol,
    Object,
    This,
    Primitive(String),
    /// A reference to a declared nominal type (class / interface / alias).
    Nominal(String),
    /// A use of a generic type parameter (e.g. `T`, `K`, `V`).
    ///
    /// Emitted as `Type::TypeVar(name)`.  See the seam note on `TypeOwned` above.
    TypeVar(String),
    Apply {
        base: Box<TypeOwned>,
        args: Vec<TypeOwned>,
    },
    Union(Vec<TypeOwned>),
    Intersection(Vec<TypeOwned>),
    Tuple(Vec<TypeOwned>),
    /// A named tuple element: `label: T` inside a `TSTupleType`.
    ///
    /// The label survives into the IR: `Type::Tuple` carries
    /// `List<TupleElement>` and `emit::lower_type` maps this to
    /// `TupleElement::Named { label, ty }`.  A `NamedTupleElem` appearing
    /// outside a `Tuple` is wrapped in a single-element named tuple so the
    /// label is still not lost.
    NamedTupleElem {
        label: String,
        ty: Box<TypeOwned>,
    },
    Array(Box<TypeOwned>),
    Function(Box<FunctionBody>),
    Literal(LiteralOwned),
    Unsupported(String),

    /// A TypeScript conditional type: `T extends string ? A : B`.
    /// Mapped directly to `Type::Conditional` in nudox-ir.
    Conditional {
        check: Box<TypeOwned>,
        extends_ty: Box<TypeOwned>,
        then_ty: Box<TypeOwned>,
        else_ty: Box<TypeOwned>,
    },

    /// A TypeScript mapped type: `{ readonly [P in keyof T]?: T[P] }`.
    /// `readonly` and `optional` are tri-state (Add/Remove/Absent).
    Mapped {
        key_var: String,
        source: Box<TypeOwned>,
        value: Box<TypeOwned>,
        readonly: nudox_ir::kinds::ty::MappedModifier,
        optional: nudox_ir::kinds::ty::MappedModifier,
        /// The `as` clause, when the mapped type remaps its key.
        name_type: Option<Box<TypeOwned>>,
    },

    /// A TypeScript template literal type: `` `prefix-${T}` ``.
    TemplateLiteral(Vec<TemplatePart>),

    /// An object type literal: `{ x: number; y?: string }`.
    /// Mapped to `Type::AnonymousRecord { form: Struct, ... }`.
    ObjectLiteral(Vec<AnonFieldOwned>),
}

#[derive(Debug, Clone)]
pub enum TemplatePart {
    Literal(String),
    Interpolated(Box<TypeOwned>),
}

#[derive(Debug, Clone)]
pub struct AnonFieldOwned {
    pub name: String,
    pub ty: TypeOwned,
    pub optional: bool,
    pub readonly: bool,
    /// Key type of an index signature. Absent on ordinary fields. The field
    /// type stays the value type; the key is walked on its own so a function
    /// used as a key is still declared.
    pub index_key: Option<TypeOwned>,
}

#[derive(Debug, Clone)]
pub enum LiteralOwned {
    Bool(bool),
    Number(String),
    String(String),
    BigInt(String),
    Null,
    Undefined,
}

/// A fully-owned generic parameter.
///
/// `variance` carries the TypeScript 4.7+ declaration-site `in`/`out` variance
/// annotation.  `None` means no annotation was present (the common case).
/// `Some(Covariant)` = `out T`, `Some(Contravariant)` = `in T`.
/// OXC 0.139.0 exposes this via `TSTypeParameter::r#in` and `TSTypeParameter::out`.
#[derive(Debug, Clone)]
pub struct GenericParamOwned {
    pub name: String,
    pub bounds: Vec<TypeOwned>,
    pub default: Option<TypeOwned>,
    /// Declaration-site variance annotation from TS 4.7+ `in`/`out` modifiers.
    /// `None` = no annotation (invariant by convention for most TS params).
    pub variance: Option<nudox_ir::kinds::ty::Variance>,
}

// ── Reference occurrences ────────────────────────────────────────────────────

/// Producer-local category for a same-module occurrence, distinguishing what
/// OXC's resolved reference graph can tell apart *without* type inference:
/// whether the reference site is a call expression's callee, a type-position
/// use, or an ordinary value read/write. `emit.rs::lower_package` maps each
/// variant onto `nudox_ir::vocab::ReferenceKind` and picks a `Confidence`
/// there — that decision lives next to the IR vocabulary it is choosing
/// between, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccurrenceKind {
    /// The reference is exactly the callee of a `CallExpression`:
    /// `runTool(task)`, not `const f = runTool;` (a plain value read of the
    /// same binding).
    Call,
    /// The reference occurs in a type position — `ReferenceFlags::Type` in
    /// OXC's binder, a resolution namespace kept separate from values so a
    /// value and a type can share a name without colliding.
    Type,
    /// Any other resolved value reference: a plain read, a write, or a value
    /// passed around without being called.
    ValueUse,
}

/// One resolved same-module reference edge, built from OXC's `Semantic`
/// symbol/reference graph once `ModuleFacts::declarations` is complete (see
/// `decl.rs::record_occurrences`).
///
/// # Same-module only
///
/// `owner` and `target` are indices into *this* module's own
/// `ModuleFacts::declarations` — nothing else. A reference that crosses a
/// module boundary (an imported binding used here, or this module's own
/// export used by another module) is invisible to OXC's per-file `Semantic`:
/// binding resolution stops at the file's own scope tree. Reaching across an
/// import to the target module's real declaration needs the same
/// resolver/export-table machinery `emit.rs` already has for re-exports,
/// wired up as a deliberate second step, not attempted here. Until it lands,
/// a cross-module call records nothing — a real, documented gap rather than
/// a silent wrong answer, which is exactly what `refs`'s coverage-honesty
/// gate (see the MCP layer) exists to keep from being misread as "no
/// callers".
///
/// Only *top-level* declarations are eligible as `owner`/`target`: a call
/// between two members of the same namespace (both nested inside one
/// top-level `Namespace` `DeclFact`, since a namespace's children live in
/// `NamespaceBody::children`, never flattened into this list) resolves both
/// ends to that one enclosing `DeclFact` and is dropped by the same
/// `owner == target` rule that drops direct recursion — see
/// `decl.rs::record_occurrences`. That under-reports a real edge; it does
/// not fabricate one, so it errs the same direction every other gap in this
/// producer already does.
#[derive(Debug, Clone)]
pub struct OccurrenceFact {
    /// Index into `ModuleFacts::declarations` of the top-level declaration
    /// whose signature or body contains the reference site.
    pub owner: usize,
    /// Index into `ModuleFacts::declarations` of the top-level declaration
    /// the reference resolves to.
    pub target: usize,
    /// Byte span of the reference site (the identifier being used), absolute
    /// within the source file. `emit.rs` converts this to a `RelSpan`
    /// relative to the owner declaration's own span, matching every other
    /// span this producer emits.
    pub span_start: u32,
    pub span_end: u32,
    pub kind: OccurrenceKind,
}

// ── ModuleFacts ────────────────────────────────────────────────────────────────

/// All declarations extracted from one TypeScript module, fully owned.
///
/// Contains no arena references; survives the allocator drop.
#[derive(Debug)]
pub struct ModuleFacts {
    pub path: PathBuf,
    pub module_name: String,
    pub module_doc: Option<String>,
    pub declarations: Vec<DeclFact>,
    pub exports: ExportTable,
    pub imports: Vec<ImportFact>,
    /// Same-module reference edges between this module's own top-level
    /// declarations. See [`OccurrenceFact`] for the same-module and
    /// top-level-only restrictions — a cross-module call is a documented gap,
    /// not silently answered wrong.
    pub occurrences: Vec<OccurrenceFact>,
    /// Byte length of the source file (`source.len()` — `str::len()` is
    /// always a byte count in Rust, never a char or UTF-16 count). Used as
    /// the module entry's own span (`0..source_len`), a real, whole-file
    /// span rather than the previous degenerate `0..0`, so two files sharing
    /// a basename (e.g. rxjs's `internal/observable/combineLatest.d.ts` and
    /// `internal/operators/combineLatest.d.ts`, both named "combineLatest"
    /// by `specifier_to_module_name`, which keeps only the file's last path
    /// segment) collide on their base identity key but no longer collide on
    /// span too, so the `Span` disambiguation tier can actually tell them
    /// apart instead of falling through to order-dependent `Ordinal`.
    pub source_len: usize,
}
