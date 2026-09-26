//! Per-language extension fact words.
//!
//! The section frame admits directories and plane lengths. Each language
//! writes and reads its own fact width here, and a non-canonical word is
//! rejected with the directory kind that owned it.

use super::{
    LanguageExtensionCommonBounds, LanguageExtensionDirectoryKind, LanguageExtensionEncodeError,
    LanguageExtensionReopenError, LanguageExtensionWireFact, NONE, get_u32, put_u32, read_word,
    wire_fact_sealed,
};

impl LanguageExtensionWireFact for crate::ir::TypeScriptFacts {
    const WIDTH: usize = 12;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let type_parameters = crate::ir::TypeParameterListId::new(read_word(bytes, offset)?);
        let declared = optional_type(read_word(bytes, offset + 4)?);
        let observed = optional_type(read_word(bytes, offset + 8)?);
        Some(Self {
            type_parameters,
            declared,
            observed,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::TypeScriptFacts {}

impl LanguageExtensionWireFact for crate::ir::CSharpFacts {
    const WIDTH: usize = 36;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let nullability = match read_word(bytes, offset)? {
            0 => crate::ir::CSharpNullability::Oblivious,
            1 => crate::ir::CSharpNullability::NonNullable,
            2 => crate::ir::CSharpNullability::Nullable,
            _ => return None,
        };
        let reference_kind = match read_word(bytes, offset + 4)? {
            0 => crate::ir::CSharpReferenceKind::Value,
            1 => crate::ir::CSharpReferenceKind::In,
            2 => crate::ir::CSharpReferenceKind::Ref,
            3 => crate::ir::CSharpReferenceKind::Out,
            _ => return None,
        };
        let effects = read_word(bytes, offset + 8)?;
        if effects & !7 != 0 {
            return None;
        }
        let partial = match read_word(bytes, offset + 12)? {
            0 => crate::ir::CSharpPartialRole::None,
            1 => crate::ir::CSharpPartialRole::Definition,
            2 => crate::ir::CSharpPartialRole::Implementation,
            _ => return None,
        };
        let file = read_word(bytes, offset + 24)?;
        let xml_provenance = if file == NONE {
            None
        } else {
            crate::ir::SourceSpan::new(
                crate::ir::AtomId::new(file),
                read_word(bytes, offset + 28)?,
                read_word(bytes, offset + 32)?,
            )
        };
        Some(Self {
            nullability,
            reference_kind,
            constraints: crate::ir::TypeParameterListId::new(read_word(bytes, offset + 16)?),
            effects: crate::ir::CSharpMemberEffects {
                is_async: effects & 1 != 0,
                is_iterator: effects & 2 != 0,
                is_extension: effects & 4 != 0,
            },
            attributes: crate::ir::AtomListId::new(read_word(bytes, offset + 20)?),
            partial,
            xml_provenance,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::CSharpFacts {}

impl LanguageExtensionWireFact for crate::ir::GoFacts {
    const WIDTH: usize = 44;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let constant_flags = read_word(bytes, offset + 40)?;
        if constant_flags & !1 != 0 {
            return None;
        }
        let constant_group = u64::from(read_word(bytes, offset + 36)?) << 32
            | u64::from(read_word(bytes, offset + 32)?);
        Some(Self {
            signature: crate::ir::GoSignature {
                parameters: crate::ir::TypeListId::new(read_word(bytes, offset)?),
                results: crate::ir::TypeListId::new(read_word(bytes, offset + 4)?),
                variadic: match read_word(bytes, offset + 8)? {
                    0 => false,
                    1 => true,
                    _ => return None,
                },
            },
            type_parameters: crate::ir::TypeParameterListId::new(read_word(bytes, offset + 12)?),
            fields: crate::ir::EntityListId::new(read_word(bytes, offset + 16)?),
            method_set: crate::ir::EntityListId::new(read_word(bytes, offset + 20)?),
            build_constraints: crate::ir::AtomListId::new(read_word(bytes, offset + 24)?),
            constant_value: crate::ir::AtomListId::new(read_word(bytes, offset + 28)?),
            constant_group: i64::from_le_bytes(constant_group.to_le_bytes()),
            constant_flags,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::GoFacts {}
impl LanguageExtensionWireFact for crate::ir::RustFacts {
    const WIDTH: usize = 24;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let ownership = match read_word(bytes, offset)? {
            0 => crate::ir::RustOwnership::Value,
            1 => crate::ir::RustOwnership::SharedBorrow,
            2 => crate::ir::RustOwnership::MutableBorrow,
            3 => crate::ir::RustOwnership::Moved,
            _ => return None,
        };
        Some(Self {
            ownership,
            lifetimes: crate::ir::AtomListId::new(read_word(bytes, offset + 4)?),
            where_clauses: crate::ir::TypeParameterListId::new(read_word(bytes, offset + 8)?),
            macros: crate::ir::AtomListId::new(read_word(bytes, offset + 12)?),
            const_defaults: crate::ir::AtomListId::new(read_word(bytes, offset + 16)?),
            free_predicates: crate::ir::FreePredicateListId::new(read_word(bytes, offset + 20)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::RustFacts {}
impl LanguageExtensionWireFact for crate::ir::PythonFacts {
    const WIDTH: usize = 12;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let parameter_kind = match read_word(bytes, offset + 4)? {
            0 => crate::ir::PythonParameterKind::PositionalOnly,
            1 => crate::ir::PythonParameterKind::PositionalOrKeyword,
            2 => crate::ir::PythonParameterKind::VariadicPositional,
            3 => crate::ir::PythonParameterKind::KeywordOnly,
            4 => crate::ir::PythonParameterKind::VariadicKeyword,
            _ => return None,
        };
        let dynamic_confidence = match read_word(bytes, offset + 8)? {
            0 => crate::ir::Confidence::Syntactic,
            1 => crate::ir::Confidence::Heuristic,
            2 => crate::ir::Confidence::Indexed,
            3 => crate::ir::Confidence::Imported,
            4 => crate::ir::Confidence::Compiler,
            _ => return None,
        };
        Some(Self {
            decorators: crate::ir::AtomListId::new(read_word(bytes, offset)?),
            parameter_kind,
            dynamic_confidence,
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::PythonFacts {}
impl LanguageExtensionWireFact for crate::ir::JavaFacts {
    const WIDTH: usize = 16;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        Some(Self {
            throws: crate::ir::TypeListId::new(read_word(bytes, offset)?),
            annotations: crate::ir::AtomListId::new(read_word(bytes, offset + 4)?),
            overloads: crate::ir::EntityListId::new(read_word(bytes, offset + 8)?),
            record_components: crate::ir::EntityListId::new(read_word(bytes, offset + 12)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::JavaFacts {}
impl LanguageExtensionWireFact for crate::ir::ClangFacts {
    const WIDTH: usize = 24;
    fn decode(bytes: &[u8], offset: usize) -> Option<Self> {
        let qualifiers = read_word(bytes, offset)?;
        if qualifiers & !7 != 0 {
            return None;
        }
        let storage = match read_word(bytes, offset + 4)? {
            0 => crate::ir::ClangStorageClass::None,
            1 => crate::ir::ClangStorageClass::Auto,
            2 => crate::ir::ClangStorageClass::Static,
            3 => crate::ir::ClangStorageClass::Extern,
            4 => crate::ir::ClangStorageClass::Register,
            5 => crate::ir::ClangStorageClass::ThreadLocal,
            _ => return None,
        };
        let align = read_word(bytes, offset + 12)?;
        if align == 0 {
            return None;
        }
        Some(Self {
            qualifiers: crate::ir::ClangQualifiers {
                is_const: qualifiers & 1 != 0,
                is_volatile: qualifiers & 2 != 0,
                is_restrict: qualifiers & 4 != 0,
            },
            storage,
            layout: crate::ir::ClangLayout {
                size_bits: optional_u32(read_word(bytes, offset + 8)?),
                align_bits: optional_u32(align),
            },
            templates: crate::ir::TypeParameterListId::new(read_word(bytes, offset + 16)?),
            includes: crate::ir::AtomListId::new(read_word(bytes, offset + 20)?),
        })
    }
}
impl wire_fact_sealed::Sealed for crate::ir::ClangFacts {}

fn optional_type(raw: u32) -> Option<crate::ir::TypeId> {
    (raw != NONE).then(|| crate::ir::TypeId::new(raw))
}
fn optional_u32(raw: u32) -> Option<u32> {
    (raw != NONE).then_some(raw)
}

fn word(
    output: &mut [u8],
    base: usize,
    index: usize,
    value: u32,
) -> Result<(), LanguageExtensionEncodeError> {
    let offset = index
        .checked_mul(4)
        .and_then(|width| base.checked_add(width))
        .ok_or(LanguageExtensionEncodeError::LengthOverflow)?;
    put_u32(output, offset, value)
}
fn option(id: Option<crate::ir::TypeId>) -> u32 {
    id.map_or(NONE, |id| id.raw)
}
pub(super) fn encode_typescript(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::TypeScriptFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.type_parameters.raw)?;
    word(output, base, 1, option(facts.declared))?;
    word(output, base, 2, facts.observed.map_or(NONE, |id| id.raw))
}
pub(super) fn encode_csharp(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::CSharpFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        match facts.nullability {
            crate::ir::CSharpNullability::Oblivious => 0,
            crate::ir::CSharpNullability::NonNullable => 1,
            crate::ir::CSharpNullability::Nullable => 2,
        },
    )?;
    word(
        output,
        base,
        1,
        match facts.reference_kind {
            crate::ir::CSharpReferenceKind::Value => 0,
            crate::ir::CSharpReferenceKind::In => 1,
            crate::ir::CSharpReferenceKind::Ref => 2,
            crate::ir::CSharpReferenceKind::Out => 3,
        },
    )?;
    word(
        output,
        base,
        2,
        u32::from(facts.effects.is_async)
            | u32::from(facts.effects.is_iterator) << 1
            | u32::from(facts.effects.is_extension) << 2,
    )?;
    word(
        output,
        base,
        3,
        match facts.partial {
            crate::ir::CSharpPartialRole::None => 0,
            crate::ir::CSharpPartialRole::Definition => 1,
            crate::ir::CSharpPartialRole::Implementation => 2,
        },
    )?;
    word(output, base, 4, facts.constraints.raw)?;
    word(output, base, 5, facts.attributes.raw)?;
    word(
        output,
        base,
        6,
        facts.xml_provenance.map_or(NONE, |span| span.file().raw),
    )?;
    word(
        output,
        base,
        7,
        facts.xml_provenance.map_or(0, crate::ir::SourceSpan::start),
    )?;
    word(
        output,
        base,
        8,
        facts.xml_provenance.map_or(0, crate::ir::SourceSpan::end),
    )
}
pub(super) fn encode_go(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::GoFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.signature.parameters.raw)?;
    word(output, base, 1, facts.signature.results.raw)?;
    word(output, base, 2, u32::from(facts.signature.variadic))?;
    word(output, base, 3, facts.type_parameters.raw)?;
    word(output, base, 4, facts.fields.raw)?;
    word(output, base, 5, facts.method_set.raw)?;
    word(output, base, 6, facts.build_constraints.raw)?;
    word(output, base, 7, facts.constant_value.raw)?;
    let group = facts.constant_group.to_le_bytes();
    word(
        output,
        base,
        8,
        u32::from_le_bytes([group[0], group[1], group[2], group[3]]),
    )?;
    word(
        output,
        base,
        9,
        u32::from_le_bytes([group[4], group[5], group[6], group[7]]),
    )?;
    word(output, base, 10, facts.constant_flags)
}
pub(super) fn encode_rust(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::RustFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        match facts.ownership {
            crate::ir::RustOwnership::Value => 0,
            crate::ir::RustOwnership::SharedBorrow => 1,
            crate::ir::RustOwnership::MutableBorrow => 2,
            crate::ir::RustOwnership::Moved => 3,
        },
    )?;
    word(output, base, 1, facts.lifetimes.raw)?;
    word(output, base, 2, facts.where_clauses.raw)?;
    word(output, base, 3, facts.macros.raw)?;
    word(output, base, 4, facts.const_defaults.raw)?;
    word(output, base, 5, facts.free_predicates.raw)
}
pub(super) fn encode_python(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::PythonFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.decorators.raw)?;
    word(
        output,
        base,
        1,
        match facts.parameter_kind {
            crate::ir::PythonParameterKind::PositionalOnly => 0,
            crate::ir::PythonParameterKind::PositionalOrKeyword => 1,
            crate::ir::PythonParameterKind::VariadicPositional => 2,
            crate::ir::PythonParameterKind::KeywordOnly => 3,
            crate::ir::PythonParameterKind::VariadicKeyword => 4,
        },
    )?;
    word(
        output,
        base,
        2,
        match facts.dynamic_confidence {
            crate::ir::Confidence::Syntactic => 0,
            crate::ir::Confidence::Heuristic => 1,
            crate::ir::Confidence::Indexed => 2,
            crate::ir::Confidence::Imported => 3,
            crate::ir::Confidence::Compiler => 4,
        },
    )
}
pub(super) fn encode_java(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::JavaFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(output, base, 0, facts.throws.raw)?;
    word(output, base, 1, facts.annotations.raw)?;
    word(output, base, 2, facts.overloads.raw)?;
    word(output, base, 3, facts.record_components.raw)
}
pub(super) fn encode_clang(
    output: &mut [u8],
    base: usize,
    facts: crate::ir::ClangFacts,
) -> Result<(), LanguageExtensionEncodeError> {
    word(
        output,
        base,
        0,
        u32::from(facts.qualifiers.is_const)
            | u32::from(facts.qualifiers.is_volatile) << 1
            | u32::from(facts.qualifiers.is_restrict) << 2,
    )?;
    word(
        output,
        base,
        1,
        match facts.storage {
            crate::ir::ClangStorageClass::None => 0,
            crate::ir::ClangStorageClass::Auto => 1,
            crate::ir::ClangStorageClass::Static => 2,
            crate::ir::ClangStorageClass::Extern => 3,
            crate::ir::ClangStorageClass::Register => 4,
            crate::ir::ClangStorageClass::ThreadLocal => 5,
        },
    )?;
    word(output, base, 2, facts.layout.size_bits.unwrap_or(NONE))?;
    word(output, base, 3, facts.layout.align_bits.unwrap_or(NONE))?;
    word(output, base, 4, facts.templates.raw)?;
    word(output, base, 5, facts.includes.raw)
}

pub(super) fn validate_fact(
    i: &[u8],
    b: usize,
    k: LanguageExtensionDirectoryKind,
    f: u32,
    n: LanguageExtensionCommonBounds,
) -> Result<(), LanguageExtensionReopenError> {
    let w = |x: usize| {
        let offset = x
            .checked_mul(4)
            .and_then(|width| b.checked_add(width))
            .ok_or(LanguageExtensionReopenError::StructuralOverflow { offset: b })?;
        get_u32(i, offset)
    };
    validate_fact_encoding(k, f, &w)?;
    let check = |raw, limit| {
        if raw < limit {
            Ok(())
        } else {
            Err(LanguageExtensionReopenError::SharedReference {
                kind: k,
                fact: f,
                raw,
                limit,
            })
        }
    };
    let check_type_parameters = |raw| match n.type_parameters {
        crate::ir::TypeParameterListBounds::ExactRanges { count } if raw < count => Ok(()),
        crate::ir::TypeParameterListBounds::LegacyStarts { element_count }
            if raw <= element_count =>
        {
            Ok(())
        }
        crate::ir::TypeParameterListBounds::ExactRanges { count } => {
            Err(LanguageExtensionReopenError::SharedReference {
                kind: k,
                fact: f,
                raw,
                limit: count,
            })
        }
        crate::ir::TypeParameterListBounds::LegacyStarts { element_count } => {
            Err(LanguageExtensionReopenError::SharedReference {
                kind: k,
                fact: f,
                raw,
                limit: element_count,
            })
        }
    };
    match k {
        LanguageExtensionDirectoryKind::TypeScript => {
            check_type_parameters(w(0)?)?;
            for x in [w(1)?, w(2)?] {
                if x != NONE {
                    check(x, n.types)?;
                }
            }
        }
        LanguageExtensionDirectoryKind::CSharp => {
            check_type_parameters(w(4)?)?;
            check(w(5)?, n.atom_lists)?;
            if w(6)? != NONE {
                check(w(6)?, n.atoms)?;
            }
        }
        LanguageExtensionDirectoryKind::Go => {
            for x in [w(0)?, w(1)?] {
                check(x, n.type_lists)?;
            }
            check_type_parameters(w(3)?)?;
            for x in [w(4)?, w(5)?] {
                check(x, n.entity_lists)?;
            }
            check(w(6)?, n.atom_lists)?;
            check(w(7)?, n.atom_lists)?;
        }
        LanguageExtensionDirectoryKind::Rust => {
            check(w(1)?, n.atom_lists)?;
            check_type_parameters(w(2)?)?;
            check(w(3)?, n.atom_lists)?;
            check(w(4)?, n.atom_lists)?;
            check(w(5)?, n.free_predicates)?;
        }
        LanguageExtensionDirectoryKind::Python => check(w(0)?, n.atom_lists)?,
        LanguageExtensionDirectoryKind::Java => {
            check(w(0)?, n.type_lists)?;
            check(w(1)?, n.atom_lists)?;
            check(w(2)?, n.entity_lists)?;
            check(w(3)?, n.entity_lists)?;
        }
        LanguageExtensionDirectoryKind::Clang => {
            check_type_parameters(w(4)?)?;
            check(w(5)?, n.atom_lists)?;
        }
    }
    let decoded = match k {
        LanguageExtensionDirectoryKind::TypeScript => {
            crate::ir::TypeScriptFacts::decode(i, b).is_some()
        }
        LanguageExtensionDirectoryKind::CSharp => crate::ir::CSharpFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Go => crate::ir::GoFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Rust => crate::ir::RustFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Python => crate::ir::PythonFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Java => crate::ir::JavaFacts::decode(i, b).is_some(),
        LanguageExtensionDirectoryKind::Clang => crate::ir::ClangFacts::decode(i, b).is_some(),
    };
    if !decoded {
        return Err(LanguageExtensionReopenError::FactEncoding {
            kind: k,
            fact: f,
            word: 0,
            observed: w(0)?,
        });
    }
    Ok(())
}

fn validate_fact_encoding(
    kind: LanguageExtensionDirectoryKind,
    fact: u32,
    word: &impl Fn(usize) -> Result<u32, LanguageExtensionReopenError>,
) -> Result<(), LanguageExtensionReopenError> {
    let reject = |word_index: u8, observed: u32| {
        Err(LanguageExtensionReopenError::FactEncoding {
            kind,
            fact,
            word: word_index,
            observed,
        })
    };
    match kind {
        LanguageExtensionDirectoryKind::TypeScript | LanguageExtensionDirectoryKind::Java => {}
        LanguageExtensionDirectoryKind::CSharp => {
            let nullability = word(0)?;
            if nullability > 2 {
                return reject(0, nullability);
            }
            let reference_kind = word(1)?;
            if reference_kind > 3 {
                return reject(1, reference_kind);
            }
            let effects = word(2)?;
            if effects & !7 != 0 {
                return reject(2, effects);
            }
            let partial = word(3)?;
            if partial > 2 {
                return reject(3, partial);
            }
            let file = word(6)?;
            let start = word(7)?;
            let end = word(8)?;
            if (file == NONE && (start != 0 || end != 0)) || (file != NONE && start > end) {
                return Err(LanguageExtensionReopenError::SourceSpanEncoding {
                    kind,
                    fact,
                    file,
                    start,
                    end,
                });
            }
        }
        LanguageExtensionDirectoryKind::Go => {
            let variadic = word(2)?;
            if variadic > 1 {
                return reject(2, variadic);
            }
            let constant_flags = word(10)?;
            if constant_flags & !1 != 0 {
                return reject(10, constant_flags);
            }
        }
        LanguageExtensionDirectoryKind::Rust => {
            let ownership = word(0)?;
            if ownership > 3 {
                return reject(0, ownership);
            }
        }
        LanguageExtensionDirectoryKind::Python => {
            let parameter_kind = word(1)?;
            if parameter_kind > 4 {
                return reject(1, parameter_kind);
            }
            let confidence = word(2)?;
            if confidence > 4 {
                return reject(2, confidence);
            }
        }
        LanguageExtensionDirectoryKind::Clang => {
            let qualifiers = word(0)?;
            if qualifiers & !7 != 0 {
                return reject(0, qualifiers);
            }
            let storage = word(1)?;
            if storage > 5 {
                return reject(1, storage);
            }
            let alignment = word(3)?;
            if alignment == 0 {
                return reject(3, alignment);
            }
        }
    }
    Ok(())
}
