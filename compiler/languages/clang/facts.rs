//! Defines compact C and C++ semantic facts emitted by the direct libclang authority.
//! All locations are exact half-open byte spans into the caller's input source slice.
//! Recursive type structure is represented by fact and edge rows, never serialized strings.

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
    /// A function or template parameter.
    Parameter,
    /// A typedef or type alias.
    TypeAlias,
    /// A function, class, or partial-specialization template.
    Template,
}

/// Whether libclang identifies this declaration as a definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionState {
    /// A declaration without a definition in this translation unit.
    Declaration,
    /// A definition in this translation unit.
    Definition,
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
    /// Root recursive type fact associated with this declaration.
    pub type_root: Option<TypeId>,
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
}

/// The local, foreign, or unresolved identity result of a native reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTarget {
    /// The target declaration belongs to this translation unit's main source file.
    Local(SymbolIdentity),
    /// The target declaration belongs to an included or otherwise external authority.
    Foreign(SymbolIdentity),
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
}
