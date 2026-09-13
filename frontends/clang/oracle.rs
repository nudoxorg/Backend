//! Owned oracle data extracted from libclang.
//!
//! All libclang objects (`Index`, `TranslationUnit`, cursors) are scoped to
//! the extraction block and dropped before anything in this module is returned.
//! Every value here is plain Rust — `String`, `Vec`, `Option` — with no arena
//! references.

use std::path::PathBuf;

// ── Stable id ─────────────────────────────────────────────────────────────────

/// A Clang Unified Symbol Resolution string.
///
/// Stable across TUs; unique per declaration; the right `Producer::Id`.
pub type Usr = String;

// ── Visibility ────────────────────────────────────────────────────────────────

/// Visibility and linkage of a declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleVisibility {
    /// A declaration available through the public interface.
    Public,
    /// A declaration visible to derived classes.
    Protected,
    /// A declaration visible only to its declaring class or scope.
    Private,
    /// `static` at file scope — C internal linkage.
    Internal,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A fully resolved C/C++ type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleType {
    /// The C or C++ `void` type.
    Void,
    /// The C or C++ boolean type.
    Bool,
    /// An integer with an ABI-known width.
    Integer {
        /// Whether the integer has a signed representation.
        signed: bool,
        /// The exact integer width when the target ABI defines it.
        bits: u16,
    },
    /// A floating-point type with an ABI-known width.
    Float {
        /// The exact floating-point width when the target ABI defines it.
        bits: u16,
    },
    /// Architecture-dependent width (e.g. `size_t` via platform headers, plain
    /// `long`/`unsigned long` on unknown-width platforms).
    IntegerArch {
        /// Whether the platform-sized integer is signed.
        signed: bool,
    },
    /// A platform-dependent floating-point width.
    FloatArch,
    /// `*const T` or `*volatile T` (not mutable from Rust perspective).
    ConstPointer(Box<OracleType>),
    /// `*T` or `*volatile T` (mutable pointer).
    MutPointer(Box<OracleType>),
    /// `T&` — lvalue reference.
    LValueRef {
        /// Whether the referenced value may be mutated through this reference.
        mutable: bool,
        /// Referenced type.
        ty: Box<OracleType>,
    },
    /// `T&&` — rvalue reference.
    RValueRef(Box<OracleType>),
    /// `T[N]`.
    Array {
        /// Element type.
        ty: Box<OracleType>,
        /// Number of elements in the complete array.
        len: usize,
    },
    /// `T[]` (incomplete array).
    Slice(Box<OracleType>),
    /// Function pointer `ret (*)(args…)`.
    FnPtr {
        /// Function result type.
        ret: Box<OracleType>,
        /// Function parameter types in declaration order.
        params: Vec<OracleType>,
    },
    /// Reference to a named type; `args` is non-empty for template instantiations.
    Named {
        /// Source spelling of the named type.
        name: String,
        /// The declaration's Unified Symbol Resolution id, when libclang
        /// could resolve one via `ty.get_declaration()`. `None` when the
        /// type kind has no backing declaration at all (a dependent type,
        /// some exotic sugar) — `lower::lower_named` treats that absence as
        /// meaningfully different from "resolved, but not ours or std's".
        ///
        /// This is what lets `lower_type` (`lower.rs`) tell a same-package
        /// reference, a standard-library reference, and a genuinely
        /// unresolved name apart, instead of collapsing all three into one
        /// `UnresolvedExternal` spelling.
        usr: Option<Usr>,
        /// Template arguments in declaration order.
        args: Vec<OracleType>,
    },
    /// A template type-parameter use (e.g. `T` in `template<typename T>`).
    TypeVar(String),
    /// Clang `auto` / dependent type — resolved as unknown.
    Inferred,
}

// ── Parameters ────────────────────────────────────────────────────────────────

/// A function parameter with an owned source coordinate.
#[derive(Debug, Clone)]
pub struct OracleParam {
    /// Source spelling of the parameter name.
    pub name: String,
    /// Resolved parameter type.
    pub ty: OracleType,
    /// Whether this parameter consumes a variadic argument.
    pub is_variadic: bool,
    /// File containing the parameter spelling.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the parameter.
    pub byte_offset: usize,
}

// ── Generic parameters ────────────────────────────────────────────────────────

/// A function template parameter.
#[derive(Debug, Clone)]
pub enum OracleGenericParam {
    /// `template <typename T>` or `template <class T>`.
    Type {
        /// Source spelling of the type parameter.
        name: String,
    },
    /// `template <int N>` or similar non-type parameter.
    Const {
        /// Source spelling of the constant parameter.
        name: String,
        /// Declared type spelling of the constant parameter.
        ty: String,
    },
    /// `template <template<…> class TT>`.
    Template {
        /// Source spelling of the template parameter.
        name: String,
    },
}

// ── Receiver ─────────────────────────────────────────────────────────────────

/// Receiver ownership and qualification for a member function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleReceiver {
    /// Constructor / static method.
    Static,
    /// Destructor / by-value consumer.
    Owned,
    /// `const` method.
    SharedRef,
    /// Non-const method.
    MutRef,
}

// ── Function-level modifiers ──────────────────────────────────────────────────

/// ABI or optimizer modifier attached to a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleFnMod {
    /// `constexpr` or `__attribute__((const))`.
    Const,
    /// `__attribute__((pure))`.
    Pure,
    /// `virtual` (not currently mapped to an IR modifier).
    Virtual,
    /// `[[noreturn]]`.
    NoReturn,
}

// ── Declared items ────────────────────────────────────────────────────────────

/// A single function / method / overload.
#[derive(Debug, Clone)]
pub struct OracleFunction {
    /// Clang USR identifying this function.
    pub usr: Usr,
    /// Source spelling of the function name.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// `None` for free functions.
    pub receiver: Option<OracleReceiver>,
    /// Template parameters declared by this function.
    pub generics: Vec<OracleGenericParam>,
    /// Parameters declared by this function.
    pub params: Vec<OracleParam>,
    /// Whether the function accepts a variadic argument list.
    pub variadic: bool,
    /// Resolved return type.
    pub ret: OracleType,
    /// ABI and optimizer modifiers attached to this function.
    pub modifiers: Vec<OracleFnMod>,
    /// ABI spelling when Clang exposes one.
    pub abi: Option<String>,
    /// Documentation attached to the function.
    pub documentation: String,
    /// Visibility and linkage of the function.
    pub visibility: OracleVisibility,
    /// USR of the containing namespace / record, or `None` for top-level.
    pub parent_usr: Option<Usr>,
}

/// A struct / class / C union (see IR gap note).
#[derive(Debug, Clone)]
pub struct OracleRecord {
    /// Clang USR identifying this record.
    pub usr: Usr,
    /// Source spelling of the record name.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Whether the declaration uses C++ class semantics.
    pub is_class: bool,
    /// `true` when this is a C `union` (IR gap: `RecordForm` has no `Union`).
    pub is_union: bool,
    /// Template parameters declared by this record.
    pub generics: Vec<OracleGenericParam>,
    /// Resolved direct base types.
    pub super_types: Vec<OracleType>,
    /// Fields declared by this record.
    pub fields: Vec<OracleField>,
    /// Documentation attached to the record.
    pub documentation: String,
    /// Visibility and linkage of the record.
    pub visibility: OracleVisibility,
    /// Clang USR of the containing declaration, when available.
    pub parent_usr: Option<Usr>,
}

/// A field / member variable inside a record.
#[derive(Debug, Clone)]
pub struct OracleField {
    /// Clang USR identifying this field.
    pub usr: Usr,
    /// Source spelling of the field name.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Resolved field type.
    pub ty: OracleType,
    /// Access level of the field.
    pub visibility: OracleVisibility,
    /// Whether the field is declared static.
    pub is_static: bool,
    /// Whether the field is mutable under source-language rules.
    pub is_mutable: bool,
    /// Documentation attached to the field.
    pub documentation: String,
    /// Clang USR of the containing record, when available.
    pub parent_usr: Option<Usr>,
}

/// A C/C++ enum.
#[derive(Debug, Clone)]
pub struct OracleEnum {
    /// Clang USR identifying this enum.
    pub usr: Usr,
    /// Source spelling of the enum name.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Whether the enum is scoped (`enum class` or `enum struct`).
    pub is_scoped: bool,
    /// Enumerators owned by this enum.
    pub variants: Vec<OracleVariant>,
    /// Documentation attached to the enum.
    pub documentation: String,
    /// Visibility of the enum.
    pub visibility: OracleVisibility,
    /// Clang USR of the containing declaration, when available.
    pub parent_usr: Option<Usr>,
}

/// One enumerator.
#[derive(Debug, Clone)]
pub struct OracleVariant {
    /// Clang USR identifying this enumerator.
    pub usr: Usr,
    /// Source spelling of the enumerator.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Explicit or evaluated integral value, when available.
    pub discr: Option<i64>,
    /// Documentation attached to the enumerator.
    pub documentation: String,
    /// Clang USR of the containing enum, when available.
    pub parent_usr: Option<Usr>,
}

/// A `typedef` or `using` alias.
#[derive(Debug, Clone)]
pub struct OracleAlias {
    /// Clang USR identifying this alias.
    pub usr: Usr,
    /// Source spelling of the alias.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Resolved target type.
    pub target: OracleType,
    /// Documentation attached to the alias.
    pub documentation: String,
    /// Access level of the alias.
    pub visibility: OracleVisibility,
    /// Clang USR of the containing declaration, when available.
    pub parent_usr: Option<Usr>,
}

/// A namespace or `extern "C"` linkage-spec (treated as a module).
#[derive(Debug, Clone)]
pub struct OracleNamespace {
    /// Clang USR identifying this namespace.
    pub usr: Usr,
    /// Source spelling of the namespace.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Documentation attached to the namespace.
    pub documentation: String,
    /// Visibility of declarations in the namespace.
    pub visibility: OracleVisibility,
    /// Clang USR of the containing declaration, when available.
    pub parent_usr: Option<Usr>,
}

/// A file-scope variable declaration.
#[derive(Debug, Clone)]
pub struct OracleVar {
    /// Clang USR identifying this variable.
    pub usr: Usr,
    /// Source spelling of the variable.
    pub name: String,
    /// File containing the declaration.
    pub source_file: PathBuf,
    /// UTF-8 source byte offset of the declaration.
    pub byte_offset: usize,
    /// Resolved variable type.
    pub ty: OracleType,
    /// Whether the variable is declared const.
    pub is_const: bool,
    /// Documentation attached to the variable.
    pub documentation: String,
    /// Linkage and access level of the variable.
    pub visibility: OracleVisibility,
    /// Clang USR of the containing declaration, when available.
    pub parent_usr: Option<Usr>,
}

/// One source occurrence resolved to a declaration USR.
#[derive(Debug, Clone)]
pub struct Reference {
    /// Clang USR of the declaration containing the occurrence.
    pub owner: Usr,
    /// Clang USR resolved at the occurrence.
    pub target: Usr,
    /// File containing the occurrence.
    pub source_file: PathBuf,
    /// First UTF-8 source byte of the occurrence.
    pub byte_start: usize,
    /// First byte after the occurrence.
    pub byte_end: usize,
}

/// One diagnostic retained from libclang's translation-unit admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleDiagnostic {
    /// Stable severity spelling from libclang.
    pub severity: String,
    /// Diagnostic message supplied by libclang.
    pub message: String,
    /// Source file selected by libclang's file location.
    pub source_file: PathBuf,
    /// First byte selected by libclang.
    pub byte_start: usize,
    /// First byte after the diagnostic range.
    pub byte_end: usize,
}

// ── Collected oracle output ───────────────────────────────────────────────────

/// The fully-owned result of the extraction pass.
///
/// All libclang objects have been dropped before this is returned.  The lists
/// are in declaration order; parent–child relationships are encoded as
/// `parent_usr: Option<Usr>` rather than nested trees.
#[derive(Debug, Default)]
pub struct ClangOracle {
    /// Namespaces and linkage scopes declared in the selected files.
    pub namespaces: Vec<OracleNamespace>,
    /// Records declared in the selected files.
    pub records: Vec<OracleRecord>,
    /// Fields declared in the selected records.
    pub fields: Vec<OracleField>,
    /// Free functions and member functions declared in the selected files.
    pub functions: Vec<OracleFunction>,
    /// Enumerations declared in the selected files.
    pub enums: Vec<OracleEnum>,
    /// Enumerators declared in the selected enums.
    pub variants: Vec<OracleVariant>,
    /// Type aliases declared in the selected files.
    pub aliases: Vec<OracleAlias>,
    /// File-scope variables declared in the selected files.
    pub vars: Vec<OracleVar>,
    /// Resolved source occurrences.
    pub references: Vec<Reference>,
    /// Diagnostics emitted while parsing the selected translation units.
    pub diagnostics: Vec<OracleDiagnostic>,
}
