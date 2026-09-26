//! Canonical semantic-document grammar.
//!
//! Admission has already selected one dialect. This module writes the
//! language-neutral frame: identity, parentage, source, semantic type, and
//! documentation. The selected extension plane is appended as its own clause.

use core::fmt;

use crate::ir::{
    CanonicalTypeRenderLimits, DocFragment, DocId, EntityId, ExternalTarget, FactAvailability,
    ForeignTargetOrigin, LanguageProfile, ParentageAuthority, SemanticReader, SourceSpan, TypeId,
};

use super::super::{kind_name, visibility_name};
use super::{
    SemanticDocumentError, SemanticDocumentFact, SemanticDocumentLanguage,
    SemanticDocumentReference,
};

pub(super) fn emit_document<Reader: SemanticReader + ?Sized>(
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
    super::validate_profile(reader.image_facts().authority, profile, entity)?;
    super::codec::write_text(entity, output, "semantic-document(profile=")?;
    super::codec::write_bytes(entity, output, &<[u8; 2]>::from(profile))?;
    super::codec::write_text(entity, output, ",dialect=")?;
    super::codec::write_text(entity, output, language.name())?;
    super::codec::write_text(entity, output, ",entity=")?;
    super::codec::emit_identity(entity, output, row.version.identity())?;
    super::codec::write_text(entity, output, ",kind=")?;
    super::codec::write_text(entity, output, kind_name(row.kind))?;
    super::codec::write_text(entity, output, ",visibility=")?;
    super::codec::write_text(entity, output, visibility_name(row.visibility))?;
    super::codec::write_text(entity, output, ",name=")?;
    super::codec::emit_atom(reader, entity, row.name, output)?;
    super::codec::write_text(entity, output, ",parentage=")?;
    emit_parentage(entity, output, row.authority.parentage)?;
    super::codec::write_text(entity, output, ",extension=")?;
    let present = extension_present(reader, language, entity);
    super::validate_availability(
        entity,
        SemanticDocumentFact::LanguageExtension,
        row.authority.language_extension,
        present,
    )?;
    match row.authority.language_extension {
        FactAvailability::Captured => {
            super::codec::write_text(entity, output, language.name())?;
            super::codec::write_text(entity, output, "(captured)")?;
        }
        FactAvailability::Unavailable => super::codec::write_text(entity, output, "unavailable")?,
    }
    super::codec::write_text(entity, output, ",source=")?;
    emit_source(reader, entity, row.authority.source, row.source, output)?;
    super::codec::write_text(entity, output, ",semantic-type=")?;
    emit_semantic_type(
        reader,
        entity,
        row.authority.semantic_type,
        row.semantic_type,
        type_limits,
        output,
    )?;
    super::codec::write_text(entity, output, ",documentation=")?;
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
    super::codec::write_text(entity, output, ",extension-facts=")?;
    if row.authority.language_extension == FactAvailability::Captured {
        super::extension::emit_extension_facts(reader, entity, language, type_limits, output)?;
    } else {
        super::codec::write_text(entity, output, "unavailable")?;
    }
    super::codec::write_text(entity, output, ")")
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

fn emit_parentage(
    entity: EntityId,
    output: &mut impl fmt::Write,
    parentage: ParentageAuthority,
) -> Result<(), SemanticDocumentError> {
    match parentage {
        ParentageAuthority::Unavailable => super::codec::write_text(entity, output, "unavailable"),
        ParentageAuthority::Root => super::codec::write_text(entity, output, "root"),
        ParentageAuthority::Bound(identity) => {
            super::codec::write_text(entity, output, "bound(")?;
            super::codec::emit_identity(entity, output, identity)?;
            super::codec::write_text(entity, output, ")")
        }
        ParentageAuthority::UnrepresentedAuthorityOwner(owner) => {
            super::codec::write_text(entity, output, "unrepresented(")?;
            super::codec::write_bytes(entity, output, &owner.as_bytes())?;
            super::codec::write_text(entity, output, ")")
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
    super::validate_availability(
        entity,
        SemanticDocumentFact::Source,
        claimed,
        source.is_some(),
    )?;
    match source {
        None => super::codec::write_text(entity, output, "unavailable"),
        Some(source) => {
            super::codec::write_text(entity, output, "span(file=")?;
            super::codec::emit_atom(reader, entity, source.file(), output)?;
            super::codec::write_text(entity, output, ",start=")?;
            super::codec::write_number(entity, output, u64::from(source.start()))?;
            super::codec::write_text(entity, output, ",end=")?;
            super::codec::write_number(entity, output, u64::from(source.end()))?;
            super::codec::write_text(entity, output, ")")
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
    super::validate_availability(
        entity,
        SemanticDocumentFact::SemanticType,
        claimed,
        semantic_type.is_some(),
    )?;
    match semantic_type {
        None => super::codec::write_text(entity, output, "unavailable"),
        Some(semantic_type) => {
            super::codec::emit_canonical_type(reader, entity, semantic_type, limits, output)
        }
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
        FactAvailability::Unavailable => super::codec::write_text(entity, output, "unavailable"),
        FactAvailability::Captured => {
            let docs =
                reader
                    .docs(documentation)
                    .ok_or(SemanticDocumentError::MissingReference {
                        entity,
                        reference: SemanticDocumentReference::Documentation(documentation),
                    })?;
            super::codec::write_text(entity, output, "[")?;
            for (index, fragment) in docs.enumerate() {
                if index != 0 {
                    super::codec::write_text(entity, output, ",")?;
                }
                emit_doc_fragment(reader, entity, fragment, output)?;
            }
            super::codec::write_text(entity, output, "]")
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
            super::codec::write_text(owner, output, "text(")?;
            super::codec::emit_text(reader, owner, text, output)?;
            super::codec::write_text(owner, output, ")")
        }
        DocFragment::Code(text) => {
            super::codec::write_text(owner, output, "code(")?;
            super::codec::emit_text(reader, owner, text, output)?;
            super::codec::write_text(owner, output, ")")
        }
        DocFragment::Link { label, target } => {
            super::codec::write_text(owner, output, "link(label=")?;
            super::codec::emit_text(reader, owner, label, output)?;
            super::codec::write_text(owner, output, ",target=")?;
            match target {
                crate::ir::LinkTarget::Local(target) => {
                    let target_row =
                        reader
                            .entity(target)
                            .ok_or(SemanticDocumentError::MissingReference {
                                entity: owner,
                                reference: SemanticDocumentReference::Entity(target),
                            })?;
                    super::codec::write_text(owner, output, "local(")?;
                    super::codec::emit_identity(owner, output, target_row.version.identity())?;
                    super::codec::write_text(owner, output, ",name=")?;
                    super::codec::emit_atom(reader, owner, target_row.name, output)?;
                    super::codec::write_text(owner, output, ")")?;
                }
                crate::ir::LinkTarget::External(target) => {
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
            super::codec::write_text(owner, output, ")")
        }
        DocFragment::SoftBreak => super::codec::write_text(owner, output, "soft-break"),
        DocFragment::HardBreak => super::codec::write_text(owner, output, "hard-break"),
    }
}

fn emit_external<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    target: ExternalTarget,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    super::codec::write_text(owner, output, "external(")?;
    match target {
        ExternalTarget::Stable { target } => {
            super::codec::write_text(owner, output, "stable(fragment=")?;
            super::codec::write_bytes(owner, output, target.fragment.as_ref())?;
            super::codec::write_text(owner, output, ",")?;
            super::codec::emit_identity(owner, output, target.declaration)?;
        }
        ExternalTarget::Foreign(target) => {
            super::codec::write_text(owner, output, "foreign(identity=")?;
            super::codec::write_bytes(owner, output, target.identity.foreign.as_bytes())?;
            super::codec::write_text(owner, output, ",variant=")?;
            match target.identity.variant {
                crate::ir::VariantAvailability::Known(variant) => {
                    super::codec::write_bytes(owner, output, variant.as_bytes())?
                }
                crate::ir::VariantAvailability::Unavailable => {
                    super::codec::write_text(owner, output, "unavailable")?
                }
            }
            super::codec::write_text(owner, output, ",origin=")?;
            emit_foreign_origin(reader, owner, target.origin, output)?;
            super::codec::write_text(owner, output, ",path=")?;
            super::codec::emit_atom(reader, owner, target.path, output)?;
            super::codec::write_text(owner, output, ",display=")?;
            super::codec::emit_atom(reader, owner, target.display, output)?;
            super::codec::write_text(owner, output, ",kind=")?;
            match target.kind {
                Some(kind) => super::codec::write_text(owner, output, kind_name(kind))?,
                None => super::codec::write_text(owner, output, "unavailable")?,
            }
        }
        ExternalTarget::FragmentEntity { target, display } => {
            super::codec::write_text(owner, output, "fragment-entity(fragment=")?;
            super::codec::write_bytes(owner, output, target.fragment.as_ref())?;
            super::codec::write_text(owner, output, ",ordinal=")?;
            super::codec::write_number(owner, output, u64::from(target.ordinal))?;
            super::codec::write_text(owner, output, ",display=")?;
            super::codec::emit_atom(reader, owner, display, output)?;
        }
    }
    super::codec::write_text(owner, output, ")")
}

fn emit_foreign_origin<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    owner: EntityId,
    origin: ForeignTargetOrigin,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    match origin {
        ForeignTargetOrigin::Package { ecosystem, package } => {
            super::codec::write_text(owner, output, "package(")?;
            super::codec::emit_atom(reader, owner, ecosystem, output)?;
            super::codec::write_text(owner, output, ",")?;
            super::codec::emit_atom(reader, owner, package, output)?;
            super::codec::write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Namespace {
            ecosystem,
            namespace,
        } => {
            super::codec::write_text(owner, output, "namespace(")?;
            super::codec::emit_atom(reader, owner, ecosystem, output)?;
            super::codec::write_text(owner, output, ",")?;
            super::codec::emit_atom(reader, owner, namespace, output)?;
            super::codec::write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Universe { ecosystem } => {
            super::codec::write_text(owner, output, "universe(")?;
            super::codec::emit_atom(reader, owner, ecosystem, output)?;
            super::codec::write_text(owner, output, ")")
        }
        ForeignTargetOrigin::Unspecified { ecosystem } => {
            super::codec::write_text(owner, output, "unspecified(")?;
            super::codec::emit_atom(reader, owner, ecosystem, output)?;
            super::codec::write_text(owner, output, ")")
        }
    }
}
