use super::ids::{AtomListId, EntityListId, FreePredicateListId, TypeListId, TypeParameterListId};
use super::relations::{Confidence, SourceSpan};
use crate::ir::TypeId;
use crate::vocabulary::{Language, LanguageProfile};

/// TypeScript declaration-specific facts kept out of generic entity fields.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeScriptFacts {
    /// Generic parameters declared by this symbol, in source order.
    pub type_parameters: TypeParameterListId,
    /// Optional declared type annotation in the shared type arena.
    pub declared: Option<TypeId>,
    /// Exact checker-observed type.  This is deliberately a general
    /// [`TypeId`]: an observation may be a concrete, computed, or unknown
    /// type.  Narrowing it to `ComputedTypeId` used to force lowerers to mint
    /// a false `typeof owner` node merely to satisfy the static marker.
    pub observed: Option<TypeId>,
}

/// Marker for TypeScript-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeScriptExtension {}
/// Marker for C#-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CSharpExtension {}
/// Marker for Go-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GoExtension {}
/// Marker for Rust-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RustExtension {}
/// Marker for Python-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PythonExtension {}
/// Marker for Java-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JavaExtension {}
/// Marker for Clang-only extension facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ClangExtension {}

/// C# nullable-reference interpretation retained independently of type spelling.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpNullability {
    /// Nullable-reference context was disabled or unavailable.
    Oblivious,
    /// Null is excluded by the declared reference annotation.
    NonNullable,
    /// Null is admitted by the declared reference annotation.
    Nullable,
}

/// C# parameter and return reference convention.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpReferenceKind {
    /// Value is passed or returned without a reference modifier.
    Value,
    /// Read-only reference parameter (`in`).
    In,
    /// Mutable reference parameter or return (`ref`).
    Ref,
    /// Write-only output parameter (`out`).
    Out,
}

/// Independent C# member effects; the three flags may occur together.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpMemberEffects {
    /// Member executes asynchronously and exposes an awaitable result.
    pub is_async: bool,
    /// Member produces a sequence through iterator semantics.
    pub is_iterator: bool,
    /// Member is an extension method declared outside its receiver type.
    pub is_extension: bool,
}

/// Whether a C# declaration is one half of a partial declaration.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpPartialRole {
    /// Declaration is not part of a partial pair.
    None,
    /// This declaration owns the admitted facts for its partial declaration pair.
    Definition,
    /// This declaration is the implementation part paired with a partial definition.
    Implementation,
}

/// C# facts not shared by the language-neutral declaration row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpFacts {
    /// Nullable-reference interpretation reported for this declaration.
    pub nullability: CSharpNullability,
    /// By-value or by-reference passing convention.
    pub reference_kind: CSharpReferenceKind,
    /// Generic constraints in declaration order.
    pub constraints: TypeParameterListId,
    /// Async, iterator, and extension-method flags.
    pub effects: CSharpMemberEffects,
    /// Source attributes retained as interned atom coordinates.
    pub attributes: AtomListId,
    /// This declaration's role in a partial declaration pair, if any.
    pub partial: CSharpPartialRole,
    /// Optional XML documentation source range.
    pub xml_provenance: Option<SourceSpan>,
}

/// Signature lists carried by a Go declaration. Both lists are shared type arenas.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoSignature {
    /// Ordered parameter types in the signature.
    pub parameters: TypeListId,
    /// Ordered result types in the signature.
    pub results: TypeListId,
    /// Whether the final parameter is variadic.
    pub variadic: bool,
}

/// Go facts that cannot be inferred from generic declarations and links.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoFacts {
    /// Function signature with parameter and result lanes.
    pub signature: GoSignature,
    /// Type parameters declared by this Go symbol.
    pub type_parameters: TypeParameterListId,
    /// Struct or interface fields in source order.
    pub fields: EntityListId,
    /// Methods admitted into the declaration's method set.
    pub method_set: EntityListId,
    /// Build-constraint expressions retained as interned atoms.
    pub build_constraints: AtomListId,
    /// Ordered spelling atoms for the constant value, when supplied.
    pub constant_value: AtomListId,
    /// Source-order `iota` group ordinal, or a negative sentinel outside a group.
    pub constant_group: i64,
    /// Go constant-spec flags retained by the frontend.
    pub constant_flags: u32,
}

/// Rust ownership fact attached to one declaration or parameter.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RustOwnership {
    /// Value is owned by the declaration or parameter.
    Value,
    /// Value is borrowed immutably.
    SharedBorrow,
    /// Value is borrowed mutably.
    MutableBorrow,
    /// Value is transferred to the callee or destination.
    Moved,
}

/// Rust-only declaration facts; text is interned in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RustFacts {
    /// Ownership or borrow behavior recorded for this declaration or parameter.
    pub ownership: RustOwnership,
    /// Lifetime names preserved as interned atoms in source order.
    pub lifetimes: AtomListId,
    /// `where`-clause constraints retained as type-parameter facts.
    pub where_clauses: TypeParameterListId,
    /// Macro names or invocations associated with this declaration.
    pub macros: AtomListId,
    /// const-generic value-default expression spellings, in written order.
    /// Rust requires every generic default to be trailing (E0128), so this
    /// suffix run pairs with the trailing const parameters; a const parameter
    /// without a default contributes no atom and no empty atom is ever used.
    pub const_defaults: AtomListId,
    /// Free generic predicates whose subject is not a declared parameter.
    pub free_predicates: FreePredicateListId,
}

/// Python parameter convention, retained instead of erasing it into an atom.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PythonParameterKind {
    /// Supplied only by position before the positional separator.
    PositionalOnly,
    /// Supplied by position or by its parameter name.
    PositionalOrKeyword,
    /// Collects extra positional arguments (`*args`).
    VariadicPositional,
    /// Supplied only by name after the keyword-only separator.
    KeywordOnly,
    /// Collects extra keyword arguments (`**kwargs`).
    VariadicKeyword,
}

/// Python-only source facts. Dynamic confidence is the existing quality lattice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PythonFacts {
    /// Decorator expressions in source order as interned atoms.
    pub decorators: AtomListId,
    /// Positional, keyword, or variadic binding convention.
    pub parameter_kind: PythonParameterKind,
    /// Confidence attached to the producer's dynamic-type observation.
    pub dynamic_confidence: Confidence,
}

/// Java-specific declaration facts for checked exceptions and record structure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JavaFacts {
    /// Checked exceptions declared by this method, in source order.
    pub throws: TypeListId,
    /// Annotation names retained as shared atom coordinates.
    pub annotations: AtomListId,
    /// Related overload declaration entities.
    pub overloads: EntityListId,
    /// Record components in declaration order.
    pub record_components: EntityListId,
}

/// Clang type qualifier bits kept separate from the generic type graph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangQualifiers {
    /// The declared type has the native `const` qualifier.
    pub is_const: bool,
    /// The declared type has the native `volatile` qualifier.
    pub is_volatile: bool,
    /// The declared type has the native `restrict` qualifier.
    pub is_restrict: bool,
}

/// Clang storage-class fact.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClangStorageClass {
    /// No explicit storage-class fact was reported.
    None,
    /// Automatic storage-class spelling (`auto` in C).
    Auto,
    /// Static storage class; linkage depends on declaration scope.
    Static,
    /// Externally linked declaration (`extern`).
    Extern,
    /// Register storage-class spelling (`register`).
    Register,
    /// Thread-local storage class.
    ThreadLocal,
}

/// Optional layout facts measured in bits; `None` means the frontend did not own a layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangLayout {
    /// Measured object or type size in bits, when layout is available.
    pub size_bits: Option<u32>,
    /// Measured alignment in bits, when layout is available.
    pub align_bits: Option<u32>,
}

/// Clang-only semantic facts. Include spelling is held in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangFacts {
    /// Native cv-qualifier bits on the declaration or type.
    pub qualifiers: ClangQualifiers,
    /// Storage-class fact emitted by Clang.
    pub storage: ClangStorageClass,
    /// Optional target-specific size and alignment, measured in bits.
    pub layout: ClangLayout,
    /// Template parameters in declaration order.
    pub templates: TypeParameterListId,
    /// Include spellings associated with the declaration.
    pub includes: AtomListId,
}

/// Authority bound into one semantic image before language facts are admitted.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum SemanticImageAuthority {
    /// A common-only image; language extensions are explicitly rejected.
    #[default]
    Shared,
    /// A closed canonical source profile, which proves its language family.
    Language(LanguageProfile),
}

impl SemanticImageAuthority {
    pub(in crate::ir::semantic) fn language(self) -> Option<Language> {
        match self {
            Self::Shared => None,
            Self::Language(profile) => Some(Language::from(profile)),
        }
    }
}

/// Exactly one language-specific extension submitted for one entity row.
///
/// This tagged input exists only during ingestion. The condensed IR routes it
/// into seven named sparse planes, so scans and reopened views never carry an
/// erased union or the union's maximum payload width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionInput<'facts> {
    /// TypeScript type-parameter, declared-type, and checker-observation facts.
    TypeScript(&'facts TypeScriptFacts),
    /// C# nullability, generic constraints, member effects, and attributes.
    CSharp(&'facts CSharpFacts),
    /// Go signature, method-set, build-constraint, and constant facts.
    Go(&'facts GoFacts),
    /// Rust ownership, lifetime, generic-predicate, and macro facts.
    Rust(&'facts RustFacts),
    /// Python decorator, parameter-binding, and dynamic-confidence facts.
    Python(&'facts PythonFacts),
    /// Java exception, annotation, overload, and record-component facts.
    Java(&'facts JavaFacts),
    /// Clang qualifier, storage-class, measured-layout, and include facts.
    Clang(&'facts ClangFacts),
}

impl LanguageExtensionInput<'_> {
    /// Closed source-language authority for this extension input.
    #[must_use]
    pub const fn language(self) -> Language {
        match self {
            Self::TypeScript(_) => Language::TypeScript,
            Self::CSharp(_) => Language::CSharp,
            Self::Go(_) => Language::Go,
            Self::Rust(_) => Language::Rust,
            Self::Python(_) => Language::Python,
            Self::Java(_) => Language::Java,
            Self::Clang(_) => Language::Clang,
        }
    }
}
