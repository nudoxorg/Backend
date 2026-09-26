use super::ids::{AtomListId, EntityListId, FreePredicateListId, TypeListId, TypeParameterListId};
use super::packed_types::TypeParameter;
use super::relations::{Confidence, SourceSpan};
use crate::ir::{AtomId, TypeId};
use crate::vocabulary::{Language, LanguageProfile};

/// TypeScript declaration-specific facts kept out of generic entity fields.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TypeScriptFacts {
    pub type_parameters: TypeParameterListId,
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
    Oblivious,
    NonNullable,
    Nullable,
}

/// C# parameter and return reference convention.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpReferenceKind {
    Value,
    In,
    Ref,
    Out,
}

/// Independent C# member effects; the three flags may occur together.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpMemberEffects {
    pub is_async: bool,
    pub is_iterator: bool,
    pub is_extension: bool,
}

/// Whether a C# declaration is one half of a partial declaration.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CSharpPartialRole {
    None,
    Definition,
    Implementation,
}

/// C# facts not shared by the language-neutral declaration row.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CSharpFacts {
    pub nullability: CSharpNullability,
    pub reference_kind: CSharpReferenceKind,
    pub constraints: TypeParameterListId,
    pub effects: CSharpMemberEffects,
    pub attributes: AtomListId,
    pub partial: CSharpPartialRole,
    pub xml_provenance: Option<SourceSpan>,
}

/// Signature lists carried by a Go declaration. Both lists are shared type arenas.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoSignature {
    pub parameters: TypeListId,
    pub results: TypeListId,
    pub variadic: bool,
}

/// Go facts that cannot be inferred from generic declarations and links.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GoFacts {
    pub signature: GoSignature,
    pub type_parameters: TypeParameterListId,
    pub fields: EntityListId,
    pub method_set: EntityListId,
    pub build_constraints: AtomListId,
    pub constant_value: AtomListId,
    pub constant_group: i64,
    pub constant_flags: u32,
}

/// Rust ownership fact attached to one declaration or parameter.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RustOwnership {
    Value,
    SharedBorrow,
    MutableBorrow,
    Moved,
}

/// Rust-only declaration facts; text is interned in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RustFacts {
    pub ownership: RustOwnership,
    pub lifetimes: AtomListId,
    pub where_clauses: TypeParameterListId,
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
    PositionalOnly,
    PositionalOrKeyword,
    VariadicPositional,
    KeywordOnly,
    VariadicKeyword,
}

/// Python-only source facts. Dynamic confidence is the existing quality lattice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PythonFacts {
    pub decorators: AtomListId,
    pub parameter_kind: PythonParameterKind,
    pub dynamic_confidence: Confidence,
}

/// Java-specific declaration facts for checked exceptions and record structure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JavaFacts {
    pub throws: TypeListId,
    pub annotations: AtomListId,
    pub overloads: EntityListId,
    pub record_components: EntityListId,
}

/// Clang type qualifier bits kept separate from the generic type graph.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
    pub is_restrict: bool,
}

/// Clang storage-class fact.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ClangStorageClass {
    None,
    Auto,
    Static,
    Extern,
    Register,
    ThreadLocal,
}

/// Optional layout facts measured in bits; `None` means the frontend did not own a layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangLayout {
    pub size_bits: Option<u32>,
    pub align_bits: Option<u32>,
}

/// Clang-only semantic facts. Include spelling is held in the shared atom arena.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClangFacts {
    pub qualifiers: ClangQualifiers,
    pub storage: ClangStorageClass,
    pub layout: ClangLayout,
    pub templates: TypeParameterListId,
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
    TypeScript(&'facts TypeScriptFacts),
    CSharp(&'facts CSharpFacts),
    Go(&'facts GoFacts),
    Rust(&'facts RustFacts),
    Python(&'facts PythonFacts),
    Java(&'facts JavaFacts),
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
