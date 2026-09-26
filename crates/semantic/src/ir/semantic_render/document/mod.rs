//! Authority-backed semantic documentation, deliberately distinct from source syntax.
//!
//! The canonical image retains declaration, documentation, type, link, and
//! language-extension facts.  It does **not** retain token trivia or the
//! declarator ownership needed to reconstruct a source file.  This module
//! therefore emits one closed semantic-document grammar and makes source
//! syntax an explicit unsupported contract rather than presenting a
//! plausible-looking reconstruction.

use core::{convert::Infallible, fmt, ops::Deref, str};

use thiserror::Error;

use crate::ir::{
    AtomId, AtomListId, CSharpVersion, CStandard, CanonicalTypeRenderError,
    CanonicalTypeRenderLimits, CxxStandard, DocId, EntityId, EntityListId, ExternalId,
    FactAvailability, FreePredicateListId, GoVersion, JavaRelease, LanguageProfile,
    ObjectMemberListId, PythonVersion, RustEdition, SemanticImageAuthority, SemanticReader,
    TemplatePartListId, TupleElementListId, TypeId, TypeListId, TypeParameterBoundListId,
    TypeParameterListId, TypeScriptSource,
};

/// Fact plane named by a semantic-document admission or reference failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDocumentFact {
    /// The finalized image authority did not name a matching language profile.
    ImageProfile,
    /// The entity's documentation plane.
    Documentation,
    /// The entity's semantic-type plane.
    SemanticType,
    /// The entity's source/source-file plane.
    Source,
    /// The selected named language-extension plane.
    LanguageExtension,
    /// Lossless source syntax, which the semantic image deliberately lacks.
    SourceSyntax,
}

/// Exact coordinate carried by a dangling semantic-document reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticDocumentReference {
    /// An atom in an entity, source, or foreign endpoint.
    Atom(AtomId),
    /// A UTF-8 document-text coordinate.
    Text(crate::ir::TextId),
    /// A documentation fragment list.
    Documentation(DocId),
    /// A local declaration target.
    Entity(EntityId),
    /// A semantic type row.
    Type(TypeId),
    /// An ordered semantic type list.
    TypeList(TypeListId),
    /// An ordered atom list.
    AtomList(AtomListId),
    /// An ordered entity/member list.
    EntityList(EntityListId),
    /// An ordered generic-parameter list.
    TypeParameterList(TypeParameterListId),
    /// An ordered bound list owned by one generic parameter.
    TypeParameterBoundList(TypeParameterBoundListId),
    /// An ordered Rust free-predicate list.
    FreePredicateList(FreePredicateListId),
    /// A tuple/callable element list nested in a semantic type.
    TupleElements(TupleElementListId),
    /// An object-member list nested in a semantic type.
    ObjectMembers(ObjectMemberListId),
    /// A template-literal part list nested in a semantic type.
    TemplateParts(TemplatePartListId),
    /// An external target row.
    External(ExternalId),
}

/// Exact semantic-document preparation or output terminal.
#[derive(Debug, Error)]
pub enum SemanticDocumentError {
    /// The requested declaration is absent from the finalized image.
    #[error("semantic document requested missing entity {entity:?}")]
    MissingEntity { entity: EntityId },
    /// A shared image does not prove a language profile for document dialect selection.
    #[error(
        "semantic document for {entity:?} requires a language image, but authority is {authority:?}"
    )]
    ProfileUnavailable {
        entity: EntityId,
        authority: SemanticImageAuthority,
    },
    /// The caller selected a profile other than the image's authority profile.
    #[error(
        "semantic document for {entity:?} requested {requested:?}, but image authority retained {observed:?}"
    )]
    ProfileMismatch {
        entity: EntityId,
        requested: LanguageProfile,
        observed: LanguageProfile,
    },
    /// A required semantic plane was explicitly unavailable.
    #[error(
        "semantic document for {entity:?} under {profile:?} cannot render unavailable {fact:?}"
    )]
    UnsupportedFact {
        entity: EntityId,
        profile: LanguageProfile,
        fact: SemanticDocumentFact,
    },
    /// Authority availability and the retained value disagree.
    #[error(
        "semantic document entity {entity:?} has {fact:?} availability {claimed:?} but value presence is {present}"
    )]
    AuthorityMismatch {
        entity: EntityId,
        fact: SemanticDocumentFact,
        claimed: FactAvailability,
        present: bool,
    },
    /// A retained semantic coordinate could not be resolved through the reader.
    #[error("semantic document entity {entity:?} names missing {reference:?}")]
    MissingReference {
        entity: EntityId,
        reference: SemanticDocumentReference,
    },
    /// The shared canonical type renderer rejected an admitted semantic type.
    #[error("semantic document entity {entity:?} rejected canonical type {semantic_type:?}")]
    CanonicalType {
        entity: EntityId,
        semantic_type: TypeId,
        #[source]
        cause: CanonicalTypeRenderError,
    },
    /// Exact output did not fit the caller's storage before writing began.
    #[error(
        "semantic document entity {entity:?} needs {required} bytes but caller supplied {available}"
    )]
    OutputTooSmall {
        entity: EntityId,
        required: usize,
        available: usize,
    },
    /// Measuring the canonical semantic-document grammar overflowed `usize`.
    #[error("semantic document entity {entity:?} output length overflowed")]
    OutputLengthOverflow { entity: EntityId },
    /// A prepared output did not reproduce its admission length.
    #[error(
        "prepared semantic document entity {entity:?} wrote {written} bytes rather than promised {promised}"
    )]
    PreparedLengthMismatch {
        entity: EntityId,
        promised: usize,
        written: usize,
    },
    /// A caller formatter rejected an otherwise admitted semantic document.
    #[error("the caller formatter rejected semantic document output for {entity:?}")]
    OutputWrite { entity: EntityId },
    /// The byte writer violated the all-ASCII semantic-document grammar.
    #[error(
        "semantic document entity {entity:?} output was invalid UTF-8 after byte {valid_up_to}"
    )]
    OutputEncoding {
        entity: EntityId,
        valid_up_to: usize,
        #[source]
        source: str::Utf8Error,
    },
}

/// Exact source-syntax terminal kept separate from semantic documentation.
#[derive(Debug, Error)]
pub enum SourceSyntaxError {
    /// The requested entity was absent before source-syntax admission.
    #[error("source syntax requested missing entity {entity:?}")]
    MissingEntity { entity: EntityId },
    /// The image did not prove one language source grammar.
    #[error(
        "source syntax for {entity:?} requires a language image, but authority is {authority:?}"
    )]
    ProfileUnavailable {
        entity: EntityId,
        authority: SemanticImageAuthority,
    },
    /// The caller-selected source grammar did not match image authority.
    #[error(
        "source syntax for {entity:?} requested {requested:?}, but image authority retained {observed:?}"
    )]
    ProfileMismatch {
        entity: EntityId,
        requested: LanguageProfile,
        observed: LanguageProfile,
    },
    /// The durable semantic image does not prove lossless source syntax.
    #[error(
        "source syntax for {entity:?} under {profile:?} is unsupported because {fact:?} is not retained"
    )]
    UnsupportedFact {
        entity: EntityId,
        profile: LanguageProfile,
        fact: SemanticDocumentFact,
    },
}

/// Immutable facts promised by a prepared semantic document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedSemanticDocumentView {
    /// Exact selected profile, proven equal to the image authority profile.
    pub profile: LanguageProfile,
    /// Exact declaration row rendered by this document.
    pub entity: EntityId,
    /// Exact canonical byte count produced after successful preparation.
    pub encoded_len: usize,
    /// Type traversal law used for a captured semantic type.
    pub type_limits: CanonicalTypeRenderLimits,
}

/// A fully admitted semantic document over one static semantic reader.
pub struct PreparedSemanticDocument<'image, Reader: SemanticReader + ?Sized> {
    reader: &'image Reader,
    view: PreparedSemanticDocumentView,
    language: SemanticDocumentLanguage,
}

impl<Reader: SemanticReader + ?Sized> Deref for PreparedSemanticDocument<'_, Reader> {
    type Target = PreparedSemanticDocumentView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'image, Reader: SemanticReader + ?Sized> PreparedSemanticDocument<'image, Reader> {
    /// Writes a prepared document atomically into caller-owned bytes.
    pub fn write_into<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output str, SemanticDocumentError> {
        if output.len() < self.encoded_len {
            return Err(SemanticDocumentError::OutputTooSmall {
                entity: self.entity,
                required: self.encoded_len,
                available: output.len(),
            });
        }
        let written = {
            let mut writer = codec::ByteWriter::new(output);
            emit::emit_document(
                self.reader,
                self.profile,
                self.entity,
                self.language,
                self.type_limits,
                &mut writer,
            )?;
            if writer.written != self.encoded_len {
                return Err(SemanticDocumentError::PreparedLengthMismatch {
                    entity: self.entity,
                    promised: self.encoded_len,
                    written: writer.written,
                });
            }
            writer.written
        };
        str::from_utf8(&output[..written]).map_err(|source| SemanticDocumentError::OutputEncoding {
            entity: self.entity,
            valid_up_to: source.valid_up_to(),
            source,
        })
    }

    /// Streams the already-admitted semantic document to a caller formatter.
    pub fn write_to(&self, output: &mut impl fmt::Write) -> Result<(), SemanticDocumentError> {
        emit::emit_document(
            self.reader,
            self.profile,
            self.entity,
            self.language,
            self.type_limits,
            output,
        )
    }
}

/// Static per-family semantic-document policy.  Each implementation selects
/// one named sparse extension plane; no erased extension union is exposed by
/// the reader or stored in a prepared document.
pub trait SemanticDocumentDialect<L>: super::sealed::Sealed {
    /// Prepares the dialect's authority-backed semantic document.
    fn prepare<'image, Reader: SemanticReader + ?Sized>(
        profile: L,
        reader: &'image Reader,
        entity: EntityId,
        type_limits: CanonicalTypeRenderLimits,
    ) -> Result<PreparedSemanticDocument<'image, Reader>, SemanticDocumentError>;
}

/// Static source-syntax contract.  It is intentionally separate from
/// [`SemanticDocumentDialect`]: semantic documentation cannot be relabelled
/// as a source reconstruction.
pub trait SourceSyntaxDialect<L>: super::sealed::Sealed {
    /// Source reconstruction is typed unsupported until a schema proves it.
    fn prepare<Reader: SemanticReader + ?Sized>(
        profile: L,
        reader: &Reader,
        entity: EntityId,
    ) -> Result<Infallible, SourceSyntaxError>;
}

/// TypeScript semantic-document policy.
pub struct TypeScriptSemanticDocumentDialect;
/// C# semantic-document policy.
pub struct CSharpSemanticDocumentDialect;
/// Go semantic-document policy.
pub struct GoSemanticDocumentDialect;
/// Rust semantic-document policy.
pub struct RustSemanticDocumentDialect;
/// Python semantic-document policy.
pub struct PythonSemanticDocumentDialect;
/// Java semantic-document policy.
pub struct JavaSemanticDocumentDialect;
/// C/C++ semantic-document policy backed by the Clang sparse plane.
pub struct CFamilySemanticDocumentDialect;
/// Explicit source-syntax policy which is intentionally unsupported today.
pub struct SemanticImageSourceSyntaxDialect;

impl super::sealed::Sealed for TypeScriptSemanticDocumentDialect {}
impl super::sealed::Sealed for CSharpSemanticDocumentDialect {}
impl super::sealed::Sealed for GoSemanticDocumentDialect {}
impl super::sealed::Sealed for RustSemanticDocumentDialect {}
impl super::sealed::Sealed for PythonSemanticDocumentDialect {}
impl super::sealed::Sealed for JavaSemanticDocumentDialect {}
impl super::sealed::Sealed for CFamilySemanticDocumentDialect {}
impl super::sealed::Sealed for SemanticImageSourceSyntaxDialect {}

macro_rules! dialect {
    ($dialect:ty, $profile:ty, $variant:ident, $lookup:ident, $language:ident) => {
        impl SemanticDocumentDialect<$profile> for $dialect {
            fn prepare<'image, Reader: SemanticReader + ?Sized>(
                profile: $profile,
                reader: &'image Reader,
                entity: EntityId,
                type_limits: CanonicalTypeRenderLimits,
            ) -> Result<PreparedSemanticDocument<'image, Reader>, SemanticDocumentError> {
                let profile = LanguageProfile::$variant(profile);
                prepare_selected(
                    profile,
                    reader,
                    entity,
                    type_limits,
                    SemanticDocumentLanguage::$language,
                    reader.$lookup(entity).is_some(),
                )
            }
        }

        impl SourceSyntaxDialect<$profile> for SemanticImageSourceSyntaxDialect {
            fn prepare<Reader: SemanticReader + ?Sized>(
                profile: $profile,
                reader: &Reader,
                entity: EntityId,
            ) -> Result<Infallible, SourceSyntaxError> {
                source_syntax_unavailable(LanguageProfile::$variant(profile), reader, entity)
            }
        }
    };
}

dialect!(
    TypeScriptSemanticDocumentDialect,
    TypeScriptSource,
    TypeScript,
    typescript_extension,
    TypeScript
);
dialect!(
    CSharpSemanticDocumentDialect,
    CSharpVersion,
    CSharp,
    csharp_extension,
    CSharp
);
dialect!(GoSemanticDocumentDialect, GoVersion, Go, go_extension, Go);
dialect!(
    RustSemanticDocumentDialect,
    RustEdition,
    Rust,
    rust_extension,
    Rust
);
dialect!(
    PythonSemanticDocumentDialect,
    PythonVersion,
    Python,
    python_extension,
    Python
);
dialect!(
    JavaSemanticDocumentDialect,
    JavaRelease,
    Java,
    java_extension,
    Java
);
dialect!(
    CFamilySemanticDocumentDialect,
    CStandard,
    C,
    clang_extension,
    Clang
);
dialect!(
    CFamilySemanticDocumentDialect,
    CxxStandard,
    Cxx,
    clang_extension,
    Clang
);

/// Selects one of seven static semantic-document policies at the image edge.
pub fn prepare_semantic_document<'image, Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &'image Reader,
    entity: EntityId,
    type_limits: CanonicalTypeRenderLimits,
) -> Result<PreparedSemanticDocument<'image, Reader>, SemanticDocumentError> {
    match profile {
        LanguageProfile::TypeScript(profile) => {
            TypeScriptSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::CSharp(profile) => {
            CSharpSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::Go(profile) => {
            GoSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::Rust(profile) => {
            RustSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::Python(profile) => {
            PythonSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::Java(profile) => {
            JavaSemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::C(profile) => {
            CFamilySemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
        LanguageProfile::Cxx(profile) => {
            CFamilySemanticDocumentDialect::prepare(profile, reader, entity, type_limits)
        }
    }
}

/// Rejects source reconstruction under the same exact profile admission law.
pub fn prepare_source_syntax<Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &Reader,
    entity: EntityId,
) -> Result<Infallible, SourceSyntaxError> {
    match profile {
        LanguageProfile::TypeScript(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::CSharp(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Go(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Rust(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Python(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Java(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::C(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
        LanguageProfile::Cxx(profile) => {
            SemanticImageSourceSyntaxDialect::prepare(profile, reader, entity)
        }
    }
}

#[derive(Clone, Copy)]
enum SemanticDocumentLanguage {
    TypeScript,
    CSharp,
    Go,
    Rust,
    Python,
    Java,
    Clang,
}

impl SemanticDocumentLanguage {
    const fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::CSharp => "csharp",
            Self::Go => "go",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Java => "java",
            Self::Clang => "clang",
        }
    }
}

fn prepare_selected<'image, Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &'image Reader,
    entity: EntityId,
    type_limits: CanonicalTypeRenderLimits,
    language: SemanticDocumentLanguage,
    extension_present: bool,
) -> Result<PreparedSemanticDocument<'image, Reader>, SemanticDocumentError> {
    let row = reader
        .entity(entity)
        .ok_or(SemanticDocumentError::MissingEntity { entity })?;
    validate_profile(reader.image_facts().authority, profile, entity)?;
    validate_availability(
        entity,
        SemanticDocumentFact::LanguageExtension,
        row.authority.language_extension,
        extension_present,
    )?;
    let mut count = codec::CountWriter::new();
    emit::emit_document(reader, profile, entity, language, type_limits, &mut count)?;
    let encoded_len = count
        .length
        .ok_or(SemanticDocumentError::OutputLengthOverflow { entity })?;
    Ok(PreparedSemanticDocument {
        reader,
        view: PreparedSemanticDocumentView {
            profile,
            entity,
            encoded_len,
            type_limits,
        },
        language,
    })
}

fn source_syntax_unavailable<Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &Reader,
    entity: EntityId,
) -> Result<Infallible, SourceSyntaxError> {
    if reader.entity(entity).is_none() {
        return Err(SourceSyntaxError::MissingEntity { entity });
    }
    match reader.image_facts().authority {
        SemanticImageAuthority::Shared => Err(SourceSyntaxError::ProfileUnavailable {
            entity,
            authority: SemanticImageAuthority::Shared,
        }),
        SemanticImageAuthority::Language(observed) if observed != profile => {
            Err(SourceSyntaxError::ProfileMismatch {
                entity,
                requested: profile,
                observed,
            })
        }
        SemanticImageAuthority::Language(_) => Err(SourceSyntaxError::UnsupportedFact {
            entity,
            profile,
            fact: SemanticDocumentFact::SourceSyntax,
        }),
    }
}

fn validate_profile(
    authority: SemanticImageAuthority,
    requested: LanguageProfile,
    entity: EntityId,
) -> Result<(), SemanticDocumentError> {
    match authority {
        SemanticImageAuthority::Shared => {
            Err(SemanticDocumentError::ProfileUnavailable { entity, authority })
        }
        SemanticImageAuthority::Language(observed) if observed != requested => {
            Err(SemanticDocumentError::ProfileMismatch {
                entity,
                requested,
                observed,
            })
        }
        SemanticImageAuthority::Language(_) => Ok(()),
    }
}

fn validate_availability(
    entity: EntityId,
    fact: SemanticDocumentFact,
    claimed: FactAvailability,
    present: bool,
) -> Result<(), SemanticDocumentError> {
    if matches!(claimed, FactAvailability::Captured) == present {
        Ok(())
    } else {
        Err(SemanticDocumentError::AuthorityMismatch {
            entity,
            fact,
            claimed,
            present,
        })
    }
}

mod codec;
mod emit;
mod extension;
