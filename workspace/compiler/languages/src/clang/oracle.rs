//! Owned oracle data extracted from libclang.
//!
//! All libclang objects (`Index`, `TranslationUnit`, cursors) are scoped to
//! the extraction block and dropped before anything in this module is returned.
//! Every value here is plain Rust — `String`, `Vec`, `Option` — with no arena
//! references.

use std::path::PathBuf;

// ── Stable id
// ─────────────────────────────────────────────────────────────────

/// A Clang Unified Symbol Resolution string.
///
/// Stable across TUs; unique per declaration; the right `Producer::Id`.
pub type Usr = String;

// ── Visibility
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleVisibility {
    Public,
    Protected,
    Private,
    /// `static` at file scope — C internal linkage.
    Internal,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A fully resolved C/C++ type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleType {
    Void,
    Bool,
    Integer {
        signed: bool,
        bits: u16,
    },
    Float {
        bits: u16,
    },
    /// Architecture-dependent width (e.g. `size_t` via platform headers, plain
    /// `long`/`unsigned long` on unknown-width platforms).
    IntegerArch {
        signed: bool,
    },
    FloatArch,
    /// `*const T` or `*volatile T` (not mutable from Rust perspective).
    ConstPointer(Box<OracleType>),
    /// `*T` or `*volatile T` (mutable pointer).
    MutPointer(Box<OracleType>),
    /// `T&` — lvalue reference.
    LValueRef {
        mutable: bool,
        ty: Box<OracleType>,
    },
    /// `T&&` — rvalue reference.
    RValueRef(Box<OracleType>),
    /// `T[N]`.
    Array {
        ty: Box<OracleType>,
        len: usize,
    },
    /// `T[]` (incomplete array).
    Slice(Box<OracleType>),
    /// Function pointer `ret (*)(args…)`.
    FnPtr {
        ret: Box<OracleType>,
        params: Vec<OracleType>,
    },
    /// Reference to a named type; `args` is non-empty for template
    /// instantiations.
    ///
    /// `decl_usr` is the declaration libclang resolved, when it has one.
    /// Lowering uses it to emit a same-package
    /// [`nudox_ir::kinds::Type::Nominal`] only for a USR this oracle will
    /// actually declare. A name with no declaration USR stays a spelling.
    Named {
        name: String,
        args: Vec<OracleType>,
        decl_usr: Option<Usr>,
    },
    /// A template type-parameter use (e.g. `T` in `template<typename T>`).
    TypeVar(String),
    /// Clang `auto` / dependent type — resolved as unknown.
    Inferred,
}

// ── Parameters
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OracleParam {
    pub name: String,
    pub ty: OracleType,
    pub is_variadic: bool,
    pub source_file: PathBuf,
    pub byte_offset: usize,
}

// ── Generic parameters
// ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum OracleGenericParam {
    /// `template <typename T>` or `template <class T>`.
    Type { name: String },
    /// `template <int N>` or similar non-type parameter.
    Const { name: String, ty: String },
    /// `template <template<…> class TT>`.
    Template { name: String },
}

// ── Receiver ─────────────────────────────────────────────────────────────────

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

// ── Function-level modifiers
// ──────────────────────────────────────────────────

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

// ── Declared items
// ────────────────────────────────────────────────────────────

/// A single function / method / overload.
#[derive(Debug, Clone)]
pub struct OracleFunction {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    /// `None` for free functions.
    pub receiver: Option<OracleReceiver>,
    pub generics: Vec<OracleGenericParam>,
    pub params: Vec<OracleParam>,
    pub variadic: bool,
    pub ret: OracleType,
    pub modifiers: Vec<OracleFnMod>,
    pub abi: Option<String>,
    pub documentation: String,
    pub visibility: OracleVisibility,
    /// USR of the containing namespace / record, or `None` for top-level.
    pub parent_usr: Option<Usr>,
}

/// A struct / class / C union (see IR gap note).
#[derive(Debug, Clone)]
pub struct OracleRecord {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub is_class: bool,
    /// `true` when this is a C `union` (IR gap: `RecordForm` has no `Union`).
    pub is_union: bool,
    pub generics: Vec<OracleGenericParam>,
    pub super_types: Vec<OracleType>,
    pub fields: Vec<OracleField>,
    pub documentation: String,
    pub visibility: OracleVisibility,
    pub parent_usr: Option<Usr>,
}

/// A field / member variable inside a record.
#[derive(Debug, Clone)]
pub struct OracleField {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub ty: OracleType,
    pub visibility: OracleVisibility,
    pub is_static: bool,
    pub is_mutable: bool,
    pub documentation: String,
    pub parent_usr: Option<Usr>,
}

/// A C/C++ enum.
#[derive(Debug, Clone)]
pub struct OracleEnum {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub is_scoped: bool,
    pub variants: Vec<OracleVariant>,
    pub documentation: String,
    pub visibility: OracleVisibility,
    pub parent_usr: Option<Usr>,
}

/// One enumerator.
#[derive(Debug, Clone)]
pub struct OracleVariant {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub discr: Option<i64>,
    pub documentation: String,
    pub parent_usr: Option<Usr>,
}

/// A `typedef` or `using` alias.
#[derive(Debug, Clone)]
pub struct OracleAlias {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub target: OracleType,
    pub documentation: String,
    pub visibility: OracleVisibility,
    pub parent_usr: Option<Usr>,
}

/// A namespace or `extern "C"` linkage-spec (treated as a module).
#[derive(Debug, Clone)]
pub struct OracleNamespace {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub documentation: String,
    pub visibility: OracleVisibility,
    pub parent_usr: Option<Usr>,
}

/// A file-scope variable declaration.
#[derive(Debug, Clone)]
pub struct OracleVar {
    pub usr: Usr,
    pub name: String,
    pub source_file: PathBuf,
    pub byte_offset: usize,
    pub ty: OracleType,
    pub is_const: bool,
    pub documentation: String,
    pub visibility: OracleVisibility,
    pub parent_usr: Option<Usr>,
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub owner: Usr,
    pub target: Usr,
    pub source_file: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
}

// ── Collected oracle output
// ───────────────────────────────────────────────────

/// The fully-owned result of the extraction pass.
///
/// All libclang objects have been dropped before this is returned.  The lists
/// are in declaration order; parent–child relationships are encoded as
/// `parent_usr: Option<Usr>` rather than nested trees.
#[derive(Debug, Default)]
pub struct ClangOracle {
    pub namespaces: Vec<OracleNamespace>,
    pub records: Vec<OracleRecord>,
    pub fields: Vec<OracleField>,
    pub functions: Vec<OracleFunction>,
    pub enums: Vec<OracleEnum>,
    pub variants: Vec<OracleVariant>,
    pub aliases: Vec<OracleAlias>,
    pub vars: Vec<OracleVar>,
    pub references: Vec<Reference>,
    /// Main files of the parses merged into this oracle, one path per parse.
    ///
    /// This is the list of translation units the producer opened, not the set
    /// of files reachable by `#include`. A package header that was opened once
    /// appears once; a header re-parsed for every includer would appear once
    /// per includer. Paths outside the package (system headers) are never
    /// pushed — those files are not chosen as translation units.
    pub main_files: Vec<PathBuf>,
}
