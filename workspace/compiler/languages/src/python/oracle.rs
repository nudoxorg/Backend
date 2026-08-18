//! Owned data structures produced by the Python oracle.
//!
//! These types hold the fully-extracted, owned view of a Python package that
//! `invoke()` produces and `lower()` consumes. No pyrefly handles, arenas, or
//! borrows survive into this layer — everything is owned plain data.
//!
//! # Lifetime strategy
//!
//! pyrefly's analysis handles (`Handle`, `Transaction`, `State`) are NOT
//! `'static` and carry internal references into the `State` arena. Rather than
//! fighting the borrow checker with self-referential Oracle types, we follow
//! the TypeScript producer's "Option C — extract to owned structures in
//! invoke()": run the oracle, walk its output, extract into owned `ModuleData`
//! / `ItemData` / etc., and drop all pyrefly handles before returning. By the
//! time `invoke()` returns, no pyrefly arena is live.
//!
//! # Id scheme
//!
//! `PythonId` is a fully-qualified dotted path string:
//!
//!   - module:     `"my_pkg.sub_module"`
//!   - class:      `"my_pkg.sub_module.MyClass"`
//!   - method:     `"my_pkg.sub_module.MyClass.my_method"`
//!   - overload:   `"my_pkg.sub_module.fn_name#0"`, `"…#1"`, …
//!   - param:      `"my_pkg.sub_module.MyClass.method.param_name"`
//!
//! The `#N` suffix for overloads ensures distinct `PythonId`s for each
//! `@overload`-decorated signature, which the IR requires (each overload is
//! its own declaration, never folded).

/// The stable producer-side identifier for every Python symbol.
///
/// A fully-qualified dotted path, with a `#N` suffix for overload branches.
/// Guaranteed to:
///   - be unique within the package (overloads distinguished by `#N`)
///   - survive source-order changes (determined by name, not position)
///   - be valid as a `HashMap` key (owned `String`, `Eq + Hash`)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PythonId(pub String);

impl PythonId {
    /// Construct from a dotted-path string.
    pub fn new(path: impl Into<String>) -> Self {
        PythonId(path.into())
    }

    /// Overload variant: append `#N` to the base path.
    pub fn overload(base: impl Into<String>, index: usize) -> Self {
        PythonId(format!("{}#{index}", base.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PythonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Owned data types
// ---------------------------------------------------------------------------

/// A fully-owned, oracle-extracted view of one Python package.
///
/// The pyrefly feature gate controls whether this is populated by the live
/// oracle or constructed manually (e.g. in tests).
#[derive(Debug, Default)]
pub struct PythonOracle {
    pub modules: Vec<ModuleData>,
}

/// One module's worth of extracted symbols.
#[derive(Debug)]
pub struct ModuleData {
    /// Dotted module name, e.g. `"my_pkg.utils"`.
    pub name: String,
    /// Optional module-level docstring.
    pub documentation: Option<String>,
    /// Optional deprecation.
    pub deprecation: Option<DeprecationData>,
    /// All top-level items in declaration order.
    pub items: Vec<ItemData>,
    /// Byte offsets of the whole file, `0..source.len()`. A module has no
    /// narrower "declaration" in the source than the file itself. See
    /// `ItemData::span`.
    pub span: std::ops::Range<usize>,
    /// The source file containing this module and all declarations walked
    /// beneath it.
    pub source: std::path::PathBuf,
    /// Lexically resolved direct calls whose target is a declaration in this
    /// package. Unresolved/dynamic calls are intentionally omitted.
    pub references: Vec<ReferenceData>,
}

#[derive(Debug, Clone)]
pub struct ReferenceData {
    pub owner: PythonId,
    pub target: PythonId,
    pub span: std::ops::Range<usize>,
}

/// One top-level or nested item.
#[derive(Debug)]
pub struct ItemData {
    /// Fully-qualified id (used as `PythonId`).
    pub id: PythonId,
    /// Parent id, or `None` for module-level items.
    pub parent: Option<PythonId>,
    /// Short name (last segment of the dotted path).
    pub name: String,
    /// Whether the name begins with `_` (conventionally private).
    pub is_private: bool,
    /// Optional docstring, already parsed.
    pub documentation: Option<String>,
    /// Optional deprecation metadata.
    pub deprecation: Option<DeprecationData>,
    /// Decorator names collected from the source.
    pub decorators: Vec<String>,
    /// Byte offsets of this declaration's own header (the `def`/`class`
    /// keyword through the statement's own range — never the body), in
    /// `Symbol::source`'s UTF-8 bytes. This is what feeds
    /// `nudox_ir::intro::Disambiguator::Span` when a structural skeleton
    /// collides; see `syntax.rs`'s extraction sites for where each variant
    /// gets it from ruff's `TextRange` (already UTF-8-byte, not char,
    /// offsets — `ruff_text_size::TextSize`'s own doc comment says so).
    pub span: std::ops::Range<usize>,
    /// The item body.
    pub body: ItemBody,
}

/// The kind-specific payload for an [`ItemData`].
#[derive(Debug)]
pub enum ItemBody {
    /// A module or package.
    Module,
    /// A class (record, enum, protocol, dataclass, TypedDict …).
    Class(ClassData),
    /// A plain function or method.
    Function(FunctionData),
    /// A `@overload`-decorated group: one `FunctionData` per overload branch,
    /// in declaration order. Each branch has its own `PythonId` with a `#N`
    /// suffix; the containing `ItemData.id` is the base name.
    Overloaded(Vec<FunctionData>),
    /// A module-level constant / annotated binding.
    Const(ConstData),
    /// A type alias (`X: TypeAlias = …` or PEP 695 `type X = …`).
    Alias(AliasData),
}

#[derive(Debug)]
pub struct ClassData {
    /// Base classes as type strings (dotted names).
    pub super_types: Vec<TypeData>,
    /// Generic parameters declared on this class.
    pub generics: Vec<GenericParamData>,
    /// Kind flag.
    pub form: ClassForm,
    /// Fields (data attributes).
    pub fields: Vec<FieldData>,
    /// Methods. Each may itself be overloaded.
    pub methods: Vec<ItemData>,
    /// Nested classes.
    pub nested: Vec<ItemData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassForm {
    /// Plain class (struct-like).
    Plain,
    /// `enum.Enum` subclass.
    Enum,
    /// `typing.Protocol`.
    Protocol,
    /// `@dataclass`.
    Dataclass,
    /// `TypedDict`.
    TypedDict,
    /// `typing.NamedTuple`.
    NamedTuple,
}

#[derive(Debug)]
pub struct FieldData {
    pub name: String,
    pub ty: Option<TypeData>,
    pub is_class_var: bool,
    pub is_final: bool,
    pub is_property: bool,
    pub has_default: bool,
    pub documentation: Option<String>,
    /// Byte offsets of the assignment/annotation statement that declared this
    /// field, in `Symbol::source`'s UTF-8 bytes. See `ItemData::span`.
    pub span: std::ops::Range<usize>,
}

#[derive(Debug)]
pub struct FunctionData {
    /// Overload index (0 for the first / only branch, N for the Nth overload).
    pub overload_index: usize,
    /// Byte offsets of this specific branch's own `def` statement, in
    /// `Symbol::source`'s UTF-8 bytes. See `ItemData::span`.
    ///
    /// Carried per-branch (not just on the containing `ItemData`) because an
    /// `@overload` group emits one declaration per branch
    /// (`oracle.rs`'s `PythonId` scheme, `base#0`/`base#1`/…) and each needs
    /// its own span, not the first branch's borrowed for all of them.
    pub span: std::ops::Range<usize>,
    pub receiver: ReceiverKind,
    pub params: Vec<ParamData>,
    pub return_ty: Option<TypeData>,
    /// Byte offsets of the `-> ReturnType` expression, when `return_ty` is
    /// `Some`. `emit/mod.rs` gives the synthesized return-type child entry
    /// this span (falling back to the function's own span in the one case
    /// where `return_ty` is filled in by the pyrefly tier — which infers a
    /// type but does not invent source text to point at).
    pub return_span: Option<std::ops::Range<usize>>,
    pub generics: Vec<GenericParamData>,
    pub is_async: bool,
    pub is_abstract: bool,
    pub is_stub: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverKind {
    /// `self` parameter — instance method.
    SharedRef,
    /// `@classmethod` with `cls` parameter.
    ClassMethod,
    /// `@staticmethod` — no receiver.
    Static,
    /// Free function (not a method).
    None,
}

#[derive(Debug)]
pub struct ParamData {
    pub name: String,
    pub ty: Option<TypeData>,
    pub kind: ParamKind,
    pub has_default: bool,
    pub doc_description: Option<String>,
    /// Byte offsets of the parameter itself (name plus annotation and
    /// default, per ruff's `Parameter`/`ParameterWithDefault` range), in
    /// `Symbol::source`'s UTF-8 bytes. See `ItemData::span`.
    pub span: std::ops::Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Regular positional-or-keyword.
    Normal,
    /// Positional-only (before `/`).
    PositionalOnly,
    /// Keyword-only (after `*`).
    KeywordOnly,
    /// `*args`.
    Varargs,
    /// `**kwargs`.
    Kwargs,
}

#[derive(Debug)]
pub struct ConstData {
    pub ty: Option<TypeData>,
    pub value: Option<String>,
}

#[derive(Debug)]
pub struct AliasData {
    pub target: Option<TypeData>,
    pub generics: Vec<GenericParamData>,
}

// ---------------------------------------------------------------------------
// Type representation
// ---------------------------------------------------------------------------

/// A fully-owned type expression.
///
/// This mirrors the shape of `nudox_ir::kinds::ty::Type` but lives in owned
/// string data, with no reference into a pyrefly arena. The `lower` phase
/// converts these into `nudox_ir::kinds::Type`.
#[derive(Debug, Clone)]
pub enum TypeData {
    /// `typing.Any` / unannotated.
    Any,
    /// `NoReturn` / `Never`.
    Never,
    /// A nominal type reference by dotted name.
    Nominal(String),
    /// A generic application: `base<args>`.
    Apply {
        base: Box<TypeData>,
        args: Vec<TypeData>,
    },
    /// `Union[A, B, …]` / `A | B`.
    Union(Vec<TypeData>),
    /// `Optional[T]` — stored as `Union([T, None])` after normalization.
    /// The lowering layer turns any two-member union containing `None` into
    /// `Type::Union([lower(T), Type::Nominal("builtins.NoneType")])`.
    Intersection(Vec<TypeData>),
    /// A reference to a type-parameter name (`T`, `K`, …).
    TypeVar(String),
    /// `tuple[A, B, …]`.
    Tuple(Vec<TypeData>),
    /// Unbounded slice `list[T]`.
    Slice(Box<TypeData>),
    /// `self` / `Self`.
    SelfType,
    /// Python `None` → lowered to `Nominal("builtins.NoneType")`.
    NoneType,
    /// PEP 593 `Annotated[T, meta, …]`.
    ///
    /// The first element is the underlying type; the remaining elements are
    /// metadata annotations (validator instances, doc strings, etc.).
    /// We record the underlying type as `inner` and each metadata item's
    /// string representation (from the oracle) as an `AttrTok`.
    ///
    /// Because metadata is arbitrary Python objects, the oracle surfaces them
    /// as unstructured strings. We store only the first annotation token (the
    /// most common case is a single metadata item). If a future oracle version
    /// surfaces typed metadata we can extend the payload without a schema break.
    Annotated {
        /// The annotated type (first arg to `Annotated[…]`).
        inner: Box<TypeData>,
        /// Metadata annotations (second-and-beyond args), as oracle strings.
        metadata: Vec<String>,
    },
    /// **A type-position construct this syntactic front end saw but has no
    /// case for**, carrying the source text verbatim.
    ///
    /// Two distinct situations collapse here, both honestly "we understood
    /// nothing about this position, not that the author wrote `Any`":
    ///
    /// - An `Expr` shape `expr_to_type` has no arm for (a lambda, a call, a
    ///   comparison — anything that is not a name, attribute, subscript,
    ///   literal, tuple, list, or `|`-union in type position).
    /// - A `Literal[...]` subscript. Its members are *values*, not types, and
    ///   the IR's type algebra has no slot for a value-set type — this is not
    ///   the gradual-typing escape hatch, it is a real PEP 586 construct we
    ///   cannot yet spell.
    ///
    /// `types::lower_type` maps this to
    /// `Type::Unknown(UnknownType::NoIrRepresentation { construct })`, never
    /// to `Type::DYNAMIC` — conflating "we didn't understand this" with "the
    /// source asked for dynamic typing" is exactly what CC-2 exists to undo.
    /// See that lowering for why `Any`/`DynamicallyTyped` is reserved for an
    /// *explicit* `typing.Any`.
    Unsupported(String),
}

// ---------------------------------------------------------------------------
// Generic parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GenericParamData {
    pub name: String,
    pub bound: Option<TypeData>,
    pub constraints: Vec<TypeData>,
    pub default: Option<TypeData>,
}

// ---------------------------------------------------------------------------
// Deprecation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DeprecationData {
    pub since: Option<String>,
    pub note: Option<String>,
}
