//! Language-extension row wire grammar.
#![deny(
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    unsafe_code
)]

use alloc::vec::Vec;

use super::super::wire::{Cursor, encode_identity, put_bytes, put_u32, read_identity};
use super::super::{SemanticPlaneRecordError, TypedRecordPlan, TypesReferenceV2, TypesRowDomainV2};
use crate::ir::{
    CSharpFacts, ClangFacts, Confidence, DeclarationIdentity, EntityListId, GoFacts, JavaFacts,
    Language, LanguageProfile, PythonFacts, RustFacts, SemanticIrPlane, SemanticPlaneKind,
    SemanticReader, TypeScriptFacts,
};

const MAX_EXTENSION_DECLARATION_REFERENCES: usize = 1_000_000;
const EXTENSION_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.language-extension-row.v1\0";
pub(super) const TYPESCRIPT_TAG: u8 = 1;
pub(super) const CSHARP_TAG: u8 = 2;
pub(super) const GO_TAG: u8 = 3;
pub(super) const RUST_TAG: u8 = 4;
pub(super) const PYTHON_TAG: u8 = 5;
pub(super) const JAVA_TAG: u8 = 6;
pub(super) const CLANG_TAG: u8 = 7;

pub(super) fn extension_row_key(
    profile: LanguageProfile,
    identity: DeclarationIdentity,
    role: u8,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(EXTENSION_KEY_DOMAIN);
    hasher.update(&<[u8; 2]>::from(profile));
    hasher.update(identity.family.as_bytes());
    hasher.update(identity.variant.as_bytes());
    hasher.update(&[role]);
    *hasher.finalize().as_bytes()
}

pub(super) fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0; 32];
    for (target, source) in bytes.iter_mut().take(16).zip(identity.family.as_bytes()) {
        *target = *source;
    }
    for (target, source) in bytes.iter_mut().skip(16).zip(identity.variant.as_bytes()) {
        *target = *source;
    }
    bytes
}

pub(super) fn extension_tag(language: Language) -> u8 {
    match language {
        Language::TypeScript => TYPESCRIPT_TAG,
        Language::CSharp => CSHARP_TAG,
        Language::Go => GO_TAG,
        Language::Rust => RUST_TAG,
        Language::Python => PYTHON_TAG,
        Language::Java => JAVA_TAG,
        Language::Clang => CLANG_TAG,
    }
}

fn put_reference(out: &mut Vec<u8>, domain: TypesRowDomainV2, key: [u8; 32]) {
    out.push(domain_code(domain));
    out.extend_from_slice(&key);
}

fn put_optional_type_reference(
    out: &mut Vec<u8>,
    plan: &TypedRecordPlan,
    value: Option<crate::ir::TypeId>,
) -> Result<(), SemanticPlaneRecordError> {
    match value {
        None => out.push(0),
        Some(id) => {
            out.push(1);
            put_reference(out, TypesRowDomainV2::Type, plan.type_key(id)?);
        }
    }
    Ok(())
}

pub(super) fn encode_typescript(
    plan: &TypedRecordPlan,
    facts: TypeScriptFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.type_parameters)?,
    );
    put_optional_type_reference(out, plan, facts.declared)?;
    put_optional_type_reference(out, plan, facts.observed)?;
    Ok(())
}

pub(super) fn encode_csharp<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: CSharpFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(csharp_nullability_code(facts.nullability));
    out.push(csharp_reference_code(facts.reference_kind));
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.constraints)?,
    );
    out.push(bool_code(facts.effects.is_async));
    out.push(bool_code(facts.effects.is_iterator));
    out.push(bool_code(facts.effects.is_extension));
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.attributes)?,
    );
    out.push(csharp_partial_code(facts.partial));
    match facts.xml_provenance {
        None => out.push(0),
        Some(span) => {
            out.push(1);
            put_bytes(
                out,
                reader
                    .atom(span.file())
                    .ok_or(SemanticPlaneRecordError::ReaderReference)?,
            )?;
            out.extend_from_slice(&span.start().to_be_bytes());
            out.extend_from_slice(&span.end().to_be_bytes());
        }
    }
    Ok(())
}

pub(super) fn encode_go<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: GoFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.signature.parameters)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.signature.results)?,
    );
    out.push(bool_code(facts.signature.variadic));
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.type_parameters)?,
    );
    encode_entity_list(reader, facts.fields, out)?;
    encode_entity_list(reader, facts.method_set, out)?;
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.build_constraints)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.constant_value)?,
    );
    out.extend_from_slice(&facts.constant_group.to_be_bytes());
    out.extend_from_slice(&facts.constant_flags.to_be_bytes());
    Ok(())
}

pub(super) fn encode_rust(
    plan: &TypedRecordPlan,
    facts: RustFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(rust_ownership_code(facts.ownership));
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.lifetimes)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.where_clauses)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.macros)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.const_defaults)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::FreePredicates,
        plan.free_predicates_key(facts.free_predicates)?,
    );
    Ok(())
}

pub(super) fn encode_python(
    plan: &TypedRecordPlan,
    facts: PythonFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.decorators)?,
    );
    out.push(python_parameter_code(facts.parameter_kind));
    out.push(confidence_code(facts.dynamic_confidence));
    Ok(())
}

pub(super) fn encode_java<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    plan: &TypedRecordPlan,
    facts: JavaFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    put_reference(
        out,
        TypesRowDomainV2::TypeList,
        plan.type_list_key(facts.throws)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.annotations)?,
    );
    encode_entity_list(reader, facts.overloads, out)?;
    encode_entity_list(reader, facts.record_components, out)?;
    Ok(())
}

pub(super) fn encode_clang(
    plan: &TypedRecordPlan,
    facts: ClangFacts,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    out.push(bool_code(facts.qualifiers.is_const));
    out.push(bool_code(facts.qualifiers.is_volatile));
    out.push(bool_code(facts.qualifiers.is_restrict));
    out.push(clang_storage_code(facts.storage));
    put_optional_u32(out, facts.layout.size_bits);
    put_optional_u32(out, facts.layout.align_bits);
    put_reference(
        out,
        TypesRowDomainV2::TypeParameters,
        plan.type_parameters_key(facts.templates)?,
    );
    put_reference(
        out,
        TypesRowDomainV2::AtomList,
        plan.atom_list_key(facts.includes)?,
    );
    Ok(())
}

fn encode_entity_list<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    list: EntityListId,
    out: &mut Vec<u8>,
) -> Result<(), SemanticPlaneRecordError> {
    let members = reader
        .entity_list(list)
        .ok_or(SemanticPlaneRecordError::ReaderReference)?;
    put_u32(out, members.len())?;
    for member in members {
        let entity = reader
            .entity(member)
            .ok_or(SemanticPlaneRecordError::ReaderReference)?;
        encode_identity(entity.version.identity(), out);
    }
    Ok(())
}

fn put_optional_u32(out: &mut Vec<u8>, value: Option<u32>) {
    match value {
        None => out.push(0),
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn bool_code(value: bool) -> u8 {
    if value { 1 } else { 0 }
}

fn csharp_nullability_code(value: crate::ir::CSharpNullability) -> u8 {
    match value {
        crate::ir::CSharpNullability::Oblivious => 0,
        crate::ir::CSharpNullability::NonNullable => 1,
        crate::ir::CSharpNullability::Nullable => 2,
    }
}

fn csharp_reference_code(value: crate::ir::CSharpReferenceKind) -> u8 {
    match value {
        crate::ir::CSharpReferenceKind::Value => 0,
        crate::ir::CSharpReferenceKind::In => 1,
        crate::ir::CSharpReferenceKind::Ref => 2,
        crate::ir::CSharpReferenceKind::Out => 3,
    }
}

fn csharp_partial_code(value: crate::ir::CSharpPartialRole) -> u8 {
    match value {
        crate::ir::CSharpPartialRole::None => 0,
        crate::ir::CSharpPartialRole::Definition => 1,
        crate::ir::CSharpPartialRole::Implementation => 2,
    }
}

fn rust_ownership_code(value: crate::ir::RustOwnership) -> u8 {
    match value {
        crate::ir::RustOwnership::Value => 0,
        crate::ir::RustOwnership::SharedBorrow => 1,
        crate::ir::RustOwnership::MutableBorrow => 2,
        crate::ir::RustOwnership::Moved => 3,
    }
}

fn python_parameter_code(value: crate::ir::PythonParameterKind) -> u8 {
    match value {
        crate::ir::PythonParameterKind::PositionalOnly => 0,
        crate::ir::PythonParameterKind::PositionalOrKeyword => 1,
        crate::ir::PythonParameterKind::VariadicPositional => 2,
        crate::ir::PythonParameterKind::KeywordOnly => 3,
        crate::ir::PythonParameterKind::VariadicKeyword => 4,
    }
}

fn confidence_code(value: Confidence) -> u8 {
    match value {
        Confidence::Syntactic => 0,
        Confidence::Heuristic => 1,
        Confidence::Indexed => 2,
        Confidence::Imported => 3,
        Confidence::Compiler => 4,
    }
}

fn clang_storage_code(value: crate::ir::ClangStorageClass) -> u8 {
    match value {
        crate::ir::ClangStorageClass::None => 0,
        crate::ir::ClangStorageClass::Auto => 1,
        crate::ir::ClangStorageClass::Static => 2,
        crate::ir::ClangStorageClass::Extern => 3,
        crate::ir::ClangStorageClass::Register => 4,
        crate::ir::ClangStorageClass::ThreadLocal => 5,
    }
}

pub(super) fn domain_code(domain: TypesRowDomainV2) -> u8 {
    match domain {
        TypesRowDomainV2::EntityRoot => 0,
        TypesRowDomainV2::TypedNode => 1,
        TypesRowDomainV2::Type => 2,
        TypesRowDomainV2::TypeList => 3,
        TypesRowDomainV2::TupleElements => 4,
        TypesRowDomainV2::ObjectMembers => 5,
        TypesRowDomainV2::TemplateParts => 6,
        TypesRowDomainV2::AtomList => 7,
        TypesRowDomainV2::TypeParameters => 8,
        TypesRowDomainV2::TypeParameterBounds => 9,
        TypesRowDomainV2::FreePredicates => 10,
        TypesRowDomainV2::Atom => 11,
        TypesRowDomainV2::ExternalTarget => 12,
    }
}

fn decode_domain(raw: u8) -> Result<TypesRowDomainV2, SemanticPlaneRecordError> {
    Ok(match raw {
        0 => TypesRowDomainV2::EntityRoot,
        1 => TypesRowDomainV2::TypedNode,
        2 => TypesRowDomainV2::Type,
        3 => TypesRowDomainV2::TypeList,
        4 => TypesRowDomainV2::TupleElements,
        5 => TypesRowDomainV2::ObjectMembers,
        6 => TypesRowDomainV2::TemplateParts,
        7 => TypesRowDomainV2::AtomList,
        8 => TypesRowDomainV2::TypeParameters,
        9 => TypesRowDomainV2::TypeParameterBounds,
        10 => TypesRowDomainV2::FreePredicates,
        11 => TypesRowDomainV2::Atom,
        12 => TypesRowDomainV2::ExternalTarget,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    })
}

#[derive(Clone, Copy)]
pub(super) struct ParsedExtensionRecord {
    pub(super) identity: DeclarationIdentity,
    pub(super) references: [Option<TypesReferenceV2>; 5],
}

fn parse_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    parse_record_inner(kind, key, tag, payload, &mut None, usize::MAX)
}

pub(super) fn parse_record_with_declarations(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    declaration_references: &mut Vec<[u8; 32]>,
    remaining_reference_budget: u64,
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    let remaining_reference_budget = usize::try_from(remaining_reference_budget)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let maximum_declaration_references = declaration_references
        .len()
        .checked_add(remaining_reference_budget)
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    parse_record_inner(
        kind,
        key,
        tag,
        payload,
        &mut Some(declaration_references),
        maximum_declaration_references,
    )
}

fn parse_record_inner(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
    declaration_references: &mut Option<&mut Vec<[u8; 32]>>,
    maximum_declaration_references: usize,
) -> Result<ParsedExtensionRecord, SemanticPlaneRecordError> {
    let SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile)) = kind else {
        return Err(SemanticPlaneRecordError::RowGrammar);
    };
    let language = match tag {
        TYPESCRIPT_TAG => Language::TypeScript,
        CSHARP_TAG => Language::CSharp,
        GO_TAG => Language::Go,
        RUST_TAG => Language::Rust,
        PYTHON_TAG => Language::Python,
        JAVA_TAG => Language::Java,
        CLANG_TAG => Language::Clang,
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    };
    if profile.language() != language {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut cursor = Cursor::new(payload);
    let identity = read_identity(&mut cursor)?;
    if cursor.u8()? != 1 {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let mut references = [None; 5];
    let mut next_reference = 0_usize;
    match tag {
        TYPESCRIPT_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_optional_reference(
                &mut cursor,
                TypesRowDomainV2::Type,
                &mut references,
                &mut next_reference,
            )?;
            read_optional_reference(
                &mut cursor,
                TypesRowDomainV2::Type,
                &mut references,
                &mut next_reference,
            )?;
        }
        CSHARP_TAG => {
            if cursor.u8()? > 2 || cursor.u8()? > 3 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            for _ in 0..3 {
                read_bool(&mut cursor)?;
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            if cursor.u8()? > 2 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            match cursor.u8()? {
                0 => {}
                1 => {
                    let _raw_path = cursor.bytes32()?;
                    let start = cursor.u32()?;
                    let end = cursor.u32()?;
                    if start > end {
                        return Err(SemanticPlaneRecordError::RowGrammar);
                    }
                }
                _ => return Err(SemanticPlaneRecordError::RowGrammar),
            }
        }
        GO_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_bool(&mut cursor)?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_entity_list(
                &mut cursor,
                declaration_references,
                maximum_declaration_references,
            )?;
            read_entity_list(
                &mut cursor,
                declaration_references,
                maximum_declaration_references,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            let _group = cursor.take(8)?;
            let _flags = cursor.u32()?;
        }
        RUST_TAG => {
            if cursor.u8()? > 3 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::FreePredicates,
                &mut references,
                &mut next_reference,
            )?;
        }
        PYTHON_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            if cursor.u8()? > 4 || cursor.u8()? > 4 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
        }
        JAVA_TAG => {
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeList,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
            read_entity_list(
                &mut cursor,
                declaration_references,
                maximum_declaration_references,
            )?;
            read_entity_list(
                &mut cursor,
                declaration_references,
                maximum_declaration_references,
            )?;
        }
        CLANG_TAG => {
            for _ in 0..3 {
                read_bool(&mut cursor)?;
            }
            if cursor.u8()? > 5 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            read_optional_u32(&mut cursor, false)?;
            read_optional_u32(&mut cursor, true)?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::TypeParameters,
                &mut references,
                &mut next_reference,
            )?;
            read_reference(
                &mut cursor,
                TypesRowDomainV2::AtomList,
                &mut references,
                &mut next_reference,
            )?;
        }
        _ => return Err(SemanticPlaneRecordError::RowGrammar),
    }
    if !cursor.is_empty() {
        return Err(SemanticPlaneRecordError::RowTrailingBytes);
    }
    if extension_row_key(profile, identity, tag) != key {
        return Err(SemanticPlaneRecordError::StableKeyMismatch);
    }
    Ok(ParsedExtensionRecord {
        identity,
        references,
    })
}

fn read_reference(
    cursor: &mut Cursor<'_>,
    expected: TypesRowDomainV2,
    references: &mut [Option<TypesReferenceV2>; 5],
    next: &mut usize,
) -> Result<(), SemanticPlaneRecordError> {
    let domain = decode_domain(cursor.u8()?)?;
    if domain != expected {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    let key = cursor
        .take(32)?
        .try_into()
        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
    let slot = references
        .get_mut(*next)
        .ok_or(SemanticPlaneRecordError::RowGrammar)?;
    *slot = Some(TypesReferenceV2 { domain, key });
    *next = next
        .checked_add(1)
        .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
    Ok(())
}

fn read_optional_reference(
    cursor: &mut Cursor<'_>,
    expected: TypesRowDomainV2,
    references: &mut [Option<TypesReferenceV2>; 5],
    next: &mut usize,
) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 => Ok(()),
        1 => read_reference(cursor, expected, references, next),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_bool(cursor: &mut Cursor<'_>) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 | 1 => Ok(()),
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_optional_u32(
    cursor: &mut Cursor<'_>,
    reject_zero: bool,
) -> Result<(), SemanticPlaneRecordError> {
    match cursor.u8()? {
        0 => Ok(()),
        1 => {
            let value = cursor.u32()?;
            if reject_zero && value == 0 {
                return Err(SemanticPlaneRecordError::RowGrammar);
            }
            Ok(())
        }
        _ => Err(SemanticPlaneRecordError::RowGrammar),
    }
}

fn read_entity_list(
    cursor: &mut Cursor<'_>,
    declaration_references: &mut Option<&mut Vec<[u8; 32]>>,
    maximum_declaration_references: usize,
) -> Result<(), SemanticPlaneRecordError> {
    let count = cursor.u32()?;
    if let Some(references) = declaration_references.as_deref_mut() {
        let count = usize::try_from(count).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        let total = references
            .len()
            .checked_add(count)
            .ok_or(SemanticPlaneRecordError::RowTooLarge)?;
        if total > MAX_EXTENSION_DECLARATION_REFERENCES.min(maximum_declaration_references) {
            return Err(SemanticPlaneRecordError::RowTooLarge);
        }
        references
            .try_reserve(count)
            .map_err(SemanticPlaneRecordError::Allocation)?;
    }
    for _ in 0..count {
        let identity = read_identity(cursor)?;
        if let Some(references) = declaration_references.as_deref_mut() {
            references.push(identity_bytes(identity));
        }
    }
    Ok(())
}

pub(super) fn validate_record(
    kind: SemanticPlaneKind,
    key: [u8; 32],
    tag: u8,
    payload: &[u8],
) -> Result<(), SemanticPlaneRecordError> {
    parse_record(kind, key, tag, payload).map(|_| ())
}
