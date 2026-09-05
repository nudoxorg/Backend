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

use crate::{
    prepare_canonical_type, AtomId, AtomListId, CSharpFacts, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, CSharpVersion, CStandard, CanonicalTypeRenderError,
    CanonicalTypeRenderLimits, CanonicalTypeRenderReference, ClangFacts, ClangStorageClass,
    Confidence, CxxStandard, DeclarationIdentity, DocFragment, DocId, EntityId, EntityListId,
    ExternalId, ExternalTarget, FactAvailability, ForeignTargetOrigin, GoFacts, GoVersion,
    JavaFacts, JavaRelease, LanguageProfile, ObjectMemberListId, ParentageAuthority, PythonFacts,
    PythonParameterKind, PythonVersion, RustEdition, RustFacts, SemanticImageAuthority,
    SemanticReader, SourceSpan, TemplatePartListId, TupleElementListId, TypeId, TypeListId,
    TypeParameter, TypeParameterBound, TypeParameterBoundListId, TypeParameterInference,
    TypeParameterListId, TypeParameterPrimaryRequirement, TypeParameterRequirements,
    TypeScriptSource,
};

use super::{kind_name, visibility_name};

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
    Text(crate::TextId),
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
            let mut writer = ByteWriter::new(output);
            emit_document(
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
        emit_document(
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
    let mut count = CountWriter::new();
    emit_document(reader, profile, entity, language, type_limits, &mut count)?;
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

fn emit_document<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    profile: LanguageProfile,
    entity: EntityId,
    language: SemanticDocumentLanguage,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let row = reader
        .entity(entity)
        .ok_or(SemanticDocumentError::MissingEntity { entity })?;
    validate_profile(reader.image_facts().authority, profile, entity)?;
    write_text(entity, output, "semantic-document(profile=")?;
    write_bytes(entity, output, &<[u8; 2]>::from(profile))?;
    write_text(entity, output, ",dialect=")?;
    write_text(entity, output, language.name())?;
    write_text(entity, output, ",entity=")?;
    emit_identity(entity, output, row.version.identity())?;
    write_text(entity, output, ",kind=")?;
    write_text(entity, output, kind_name(row.kind))?;
    write_text(entity, output, ",visibility=")?;
    write_text(entity, output, visibility_name(row.visibility))?;
    write_text(entity, output, ",name=")?;
    emit_atom(reader, entity, row.name, output)?;
    write_text(entity, output, ",parentage=")?;
    emit_parentage(entity, output, row.authority.parentage)?;
    write_text(entity, output, ",extension=")?;
    let present = extension_present(reader, language, entity);
    validate_availability(
        entity,
        SemanticDocumentFact::LanguageExtension,
        row.authority.language_extension,
        present,
    )?;
    match row.authority.language_extension {
        FactAvailability::Captured => {
            write_text(entity, output, language.name())?;
            write_text(entity, output, "(captured)")?;
        }
        FactAvailability::Unavailable => write_text(entity, output, "unavailable")?,
    }
    write_text(entity, output, ",source=")?;
    emit_source(reader, entity, row.authority.source, row.source, output)?;
    write_text(entity, output, ",semantic-type=")?;
    emit_semantic_type(
        reader,
        entity,
        row.authority.semantic_type,
        row.semantic_type,
        type_limits,
        output,
    )?;
    write_text(entity, output, ",documentation=")?;
    emit_documentation(
        reader,
        entity,
        row.authority.documentation,
        row.docs,
        output,
    )?;
    // Keep the historical compact extension marker above stable while
    // appending the complete selected named plane here.  The payload is read
    // only through `SemanticReader`; every typed list coordinate is resolved
    // before preparation succeeds and is rendered again by `write_into`.
    write_text(entity, output, ",extension-facts=")?;
    if row.authority.language_extension == FactAvailability::Captured {
        emit_extension_facts(reader, entity, language, type_limits, output)?;
    } else {
        write_text(entity, output, "unavailable")?;
    }
    write_text(entity, output, ")")
}

fn extension_present<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    language: SemanticDocumentLanguage,
    entity: EntityId,
) -> bool {
    match language {
        SemanticDocumentLanguage::TypeScript => reader.typescript_extension(entity).is_some(),
        SemanticDocumentLanguage::CSharp => reader.csharp_extension(entity).is_some(),
        SemanticDocumentLanguage::Go => reader.go_extension(entity).is_some(),
        SemanticDocumentLanguage::Rust => reader.rust_extension(entity).is_some(),
        SemanticDocumentLanguage::Python => reader.python_extension(entity).is_some(),
        SemanticDocumentLanguage::Java => reader.java_extension(entity).is_some(),
        SemanticDocumentLanguage::Clang => reader.clang_extension(entity).is_some(),
    }
}

fn emit_extension_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    language: SemanticDocumentLanguage,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match language {
        SemanticDocumentLanguage::TypeScript => {
            let facts = reader
                .typescript_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_typescript_facts(reader, entity, facts, type_limits, output)
        }
        SemanticDocumentLanguage::CSharp => {
            let facts = reader
                .csharp_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_csharp_facts(reader, entity, facts, type_limits, output)
        }
        SemanticDocumentLanguage::Go => {
            let facts = reader
                .go_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_go_facts(reader, entity, facts, type_limits, output)
        }
        SemanticDocumentLanguage::Rust => {
            let facts = reader
                .rust_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_rust_facts(reader, entity, facts, type_limits, output)
        }
        SemanticDocumentLanguage::Python => {
            let facts = reader
                .python_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_python_facts(reader, entity, facts, output)
        }
        SemanticDocumentLanguage::Java => {
            let facts = reader
                .java_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_java_facts(reader, entity, facts, type_limits, output)
        }
        SemanticDocumentLanguage::Clang => {
            let facts = reader
                .clang_extension(entity)
                .ok_or(extension_authority_mismatch(entity))?;
            emit_clang_facts(reader, entity, facts, type_limits, output)
        }
    }
}

const fn extension_authority_mismatch(entity: EntityId) -> SemanticDocumentError {
    SemanticDocumentError::AuthorityMismatch {
        entity,
        fact: SemanticDocumentFact::LanguageExtension,
        claimed: FactAvailability::Captured,
        present: false,
    }
}

fn emit_typescript_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: crate::TypeScriptFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "typescript(type-parameters=")?;
    emit_type_parameter_list(reader, entity, facts.type_parameters, type_limits, output)?;
    write_text(entity, output, ",declared=")?;
    emit_optional_type(reader, entity, facts.declared, type_limits, output)?;
    write_text(entity, output, ",observed=")?;
    emit_optional_type(reader, entity, facts.observed, type_limits, output)?;
    write_text(entity, output, ")")
}

fn emit_csharp_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: CSharpFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "csharp(nullability=")?;
    write_text(entity, output, csharp_nullability_name(facts.nullability))?;
    write_text(entity, output, ",reference-kind=")?;
    write_text(
        entity,
        output,
        csharp_reference_kind_name(facts.reference_kind),
    )?;
    write_text(entity, output, ",constraints=")?;
    emit_type_parameter_list(reader, entity, facts.constraints, type_limits, output)?;
    write_text(entity, output, ",effects=(async=")?;
    write_bool(entity, output, facts.effects.is_async)?;
    write_text(entity, output, ",iterator=")?;
    write_bool(entity, output, facts.effects.is_iterator)?;
    write_text(entity, output, ",extension=")?;
    write_bool(entity, output, facts.effects.is_extension)?;
    write_text(entity, output, "),attributes=")?;
    emit_atom_list(reader, entity, facts.attributes, output)?;
    write_text(entity, output, ",partial=")?;
    write_text(entity, output, csharp_partial_role_name(facts.partial))?;
    write_text(entity, output, ",xml-provenance=")?;
    emit_optional_source_span(reader, entity, facts.xml_provenance, output)?;
    write_text(entity, output, ")")
}

fn emit_go_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: GoFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "go(signature(parameters=")?;
    emit_type_list(
        reader,
        entity,
        facts.signature.parameters,
        type_limits,
        output,
    )?;
    write_text(entity, output, ",results=")?;
    emit_type_list(reader, entity, facts.signature.results, type_limits, output)?;
    write_text(entity, output, ",variadic=")?;
    write_bool(entity, output, facts.signature.variadic)?;
    write_text(entity, output, "),type-parameters=")?;
    emit_type_parameter_list(reader, entity, facts.type_parameters, type_limits, output)?;
    write_text(entity, output, ",fields=")?;
    emit_entity_list(reader, entity, facts.fields, output)?;
    write_text(entity, output, ",method-set=")?;
    emit_entity_list(reader, entity, facts.method_set, output)?;
    write_text(entity, output, ",build-constraints=")?;
    emit_atom_list(reader, entity, facts.build_constraints, output)?;
    write_text(entity, output, ",constant-value=")?;
    emit_atom_list(reader, entity, facts.constant_value, output)?;
    write_text(entity, output, ",constant-group=")?;
    write_signed(entity, output, facts.constant_group)?;
    write_text(entity, output, ",constant-flags=")?;
    write_number(entity, output, u64::from(facts.constant_flags))?;
    write_text(entity, output, ")")
}

fn emit_rust_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: RustFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "rust(ownership=")?;
    write_text(entity, output, rust_ownership_name(facts.ownership))?;
    write_text(entity, output, ",lifetimes=")?;
    emit_atom_list(reader, entity, facts.lifetimes, output)?;
    write_text(entity, output, ",where-clauses=")?;
    emit_type_parameter_list(reader, entity, facts.where_clauses, type_limits, output)?;
    write_text(entity, output, ",macros=")?;
    emit_atom_list(reader, entity, facts.macros, output)?;
    write_text(entity, output, ")")
}

fn emit_python_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: PythonFacts,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "python(decorators=")?;
    emit_atom_list(reader, entity, facts.decorators, output)?;
    write_text(entity, output, ",parameter-kind=")?;
    write_text(
        entity,
        output,
        python_parameter_kind_name(facts.parameter_kind),
    )?;
    write_text(entity, output, ",dynamic-confidence=")?;
    write_text(entity, output, confidence_name(facts.dynamic_confidence))?;
    write_text(entity, output, ")")
}

fn emit_java_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: JavaFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "java(throws=")?;
    emit_type_list(reader, entity, facts.throws, type_limits, output)?;
    write_text(entity, output, ",annotations=")?;
    emit_atom_list(reader, entity, facts.annotations, output)?;
    write_text(entity, output, ",overloads=")?;
    emit_entity_list(reader, entity, facts.overloads, output)?;
    write_text(entity, output, ",record-components=")?;
    emit_entity_list(reader, entity, facts.record_components, output)?;
    write_text(entity, output, ")")
}

fn emit_clang_facts<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    facts: ClangFacts,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "clang(qualifiers=(const=")?;
    write_bool(entity, output, facts.qualifiers.is_const)?;
    write_text(entity, output, ",volatile=")?;
    write_bool(entity, output, facts.qualifiers.is_volatile)?;
    write_text(entity, output, ",restrict=")?;
    write_bool(entity, output, facts.qualifiers.is_restrict)?;
    write_text(entity, output, "),storage=")?;
    write_text(entity, output, clang_storage_name(facts.storage))?;
    write_text(entity, output, ",layout=(size-bits=")?;
    emit_optional_u32(entity, facts.layout.size_bits, output)?;
    write_text(entity, output, ",align-bits=")?;
    emit_optional_u32(entity, facts.layout.align_bits, output)?;
    write_text(entity, output, "),templates=")?;
    emit_type_parameter_list(reader, entity, facts.templates, type_limits, output)?;
    write_text(entity, output, ",includes=")?;
    emit_atom_list(reader, entity, facts.includes, output)?;
    write_text(entity, output, ")")
}

fn emit_optional_source_span<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    source: Option<SourceSpan>,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match source {
        None => write_text(entity, output, "none"),
        Some(source) => {
            write_text(entity, output, "span(file=")?;
            emit_atom(reader, entity, source.file(), output)?;
            write_text(entity, output, ",start=")?;
            write_number(entity, output, u64::from(source.start()))?;
            write_text(entity, output, ",end=")?;
            write_number(entity, output, u64::from(source.end()))?;
            write_text(entity, output, ")")
        }
    }
}

fn emit_optional_u32(
    entity: EntityId,
    value: Option<u32>,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match value {
        Some(value) => write_number(entity, output, u64::from(value)),
        None => write_text(entity, output, "none"),
    }
}

fn emit_optional_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    value: Option<TypeId>,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match value {
        Some(value) => {
            write_text(entity, output, "some(")?;
            emit_type_coordinate(reader, entity, value, type_limits, output)?;
            write_text(entity, output, ")")
        }
        None => write_text(entity, output, "none"),
    }
}

fn emit_type_coordinate<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    semantic_type: TypeId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "type=")?;
    emit_canonical_type(reader, entity, semantic_type, type_limits, output)
}

fn emit_canonical_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    semantic_type: TypeId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let prepared = prepare_canonical_type(reader, semantic_type, type_limits)
        .map_err(|cause| map_canonical_type_error(entity, semantic_type, cause))?;
    prepared
        .write_to(output)
        .map_err(|cause| map_canonical_type_error(entity, semantic_type, cause))
}

fn emit_type_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let types = reader
        .types(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::TypeList(list),
        })?;
    write_text(entity, output, "type-list(values=[")?;
    for (index, semantic_type) in types.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_type_coordinate(reader, entity, semantic_type, type_limits, output)?;
    }
    write_text(entity, output, "])")
}

fn emit_atom_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: AtomListId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let atoms = reader
        .atom_list(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::AtomList(list),
        })?;
    write_text(entity, output, "atom-list(values=[")?;
    for (index, atom) in atoms.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        write_text(entity, output, "atom=")?;
        emit_atom(reader, entity, atom, output)?;
    }
    write_text(entity, output, "])")
}

fn emit_entity_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: EntityListId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let entities = reader
        .entity_list(list)
        .ok_or(SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::EntityList(list),
        })?;
    write_text(entity, output, "entity-list(values=[")?;
    for (index, target) in entities.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_entity_coordinate(reader, entity, target, output)?;
    }
    write_text(entity, output, "])")
}

fn emit_entity_coordinate<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    target: EntityId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let row = reader
        .entity(target)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Entity(target),
        })?;
    write_text(owner, output, "entity(identity=")?;
    emit_identity(owner, output, row.version.identity())?;
    write_text(owner, output, ",name=")?;
    emit_atom(reader, owner, row.name, output)?;
    write_text(owner, output, ")")
}

fn emit_type_parameter_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeParameterListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let parameters =
        reader
            .type_parameters(list)
            .ok_or(SemanticDocumentError::MissingReference {
                entity,
                reference: SemanticDocumentReference::TypeParameterList(list),
            })?;
    write_text(entity, output, "type-parameter-list(values=[")?;
    for (index, parameter) in parameters.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_type_parameter(reader, entity, parameter, type_limits, output)?;
    }
    write_text(entity, output, "])")
}

fn emit_type_parameter<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    parameter: TypeParameter,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "parameter(name=")?;
    emit_atom(reader, entity, parameter.name, output)?;
    write_text(entity, output, ",bounds=")?;
    emit_type_parameter_bound_list(reader, entity, parameter.bounds, type_limits, output)?;
    write_text(entity, output, ",default=")?;
    emit_optional_type(reader, entity, parameter.default, type_limits, output)?;
    write_text(entity, output, ",variance=")?;
    write_text(entity, output, variance_name(parameter.variance))?;
    write_text(entity, output, ",kind=")?;
    emit_type_parameter_kind(reader, entity, parameter.kind, type_limits, output)?;
    write_text(entity, output, ",requirements=")?;
    emit_type_parameter_requirements(entity, parameter.requirements, output)?;
    write_text(entity, output, ")")
}

fn emit_type_parameter_bound_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: TypeParameterBoundListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let bounds =
        reader
            .type_parameter_bounds(list)
            .ok_or(SemanticDocumentError::MissingReference {
                entity,
                reference: SemanticDocumentReference::TypeParameterBoundList(list),
            })?;
    write_text(entity, output, "bound-list(values=[")?;
    for (index, bound) in bounds.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        match bound {
            TypeParameterBound::Type(semantic_type) => {
                emit_type_coordinate(reader, entity, semantic_type, type_limits, output)?;
            }
            TypeParameterBound::Lifetime(atom) => {
                write_text(entity, output, "lifetime(atom=")?;
                emit_atom(reader, entity, atom, output)?;
                write_text(entity, output, ")")?;
            }
        }
    }
    write_text(entity, output, "])")
}

fn emit_type_parameter_kind<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    kind: crate::TypeParameterKind,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match kind {
        crate::TypeParameterKind::Type { inference } => {
            write_text(entity, output, "type(inference=")?;
            write_text(entity, output, type_parameter_inference_name(inference))?;
            write_text(entity, output, ")")
        }
        crate::TypeParameterKind::ConstValue { value_type } => {
            write_text(entity, output, "const-value(type=")?;
            emit_type_coordinate(reader, entity, value_type, type_limits, output)?;
            write_text(entity, output, ")")
        }
        crate::TypeParameterKind::Lifetime => write_text(entity, output, "lifetime"),
    }
}

fn emit_type_parameter_requirements(
    entity: EntityId,
    requirements: TypeParameterRequirements,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "(primary=")?;
    match requirements.primary {
        TypeParameterPrimaryRequirement::None => write_text(entity, output, "none")?,
        TypeParameterPrimaryRequirement::Reference { nullable } => {
            write_text(entity, output, "reference(nullable=")?;
            write_bool(entity, output, nullable)?;
            write_text(entity, output, ")")?;
        }
        TypeParameterPrimaryRequirement::Value => write_text(entity, output, "value")?,
        TypeParameterPrimaryRequirement::Unmanaged => write_text(entity, output, "unmanaged")?,
        TypeParameterPrimaryRequirement::NotNull => write_text(entity, output, "not-null")?,
        TypeParameterPrimaryRequirement::Default => write_text(entity, output, "default")?,
    }
    write_text(entity, output, ",constructor=")?;
    write_bool(entity, output, requirements.constructor)?;
    write_text(entity, output, ",allows-ref-like=")?;
    write_bool(entity, output, requirements.allows_ref_like)?;
    write_text(entity, output, ")")
}

fn map_canonical_type_error(
    entity: EntityId,
    semantic_type: TypeId,
    cause: CanonicalTypeRenderError,
) -> SemanticDocumentError {
    match cause {
        CanonicalTypeRenderError::MissingRoot { root } => SemanticDocumentError::MissingReference {
            entity,
            reference: SemanticDocumentReference::Type(root),
        },
        CanonicalTypeRenderError::MissingReference { reference, .. } => {
            SemanticDocumentError::MissingReference {
                entity,
                reference: map_canonical_reference(reference),
            }
        }
        cause => SemanticDocumentError::CanonicalType {
            entity,
            semantic_type,
            cause,
        },
    }
}

const fn map_canonical_reference(
    reference: CanonicalTypeRenderReference,
) -> SemanticDocumentReference {
    match reference {
        CanonicalTypeRenderReference::Type(value) => SemanticDocumentReference::Type(value),
        CanonicalTypeRenderReference::Atom(value) => SemanticDocumentReference::Atom(value),
        CanonicalTypeRenderReference::TypeList(value) => SemanticDocumentReference::TypeList(value),
        CanonicalTypeRenderReference::AtomList(value) => SemanticDocumentReference::AtomList(value),
        CanonicalTypeRenderReference::TupleElements(value) => {
            SemanticDocumentReference::TupleElements(value)
        }
        CanonicalTypeRenderReference::ObjectMembers(value) => {
            SemanticDocumentReference::ObjectMembers(value)
        }
        CanonicalTypeRenderReference::TemplateParts(value) => {
            SemanticDocumentReference::TemplateParts(value)
        }
        CanonicalTypeRenderReference::Entity(value) => SemanticDocumentReference::Entity(value),
        CanonicalTypeRenderReference::External(value) => SemanticDocumentReference::External(value),
    }
}

const fn csharp_nullability_name(value: CSharpNullability) -> &'static str {
    match value {
        CSharpNullability::Oblivious => "oblivious",
        CSharpNullability::NonNullable => "non-nullable",
        CSharpNullability::Nullable => "nullable",
    }
}

const fn csharp_reference_kind_name(value: CSharpReferenceKind) -> &'static str {
    match value {
        CSharpReferenceKind::Value => "value",
        CSharpReferenceKind::In => "in",
        CSharpReferenceKind::Ref => "ref",
        CSharpReferenceKind::Out => "out",
    }
}

const fn csharp_partial_role_name(value: CSharpPartialRole) -> &'static str {
    match value {
        CSharpPartialRole::None => "none",
        CSharpPartialRole::Definition => "definition",
        CSharpPartialRole::Implementation => "implementation",
    }
}

const fn rust_ownership_name(value: crate::RustOwnership) -> &'static str {
    match value {
        crate::RustOwnership::Value => "value",
        crate::RustOwnership::SharedBorrow => "shared-borrow",
        crate::RustOwnership::MutableBorrow => "mutable-borrow",
        crate::RustOwnership::Moved => "moved",
    }
}

const fn python_parameter_kind_name(value: PythonParameterKind) -> &'static str {
    match value {
        PythonParameterKind::PositionalOnly => "positional-only",
        PythonParameterKind::PositionalOrKeyword => "positional-or-keyword",
        PythonParameterKind::VariadicPositional => "variadic-positional",
        PythonParameterKind::KeywordOnly => "keyword-only",
        PythonParameterKind::VariadicKeyword => "variadic-keyword",
    }
}

const fn confidence_name(value: Confidence) -> &'static str {
    match value {
        Confidence::Syntactic => "syntactic",
        Confidence::Heuristic => "heuristic",
        Confidence::Indexed => "indexed",
        Confidence::Imported => "imported",
        Confidence::Compiler => "compiler",
    }
}

const fn clang_storage_name(value: ClangStorageClass) -> &'static str {
    match value {
        ClangStorageClass::None => "none",
        ClangStorageClass::Auto => "auto",
        ClangStorageClass::Static => "static",
        ClangStorageClass::Extern => "extern",
        ClangStorageClass::Register => "register",
        ClangStorageClass::ThreadLocal => "thread-local",
    }
}

const fn variance_name(value: crate::Variance) -> &'static str {
    match value {
        crate::Variance::Invariant => "invariant",
        crate::Variance::Covariant => "covariant",
        crate::Variance::Contravariant => "contravariant",
        crate::Variance::Bivariant => "bivariant",
    }
}

const fn type_parameter_inference_name(value: TypeParameterInference) -> &'static str {
    match value {
        TypeParameterInference::Ordinary => "ordinary",
        TypeParameterInference::Const => "const",
    }
}

fn emit_parentage(
    entity: EntityId,
    output: &mut impl fmt::Write,
    parentage: ParentageAuthority,
) -> Result<(), SemanticDocumentError> {
    match parentage {
        ParentageAuthority::Unavailable => write_text(entity, output, "unavailable"),
        ParentageAuthority::Root => write_text(entity, output, "root"),
        ParentageAuthority::Bound(identity) => {
            write_text(entity, output, "bound(")?;
            emit_identity(entity, output, identity)?;
            write_text(entity, output, ")")
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            write_text(entity, output, "unrepresented(")?;
            write_bytes(entity, output, &owner.as_bytes())?;
            write_text(entity, output, ")")
        }
    }
}

fn emit_source<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    claimed: FactAvailability,
    source: Option<SourceSpan>,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    validate_availability(
        entity,
        SemanticDocumentFact::Source,
        claimed,
        source.is_some(),
    )?;
    match source {
        None => write_text(entity, output, "unavailable"),
        Some(source) => {
            write_text(entity, output, "span(file=")?;
            emit_atom(reader, entity, source.file(), output)?;
            write_text(entity, output, ",start=")?;
            write_number(entity, output, u64::from(source.start()))?;
            write_text(entity, output, ",end=")?;
            write_number(entity, output, u64::from(source.end()))?;
            write_text(entity, output, ")")
        }
    }
}

fn emit_semantic_type<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    claimed: FactAvailability,
    semantic_type: Option<TypeId>,
    limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    validate_availability(
        entity,
        SemanticDocumentFact::SemanticType,
        claimed,
        semantic_type.is_some(),
    )?;
    match semantic_type {
        None => write_text(entity, output, "unavailable"),
        Some(semantic_type) => emit_canonical_type(reader, entity, semantic_type, limits, output),
    }
}

fn emit_documentation<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    claimed: FactAvailability,
    documentation: DocId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match claimed {
        FactAvailability::Unavailable => write_text(entity, output, "unavailable"),
        FactAvailability::Captured => {
            let docs =
                reader
                    .docs(documentation)
                    .ok_or(SemanticDocumentError::MissingReference {
                        entity,
                        reference: SemanticDocumentReference::Documentation(documentation),
                    })?;
            write_text(entity, output, "[")?;
            for (index, fragment) in docs.enumerate() {
                if index != 0 {
                    write_text(entity, output, ",")?;
                }
                emit_doc_fragment(reader, entity, fragment, output)?;
            }
            write_text(entity, output, "]")
        }
    }
}

fn emit_doc_fragment<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    fragment: DocFragment,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match fragment {
        DocFragment::Text(text) => {
            write_text(owner, output, "text(")?;
            emit_text(reader, owner, text, output)?;
            write_text(owner, output, ")")
        }
        DocFragment::Code(text) => {
            write_text(owner, output, "code(")?;
            emit_text(reader, owner, text, output)?;
            write_text(owner, output, ")")
        }
        DocFragment::Link { label, target } => {
            write_text(owner, output, "link(label=")?;
            emit_text(reader, owner, label, output)?;
            write_text(owner, output, ",target=")?;
            match target {
                crate::LinkTarget::Local(target) => {
                    let target_row =
                        reader
                            .entity(target)
                            .ok_or(SemanticDocumentError::MissingReference {
                                entity: owner,
                                reference: SemanticDocumentReference::Entity(target),
                            })?;
                    write_text(owner, output, "local(")?;
                    emit_identity(owner, output, target_row.version.identity())?;
                    write_text(owner, output, ",name=")?;
                    emit_atom(reader, owner, target_row.name, output)?;
                    write_text(owner, output, ")")?;
                }
                crate::LinkTarget::External(target) => {
                    let target =
                        reader
                            .external(target)
                            .ok_or(SemanticDocumentError::MissingReference {
                                entity: owner,
                                reference: SemanticDocumentReference::External(target),
                            })?;
                    emit_external(reader, owner, target, output)?;
                }
            }
            write_text(owner, output, ")")
        }
        DocFragment::SoftBreak => write_text(owner, output, "soft-break"),
        DocFragment::HardBreak => write_text(owner, output, "hard-break"),
    }
}

fn emit_external<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    target: ExternalTarget,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(owner, output, "external(")?;
    match target {
        ExternalTarget::Stable { target } => {
            write_text(owner, output, "stable(fragment=")?;
            write_bytes(owner, output, target.fragment.as_ref())?;
            write_text(owner, output, ",")?;
            emit_identity(owner, output, target.declaration)?;
        }
        ExternalTarget::Foreign(target) => {
            write_text(owner, output, "foreign(identity=")?;
            write_bytes(owner, output, target.identity.foreign.as_bytes())?;
            write_text(owner, output, ",variant=")?;
            match target.identity.variant {
                crate::VariantAvailability::Known(variant) => {
                    write_bytes(owner, output, variant.as_bytes())?
                }
                crate::VariantAvailability::Unavailable => {
                    write_text(owner, output, "unavailable")?
                }
            }
            write_text(owner, output, ",origin=")?;
            emit_foreign_origin(reader, owner, target.origin, output)?;
            write_text(owner, output, ",path=")?;
            emit_atom(reader, owner, target.path, output)?;
            write_text(owner, output, ",display=")?;
            emit_atom(reader, owner, target.display, output)?;
            write_text(owner, output, ",kind=")?;
            match target.kind {
                Some(kind) => write_text(owner, output, kind_name(kind))?,
                None => write_text(owner, output, "unavailable")?,
            }
        }
        ExternalTarget::FragmentEntity { target, display } => {
            write_text(owner, output, "fragment-entity(fragment=")?;
            write_bytes(owner, output, target.fragment.as_ref())?;
            write_text(owner, output, ",ordinal=")?;
            write_number(owner, output, u64::from(target.ordinal))?;
            write_text(owner, output, ",display=")?;
            emit_atom(reader, owner, display, output)?;
        }
    }
    write_text(owner, output, ")")
}

fn emit_foreign_origin<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    origin: ForeignTargetOrigin,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match origin {
        ForeignTargetOrigin::Package { ecosystem, package } => {
            write_text(owner, output, "package(")?;
            emit_atom(reader, owner, ecosystem, output)?;
            write_text(owner, output, ",")?;
            emit_atom(reader, owner, package, output)?;
            write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Namespace {
            ecosystem,
            namespace,
        } => {
            write_text(owner, output, "namespace(")?;
            emit_atom(reader, owner, ecosystem, output)?;
            write_text(owner, output, ",")?;
            emit_atom(reader, owner, namespace, output)?;
            write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Universe { ecosystem } => {
            write_text(owner, output, "universe(")?;
            emit_atom(reader, owner, ecosystem, output)?;
            write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Unspecified { ecosystem } => {
            write_text(owner, output, "unspecified(")?;
            emit_atom(reader, owner, ecosystem, output)?;
            write_text(owner, output, ")")
        }
    }
}

fn emit_identity(
    owner: EntityId,
    output: &mut impl fmt::Write,
    identity: DeclarationIdentity,
) -> Result<(), SemanticDocumentError> {
    write_text(owner, output, "identity(family=")?;
    write_bytes(owner, output, identity.family.as_bytes())?;
    write_text(owner, output, ",variant=")?;
    write_bytes(owner, output, identity.variant.as_bytes())?;
    write_text(owner, output, ")")
}

fn emit_atom<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    atom: AtomId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let bytes = reader
        .atom(atom)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Atom(atom),
        })?;
    write_bytes(owner, output, bytes)
}

fn emit_text<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    text: crate::TextId,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let text = reader
        .text(text)
        .ok_or(SemanticDocumentError::MissingReference {
            entity: owner,
            reference: SemanticDocumentReference::Text(text),
        })?;
    write_bytes(owner, output, text.as_bytes())
}

fn write_bytes(
    entity: EntityId,
    output: &mut impl fmt::Write,
    bytes: &[u8],
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "x\"")?;
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in bytes {
        let high = char::from(HEX[usize::from(*byte >> 4)]);
        let low = char::from(HEX[usize::from(*byte & 0x0F)]);
        output
            .write_char(high)
            .and_then(|()| output.write_char(low))
            .map_err(|_| SemanticDocumentError::OutputWrite { entity })?;
    }
    write_text(entity, output, "\"")
}

fn write_number(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: u64,
) -> Result<(), SemanticDocumentError> {
    fmt::write(output, format_args!("{value}"))
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

fn write_signed(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: i64,
) -> Result<(), SemanticDocumentError> {
    fmt::write(output, format_args!("{value}"))
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

fn write_bool(
    entity: EntityId,
    output: &mut impl fmt::Write,
    value: bool,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, if value { "true" } else { "false" })
}

fn write_text(
    entity: EntityId,
    output: &mut impl fmt::Write,
    text: &str,
) -> Result<(), SemanticDocumentError> {
    output
        .write_str(text)
        .map_err(|_| SemanticDocumentError::OutputWrite { entity })
}

#[derive(Default)]
struct CountWriter {
    length: Option<usize>,
}

impl CountWriter {
    const fn new() -> Self {
        Self { length: Some(0) }
    }
}

impl fmt::Write for CountWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.length = self
            .length
            .and_then(|length| length.checked_add(value.len()));
        Ok(())
    }
}

struct ByteWriter<'output> {
    output: &'output mut [u8],
    written: usize,
}

impl<'output> ByteWriter<'output> {
    const fn new(output: &'output mut [u8]) -> Self {
        Self { output, written: 0 }
    }
}

impl fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.written.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.output.get_mut(self.written..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.written = end;
        Ok(())
    }
}
