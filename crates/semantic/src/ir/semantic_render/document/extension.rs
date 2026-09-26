//! Selected language-extension plane of a semantic document.
//!
//! Each dialect renders one named sparse fact plane. The document frame
//! stays language-neutral and only asks this module for the captured plane.

use core::fmt;

use crate::ir::{
    CSharpFacts, CSharpNullability, CSharpPartialRole, CSharpReferenceKind,
    CanonicalTypeRenderLimits, ClangFacts, ClangStorageClass, Confidence, EntityId,
    FactAvailability, FreePredicate, FreePredicateListId, GoFacts, JavaFacts, PythonFacts,
    PythonParameterKind, RustFacts, SemanticReader,
};

use super::codec::{
    emit_atom_list, emit_entity_list, emit_optional_source_span, emit_optional_type,
    emit_optional_u32, emit_type_coordinate, emit_type_list, emit_type_parameter_bound_list,
    emit_type_parameter_list, write_bool, write_number, write_signed, write_text,
};
use super::{
    SemanticDocumentError, SemanticDocumentFact, SemanticDocumentLanguage,
    SemanticDocumentReference,
};

pub(super) fn emit_extension_facts<Reader: SemanticReader + ?Sized>(
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
    facts: crate::ir::TypeScriptFacts,
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
    write_text(entity, output, ",const-defaults=")?;
    emit_atom_list(reader, entity, facts.const_defaults, output)?;
    write_text(entity, output, ",free-predicates=")?;
    emit_free_predicate_list(reader, entity, facts.free_predicates, type_limits, output)?;
    write_text(entity, output, ")")
}

fn emit_free_predicate_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    list: FreePredicateListId,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    let predicates =
        reader
            .free_predicates(list)
            .ok_or(SemanticDocumentError::MissingReference {
                entity,
                reference: SemanticDocumentReference::FreePredicateList(list),
            })?;
    write_text(entity, output, "free-predicate-list(values=[")?;
    for (index, predicate) in predicates.enumerate() {
        if index != 0 {
            write_text(entity, output, ",")?;
        }
        emit_free_predicate(reader, entity, predicate, type_limits, output)?;
    }
    write_text(entity, output, "])")
}

fn emit_free_predicate<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: EntityId,
    predicate: FreePredicate,
    type_limits: CanonicalTypeRenderLimits,
    output: &mut impl fmt::Write,
) -> Result<(), SemanticDocumentError> {
    write_text(entity, output, "free-predicate(subject=")?;
    emit_type_coordinate(reader, entity, predicate.subject, type_limits, output)?;
    write_text(entity, output, ",bounds=")?;
    emit_type_parameter_bound_list(reader, entity, predicate.bounds, type_limits, output)?;
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

const fn rust_ownership_name(value: crate::ir::RustOwnership) -> &'static str {
    match value {
        crate::ir::RustOwnership::Value => "value",
        crate::ir::RustOwnership::SharedBorrow => "shared-borrow",
        crate::ir::RustOwnership::MutableBorrow => "mutable-borrow",
        crate::ir::RustOwnership::Moved => "moved",
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
