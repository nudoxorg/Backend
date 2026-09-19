//! Defines compact C and C++ semantic facts emitted by the direct libclang authority.
//! All locations are exact half-open byte spans into the caller's input source slice.
//! Recursive type structure is represented by fact and edge rows, never serialized strings.
//!
//! Authority lane geometry is 32,768 rows: declarations and references feed the trunk's
//! `MAX_EMISSION_FACTS` and `MAX_EMISSION_OCCURRENCES` lanes respectively. Types and type edges
//! feed the trunk fact/type-child lanes; diagnostics feed doc fragments; includes and overrides
//! feed the extension/occurrence lanes. A filled lane retains only its typed slots on the caller's
//! heap: bytes below include element storage but exclude the caller's slice descriptor.
//! The empty `ClangScratch` descriptor remains 112 bytes before and after scaling (seven
//! caller-provided mutable slice descriptors); scaled storage is not embedded in it.

use core::mem::size_of;

/// Shared trunk-class row geometry for every bounded Clang authority lane.
///
/// This matches the compiler driver's measured declaration ceiling: the
/// target corpus reaches 24,924 declarations in one translation unit
/// (`sqlite3.c`), and fmt's `gmock-gtest-all.cc` defers more than 16,384
/// parameter cursors through the declaration lane's deferred-parameter
/// stash while committing only 2,553 rows, so the former 16,384-row
/// authority boundary rejected valid source before the shared typed
/// admission lane could apply its own capacity contract.
pub const MAX_CLANG_FACTS: usize = 32_768;
/// Declaration rows feed trunk `MAX_EMISSION_FACTS`.
pub const MAX_CLANG_DECLARATIONS: usize = MAX_CLANG_FACTS;
/// Recursive type rows feed trunk type facts; one declaration may own a
/// nominal row plus nested pointer/element rows, so the lane keeps the
/// historical four-times ratio and preserves capacity for nested native
/// type graphs without truncation.
pub const MAX_CLANG_TYPES: usize = 4 * MAX_CLANG_FACTS;
/// Recursive type edges feed trunk type-child adjacency within
/// `MAX_EMISSION_FACTS`; edges scale with type rows, keeping the same ratio.
pub const MAX_CLANG_TYPE_EDGES: usize = 4 * MAX_CLANG_FACTS;
/// Reference rows feed trunk `MAX_EMISSION_OCCURRENCES`; call- and
/// member-rich sources emit several references per declaration, keeping the
/// historical four-times ratio.
pub const MAX_CLANG_REFERENCES: usize = 4 * MAX_CLANG_FACTS;
/// Diagnostic rows feed trunk `MAX_EMISSION_DOC_FRAGMENTS`.
pub const MAX_CLANG_DIAGNOSTICS: usize = MAX_CLANG_FACTS;
/// Include rows feed the trunk extension atom lane.
pub const MAX_CLANG_INCLUDES: usize = MAX_CLANG_FACTS;
/// Override rows feed trunk `MAX_EMISSION_OCCURRENCES`.
pub const MAX_CLANG_OVERRIDES: usize = MAX_CLANG_FACTS;

/// An exact half-open byte range into the caller-provided source slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    /// Inclusive first source byte.
    pub start: u32,
    /// Exclusive byte immediately after this range.
    pub end: u32,
}

/// The fixed compact width of one domain-separated native symbol identity.
pub const SYMBOL_IDENTITY_BYTES: usize = 16;

/// A stable native symbol identity derived from a libclang USR under a fixed domain tag.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SymbolIdentity {
    /// Domain-separated truncated BLAKE3 bytes of the exact native USR.
    pub bytes: [u8; SYMBOL_IDENTITY_BYTES],
}

impl SymbolIdentity {
    /// Derives one domain-separated compact identity from transient native authority bytes.
    pub(crate) fn from_native(domain: &[u8], native: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain);
        hasher.update(native);
        let digest = hasher.finalize();
        let mut bytes = [0; SYMBOL_IDENTITY_BYTES];
        bytes.copy_from_slice(&digest.as_bytes()[..SYMBOL_IDENTITY_BYTES]);
        Self { bytes }
    }
}

/// The typed ordinal of one declaration fact in a caller-provided slot array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationId {
    /// Zero-based declaration slot ordinal.
    pub raw: u32,
}

/// The typed ordinal of one recursive type fact in a caller-provided slot array.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeId {
    /// Zero-based type slot ordinal.
    pub raw: u32,
}

/// One semantic declaration form recognized directly by libclang.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    /// An unrecognized declaration retained without invented interpretation.
    Unknown,
    /// A namespace declaration.
    Namespace,
    /// A preprocessor macro definition.
    Macro,
    /// A record, struct, class, or union declaration.
    Record,
    /// An enum declaration.
    Enumeration,
    /// An enum member.
    Enumerator,
    /// A free function declaration or definition.
    Function,
    /// A C++ method declaration or definition.
    Method,
    /// A C++ constructor.
    Constructor,
    /// A C++ destructor.
    Destructor,
    /// A field declaration.
    Field,
    /// A variable declaration.
    Variable,
    /// A function parameter.
    Parameter,
    /// A C++ template type, non-type, or template parameter.
    TemplateParameter,
    /// A typedef or type alias.
    TypeAlias,
    /// A function, class, or partial-specialization template.
    Template,
}

/// How one declaration's storage is bound, projected from libclang's
/// closed storage-class query into the driver's closed storage lattice.
///
/// `ThreadLocal` is part of the closed lattice but unreachable through the
/// enabled `clang_3_6` symbol surface, which carries no TLS query; a TLS
/// variable therefore retains the storage class libclang reports for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageClass {
    /// No storage-class fact.
    None,
    /// Block-scope automatic storage.
    Auto,
    /// Static or internal-linkage storage.
    Static,
    /// External linkage.
    Extern,
    /// Register storage.
    Register,
    /// Thread-local storage.
    ThreadLocal,
}

/// The closed C integer conversion rank of one native integer scalar.
///
/// Rank is independent of measured width: on LP64 `long` and `long long` are
/// both 64 bits, and on LLP64 `int` and `long` are both 32 bits. A width cell
/// alone therefore cannot distinguish the declaration families, so the rank
/// travels with the fact exactly as libclang reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegerRank {
    /// `short` / `unsigned short`.
    Short,
    /// `int` / `unsigned int`.
    Int,
    /// `long` / `unsigned long`.
    Long,
    /// `long long` / `unsigned long long`.
    LongLong,
    /// `__int128` / `unsigned __int128`.
    Int128,
}

/// The closed scalar classification of one native builtin type fact.
///
/// The collector classifies directly from libclang's type kind, so a C `int`
/// never has to be recovered from source text downstream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinClass {
    /// The C `void` type.
    Void,
    /// The C `_Bool` type.
    Bool,
    /// Plain `char` with a signed representation reported by libclang.
    PlainCharSigned,
    /// Plain `char` with an unsigned representation reported by libclang.
    PlainCharUnsigned,
    /// Explicit `signed char`.
    SignedChar,
    /// Explicit `unsigned char`.
    UnsignedChar,
    /// `char16_t`, a UTF-16 code unit.
    Utf16CodeUnit,
    /// `char32_t`, a UTF-32 code unit.
    Utf32CodeUnit,
    /// `wchar_t` with signed representation verified by canonical native type.
    WideCharSigned,
    /// `wchar_t` with unsigned representation verified by canonical native type.
    WideCharUnsigned,
    /// `wchar_t` where the native surface supplies no signedness proof.
    WideCharSignednessUnavailable,
    /// A signed or unsigned integer scalar with its exact C conversion rank.
    Integer {
        /// The type excludes negative values.
        signed: bool,
        /// The exact C integer conversion rank, independent of measured width.
        rank: IntegerRank,
    },
    /// A floating-point scalar (`float`, `double`, `long double`).
    Float,
    /// Any other native builtin (`nullptr_t`, `_Complex`, vector extensions).
    Other,
}

/// Whether libclang identifies this declaration as a definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionState {
    /// A declaration without a definition in this translation unit.
    Declaration,
    /// A definition in this translation unit.
    Definition,
}

/// The closed C++ method virtuality reported by libclang.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MethodVirtuality {
    /// The method does not participate in virtual dispatch.
    NonVirtual,
    /// The method participates in virtual dispatch and is implementable.
    Virtual,
    /// The method participates in virtual dispatch and has no implementation.
    PureVirtual,
}

/// One C or C++ declaration with exact source and semantic identity facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclarationFact {
    /// Typed fact-row identity.
    pub id: DeclarationId,
    /// Directly classified native declaration form.
    pub kind: DeclarationKind,
    /// Native declaration-versus-definition fact.
    pub definition: DefinitionState,
    /// Direct C++ method virtuality; non-method declarations are non-virtual.
    pub virtuality: MethodVirtuality,
    /// Domain-separated libclang USR identity when libclang provides one.
    pub identity: Option<SymbolIdentity>,
    /// Exact declaration extent in the main source file.
    pub span: SourceSpan,
    /// Exact declared-name span in the main source file when libclang provides one.
    pub name: Option<SourceSpan>,
    /// Exact enclosing semantic-parent identity when libclang provides one.
    pub owner: Option<SymbolIdentity>,
    /// Exact raw-comment span in the main source file.
    pub documentation: Option<SourceSpan>,
    /// Directly classified native storage binding.
    pub storage: StorageClass,
    /// Root recursive type fact associated with this declaration.
    pub type_root: Option<TypeId>,
    /// Exact native integer type declared for an enumeration. Only enum
    /// declarations carry this fact; absent means the authority did not
    /// provide a valid underlying type.
    pub enum_underlying: Option<TypeId>,
}

/// One directed C++ override-authority relation reported by libclang.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverrideFact {
    /// The overriding declaration identity.
    pub source: SymbolIdentity,
    /// The declaration identity overridden by `source`.
    pub target: SymbolIdentity,
    /// Compilation-directory-relative identity of the file declaring
    /// `target`, when the native path lives under the compilation root.
    ///
    /// The relative tail is hashed here, not at cross-fragment resolution:
    /// owners mint declaration identities from `DeclarationKey`, while these
    /// stable references are keyed from the native USR. Bridging the two
    /// (resolving a `StableRef` against a loaded fragment's declarations) is
    /// future work and must not silently treat one identity as the other.
    pub target_file: Option<SymbolIdentity>,
}

/// A compact fact describing C/C++ type qualifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeQualifiers {
    /// The type is const-qualified.
    pub is_const: bool,
    /// The type is volatile-qualified.
    pub is_volatile: bool,
    /// The type is restrict-qualified.
    pub is_restrict: bool,
}

/// One direct recursive C or C++ type form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeKind {
    /// An invalid or unmodeled native type preserved as unknown.
    Unknown,
    /// A primitive scalar type.
    Builtin,
    /// A declaration-named record, enum, typedef, or template specialization.
    Named,
    /// A pointer type with one pointee edge.
    Pointer,
    /// An Objective-C block pointer. It has a signature/pointee edge but is
    /// structurally distinct from an ordinary C pointer.
    BlockPointer,
    /// A C++ member pointer with independent owning-class and member-type
    /// edges. It must not be flattened into an ordinary pointer because
    /// `Field Owner::*` has two source-semantic operands.
    MemberPointer,
    /// An lvalue reference type with one referent edge.
    LvalueReference,
    /// An rvalue reference type with one referent edge.
    RvalueReference,
    /// An array type with one element edge.
    Array,
    /// A function type with result and parameter edges.
    Function,
}

/// One role held by a recursive child relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeRelation {
    /// The pointee of a pointer type.
    Pointee,
    /// The owning class of a C++ member pointer.
    MemberOwner,
    /// The referent of a C++ reference type.
    Referent,
    /// The element type of an array.
    Element,
    /// The result type of a function.
    Result,
    /// A parameter type of a function.
    Parameter,
    /// A type argument of a template specialization.
    TemplateArgument,
}

/// One compact direct native type fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeFact {
    /// Typed recursive-type row identity.
    pub id: TypeId,
    /// Directly classified native type form.
    pub kind: TypeKind,
    /// Direct qualifier facts reported by libclang.
    pub qualifiers: TypeQualifiers,
    /// The named declaration identity when this type resolves to a declaration.
    pub declaration: Option<SymbolIdentity>,
    /// Exact native array cardinality when known and non-negative.
    pub array_len: Option<u64>,
    /// Closed scalar classification when this type is a native builtin, or
    /// when a named alias canonicalizes to one. The declaration identity still
    /// records the alias; this class is the measured underlying form.
    pub builtin: Option<BuiltinClass>,
    /// Exact bit size measured by libclang, or `None` when the type is
    /// incomplete, dependent, or otherwise unmeasured.
    pub size_bits: Option<u32>,
    /// Exact bit alignment measured by libclang under the same law.
    pub align_bits: Option<u32>,
    /// Native `clang_isFunctionTypeVariadic` fact. This is meaningful only
    /// on [`TypeKind::Function`] and is never inferred from source spelling.
    pub is_variadic: bool,
}

impl TypeFact {
    /// True when this type fact is the native `void` type.
    #[must_use]
    pub const fn is_void(&self) -> bool {
        matches!(
            (self.kind, self.builtin),
            (TypeKind::Builtin, Some(BuiltinClass::Void))
        )
    }
}

/// One directed row in the recursive type fact graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypeEdge {
    /// The parent recursive type fact.
    pub source: TypeId,
    /// The direct semantic relation to the child fact.
    pub relation: TypeRelation,
    /// The child recursive type fact.
    pub target: TypeId,
}

/// How a native expression or type cursor refers to its target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceKind {
    /// A value or declaration reference.
    Value,
    /// A type reference.
    Type,
    /// A template reference.
    Template,
    /// A member reference.
    Member,
    /// A call expression.
    Call,
    /// A macro invocation.
    MacroExpansion,
}

/// The local, foreign, or unresolved identity result of a native reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTarget {
    /// The target declaration belongs to this translation unit's main source file.
    Local(SymbolIdentity),
    /// The target declaration belongs to an included or otherwise external authority.
    Foreign {
        /// Domain-separated USR identity of the target declaration.
        identity: SymbolIdentity,
        /// Compilation-directory-relative identity of the file declaring the
        /// target, when the native path lives under the compilation root. An
        /// absolute system or store path never enters this cell; those targets
        /// are keyed opaquely on the USR against a fixed system fragment.
        ///
        /// The USR→declaration-identity bridge is an explicit follow-up: owners
        /// mint from `DeclarationKey` while stable references carry the USR, so
        /// cross-fragment resolution must validate the two rather than assume
        /// they agree.
        file: Option<SymbolIdentity>,
    },
    /// libclang did not resolve the target cursor to a USR identity.
    Unresolved,
}

/// One direct native reference with an exact use span.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceFact {
    /// Directly classified native reference form.
    pub kind: ReferenceKind,
    /// Exact use extent in the main source file.
    pub span: SourceSpan,
    /// Enclosing semantic declaration identity when libclang provides one.
    pub owner: Option<SymbolIdentity>,
    /// Direct target-resolution result.
    pub target: ReferenceTarget,
}

/// A structured native diagnostic severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    /// An ignored native diagnostic.
    Ignored,
    /// A note attached to another diagnostic.
    Note,
    /// A warning diagnostic.
    Warning,
    /// An error diagnostic.
    Error,
    /// A fatal native diagnostic.
    Fatal,
    /// A newer or unknown native severity.
    Unknown,
}

/// One diagnostic fact whose text is represented by a stable native-message identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticFact {
    /// Direct native severity.
    pub severity: DiagnosticSeverity,
    /// Exact diagnostic location when it belongs to the main source file.
    pub location: Option<SourceSpan>,
    /// Native diagnostic category number.
    pub category: u32,
    /// Domain-separated identity of the original native diagnostic spelling when nonempty.
    pub message: Option<SymbolIdentity>,
}

/// The direct source-dependency form observed by libclang.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceDependencyKind {
    /// A preprocessor include directive.
    Include,
    /// A C++ module import declaration.
    ModuleImport,
}

/// One include-directive fact whose resolved file is retained as an opaque authority identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncludeFact {
    /// Direct source dependency form.
    pub kind: SourceDependencyKind,
    /// Exact include directive extent in the main source file.
    pub span: SourceSpan,
    /// Domain-separated identity of the resolved include file or imported module when available.
    pub resolved: Option<SymbolIdentity>,
}

/// Bounded fact views filled by one successful direct libclang collection.
#[derive(Debug)]
pub struct ClangFacts<'scratch> {
    /// Prefix of caller declaration slots written by libclang.
    pub declarations: &'scratch [DeclarationFact],
    /// Prefix of caller type slots written by libclang.
    pub types: &'scratch [TypeFact],
    /// Prefix of caller type-edge slots written by libclang.
    pub type_edges: &'scratch [TypeEdge],
    /// Prefix of caller reference slots written by libclang.
    pub references: &'scratch [ReferenceFact],
    /// Prefix of caller diagnostic slots written by libclang.
    pub diagnostics: &'scratch [DiagnosticFact],
    /// Prefix of caller include slots written by libclang.
    pub includes: &'scratch [IncludeFact],
    /// Prefix of caller override-authority slots.
    pub overrides: &'scratch [OverrideFact],
}

/// Retained bytes for one filled declaration lane, excluding its slice descriptor.
pub const CLANG_DECLARATION_LANE_BYTES: usize =
    size_of::<DeclarationFact>() * MAX_CLANG_DECLARATIONS;
/// Retained bytes for one filled type lane, excluding its slice descriptor.
pub const CLANG_TYPE_LANE_BYTES: usize = size_of::<TypeFact>() * MAX_CLANG_TYPES;
/// Retained bytes for one filled type-edge lane, excluding its slice descriptor.
pub const CLANG_TYPE_EDGE_LANE_BYTES: usize = size_of::<TypeEdge>() * MAX_CLANG_TYPE_EDGES;
/// Retained bytes for one filled reference lane, excluding its slice descriptor.
pub const CLANG_REFERENCE_LANE_BYTES: usize = size_of::<ReferenceFact>() * MAX_CLANG_REFERENCES;
/// Retained bytes for one filled diagnostic lane, excluding its slice descriptor.
pub const CLANG_DIAGNOSTIC_LANE_BYTES: usize = size_of::<DiagnosticFact>() * MAX_CLANG_DIAGNOSTICS;
/// Retained bytes for one filled include lane, excluding its slice descriptor.
pub const CLANG_INCLUDE_LANE_BYTES: usize = size_of::<IncludeFact>() * MAX_CLANG_INCLUDES;
/// Retained bytes for one filled override lane, excluding its slice descriptor.
pub const CLANG_OVERRIDE_LANE_BYTES: usize = size_of::<OverrideFact>() * MAX_CLANG_OVERRIDES;

const _: [(); CLANG_DECLARATION_LANE_BYTES] = [(); size_of::<DeclarationFact>() * MAX_CLANG_FACTS];
const _: [(); CLANG_TYPE_LANE_BYTES] = [(); size_of::<TypeFact>() * MAX_CLANG_TYPES];
const _: [(); CLANG_TYPE_EDGE_LANE_BYTES] = [(); size_of::<TypeEdge>() * MAX_CLANG_TYPE_EDGES];
const _: [(); CLANG_REFERENCE_LANE_BYTES] = [(); size_of::<ReferenceFact>() * MAX_CLANG_REFERENCES];
const _: [(); CLANG_DIAGNOSTIC_LANE_BYTES] = [(); size_of::<DiagnosticFact>() * MAX_CLANG_FACTS];
const _: [(); CLANG_INCLUDE_LANE_BYTES] = [(); size_of::<IncludeFact>() * MAX_CLANG_FACTS];
const _: [(); CLANG_OVERRIDE_LANE_BYTES] = [(); size_of::<OverrideFact>() * MAX_CLANG_FACTS];
