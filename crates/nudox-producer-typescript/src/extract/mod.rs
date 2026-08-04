//! Pass-1 extraction: parse one module into owned [`ModuleFacts`].
//!
//! Each module gets its own `Allocator` scoped to the extraction block;
//! `ModuleFacts` contains no arena references. This is how the OXC lifetime
//! problem is solved: all data is owned before the arena is dropped.

use std::path::PathBuf;

pub mod decl;
pub mod jsdoc;
pub mod types;

// ── Error types ───────────────────────────────────────────────────────────────

/// A failure during per-module extraction.
#[derive(Debug)]
pub enum ExtractError {
    /// An OXC AST construct that has no IR representation.
    UnsupportedConstruct { symbol: String, description: String },
    /// A type expression could not be lowered.
    TypeLowering { context: String, detail: String },
    /// Parser panic.
    ParsePanic { path: PathBuf, detail: String },
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::UnsupportedConstruct { symbol, description } => {
                write!(f, "unsupported construct at `{symbol}`: {description}")
            }
            ExtractError::TypeLowering { context, detail } => {
                write!(f, "type lowering failed in `{context}`: {detail}")
            }
            ExtractError::ParsePanic { path, detail } => {
                write!(f, "parser panicked on `{}`: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for ExtractError {}

/// Package-level errors (entry discovery, graph build, I/O).
#[derive(Debug)]
pub enum PackageError {
    Io(std::io::Error),
    Serialization(serde_json::Error),
    Extract(ExtractError),
    EntryPointDiscoveryFailed { path: PathBuf },
    ParseFailed { path: PathBuf, detail: String },
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageError::Io(e) => write!(f, "I/O error: {e}"),
            PackageError::Serialization(e) => write!(f, "serialization error: {e}"),
            PackageError::Extract(e) => write!(f, "extraction error: {e}"),
            PackageError::EntryPointDiscoveryFailed { path } => {
                write!(f, "could not discover entry point under `{}`", path.display())
            }
            PackageError::ParseFailed { path, detail } => {
                write!(f, "parse failed for `{}`: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for PackageError {}

impl From<std::io::Error> for PackageError {
    fn from(e: std::io::Error) -> Self {
        PackageError::Io(e)
    }
}

impl From<serde_json::Error> for PackageError {
    fn from(e: serde_json::Error) -> Self {
        PackageError::Serialization(e)
    }
}

impl From<ExtractError> for PackageError {
    fn from(e: ExtractError) -> Self {
        PackageError::Extract(e)
    }
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
        DeprecationOwned { note: d.note, since: d.since }
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
}

/// A star re-export (`export * from "m"`).
#[derive(Debug, Clone)]
pub struct StarExport {
    pub module_request: String,
}

/// Export surface extracted from the module record.
#[derive(Debug, Clone, Default)]
pub struct ExportTable {
    pub exported_names: Vec<String>,
    pub indirect: Vec<IndirectExport>,
    pub star: Vec<StarExport>,
    pub default_local_name: Option<String>,
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
    Reexport { module_request: String, import_name: String },
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
}

#[derive(Debug)]
pub struct PropertyFact {
    pub name: String,
    pub ty: Option<TypeOwned>,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
}

#[derive(Debug)]
pub struct MethodFact {
    pub name: String,
    pub sig: FunctionBody,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
    /// True for overload signatures.
    pub is_overload: bool,
}

#[derive(Debug)]
pub struct MemberFact {
    pub name: String,
    pub kind: MemberKind,
    pub modifiers: MemberModifiers,
    pub doc: DocFacts,
    /// Decorators on the member (e.g. `@readonly`, `@Column()`).
    pub decorators: Vec<AttrTok>,
}

#[derive(Debug)]
pub enum MemberKind {
    Property { ty: Option<TypeOwned> },
    Method(Vec<FunctionBody>),
    Constructor(FunctionBody),
    /// TC39 `accessor x: T` — auto-generates a getter/setter pair.
    Accessor { ty: Option<TypeOwned> },
    /// `static { … }` initializer block — synthetic `__static[_N]` function.
    StaticBlock { name: String },
}

#[derive(Debug, Clone)]
pub struct ParamFact {
    pub name: String,
    pub ty: Option<TypeOwned>,
    pub is_optional: bool,
    pub is_rest: bool,
    pub is_readonly: bool,
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
    Apply { base: Box<TypeOwned>, args: Vec<TypeOwned> },
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
    NamedTupleElem { label: String, ty: Box<TypeOwned> },
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
}
