//! Trusted reopen of pooled extension lanes.
//!
//! Every byte and cross-lane reference is proved against the carrying
//! fragment before borrowed views are returned.

use super::super::{grammar::*, model::*};
use super::{
    DecodedTypeParameter, DecodedTypeParameterBound, DecodedTypeParameterKind,
    DecodedTypeParameterSemantics, ReopenedExtensionPools, TypeParameterListLayout,
};
use crate::ir::{
    TypeParameterInference, TypeParameterPrimaryRequirement, TypeParameterRequirements, Variance,
};

/// Reopens the shared extension pools after proving every byte and reference
/// against the carrying fragment's already-validated common lanes.
pub fn reopen_extension_pools(
    schema: u16,
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<ReopenedExtensionPools<'_>, ExtensionPoolFault> {
    validate_extension_pool_payload(schema, payload, atom_count, type_count, entity_count)?;
    reopen_validated_extension_pools(schema, payload)
}

/// Decodes an extension-pool layout whose bytes and cross-lane references
/// were already proved by fragment validation. This is crate-private so a
/// caller cannot bypass the public checked reopen boundary.
pub(crate) fn reopen_validated_extension_pools(
    schema: u16,
    payload: &[u8],
) -> Result<ReopenedExtensionPools<'_>, ExtensionPoolFault> {
    let type_count = read_u32(payload, 0)?;
    let mut cursor = 4;
    let type_parameters = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_type_parameter(schema, payload, cursor)?;
    }
    let type_parameter_bounds = if schema >= 5 {
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        let start = cursor;
        for _ in 0..count {
            cursor = skip_type_parameter_bound(payload, cursor)?;
        }
        Some((start, count))
    } else {
        None
    };
    let type_parameter_lists = if schema >= 4 {
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        let start = cursor;
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(8))
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        cursor = advance(cursor, bytes)?;
        TypeParameterListLayout::Exact { start, count }
    } else {
        TypeParameterListLayout::LegacyStarts
    };
    let atom_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let atom_lists = (cursor, atom_count);
    for _ in 0..atom_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let type_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let type_lists = (cursor, type_count);
    for _ in 0..type_count {
        cursor = skip_ref_list(payload, cursor)?;
    }
    let entity_count = read_u32(payload, cursor)?;
    cursor = advance(cursor, 4)?;
    let entity_lists = (cursor, entity_count);
    let (free_predicates, free_predicate_lists) = if schema >= 7 {
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        let start = cursor;
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(12))
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        cursor = advance(cursor, bytes)?;
        let free_predicates = (start, count);
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        let start = cursor;
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(8))
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        cursor = advance(cursor, bytes)?;
        (free_predicates, (start, count))
    } else {
        ((0, 0), (0, 0))
    };
    Ok(ReopenedExtensionPools {
        schema,
        bytes: payload,
        type_parameters,
        type_parameter_bounds,
        type_parameter_lists,
        atom_lists,
        type_lists,
        entity_lists,
        free_predicates,
        free_predicate_lists,
    })
}

/// Validates one pooled-lane payload against the carrying fragment's lane
/// counts, proving every reference and type-parameter cell.
pub(crate) fn validate_extension_pool_payload(
    schema: u16,
    payload: &[u8],
    atom_count: u32,
    type_count: u32,
    entity_count: u32,
) -> Result<(), ExtensionPoolFault> {
    let mut cursor = 4;
    let type_parameter_count = read_u32(payload, 0)?;
    for ordinal in 0..type_parameter_count {
        let name_len_offset = advance(cursor, 1)?;
        let name_len = usize::try_from(read_u32(payload, name_len_offset)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let name_start = advance(cursor, 5)?;
        let name_end = advance(name_start, name_len)?;
        let name = payload
            .get(name_start..name_end)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        if name.is_empty() {
            return Err(ExtensionPoolFault::EmptyName { ordinal });
        }
        let mut at = name_end;
        if schema >= 5 {
            let bounds = ExtensionTypeParameterBoundRange {
                start: read_u32(payload, at)?,
                length: read_u32(payload, advance(at, 4)?)?,
            };
            at = advance(at, 8)?;
            let default = read_optional(payload, &mut at)?;
            if let Some(raw) = default
                && raw >= type_count
            {
                return Err(ExtensionPoolFault::TypeReference {
                    ordinal,
                    field: TypeParameterField::Default,
                    raw,
                    limit: type_count,
                });
            }
            let variance = payload
                .get(at)
                .copied()
                .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
            let _ = decode_variance(ordinal, variance)?;
            at = advance(at, 1)?;
            let kind = payload
                .get(at)
                .copied()
                .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
            at = advance(at, 1)?;
            if kind == TYPE_PARAMETER_KIND_CONST_VALUE {
                let raw = read_u32(payload, at)?;
                if raw >= type_count {
                    return Err(ExtensionPoolFault::TypeReference {
                        ordinal,
                        field: TypeParameterField::ConstValueType,
                        raw,
                        limit: type_count,
                    });
                }
                at = advance(at, 4)?;
            } else if !matches!(
                kind,
                TYPE_PARAMETER_KIND_TYPE
                    | TYPE_PARAMETER_KIND_CONST_INFERENCE
                    | TYPE_PARAMETER_KIND_LIFETIME
            ) {
                return Err(ExtensionPoolFault::TypeParameterTag {
                    ordinal,
                    field: TypeParameterTagField::Kind,
                    actual: kind,
                });
            }
            let primary = payload
                .get(at)
                .copied()
                .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
            let primary = decode_primary_requirement(ordinal, primary)?;
            at = advance(at, 1)?;
            let flags = payload
                .get(at)
                .copied()
                .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
            let requirements = TypeParameterRequirements {
                primary,
                ..decode_requirement_flags(ordinal, flags)?
            };
            if !requirements.is_valid() {
                return Err(ExtensionPoolFault::TypeParameterRequirements {
                    ordinal,
                    primary: requirements.primary,
                    constructor: requirements.constructor,
                    allows_ref_like: requirements.allows_ref_like,
                });
            }
            at = advance(at, 1)?;
            // Bound ranges are checked after their flat lane is decoded.
            let _ = bounds;
        } else {
            let constraint = read_optional(payload, &mut at)?;
            let default = read_optional(payload, &mut at)?;
            for (field, raw) in [
                (TypeParameterField::Default, default),
                // The legacy schema called this a constraint; retain the
                // exact operand but name it as a generic type reference here.
                (TypeParameterField::LegacyConstraint, constraint),
            ] {
                if let Some(raw) = raw
                    && raw >= type_count
                {
                    return Err(ExtensionPoolFault::TypeReference {
                        ordinal,
                        field,
                        raw,
                        limit: type_count,
                    });
                }
            }
        }
        cursor = at;
    }
    let mut bound_total = 0_u32;
    if schema >= 5 {
        let bound_count = read_u32(payload, cursor)?;
        bound_total = bound_count;
        cursor = advance(cursor, 4)?;
        for position in 0..bound_count {
            let (bound, next) = decode_type_parameter_bound_at(payload, cursor)?;
            match bound {
                DecodedTypeParameterBound::Type(raw) if raw >= type_count => {
                    return Err(ExtensionPoolFault::BoundTypeReference {
                        bound: position,
                        raw,
                        limit: type_count,
                    });
                }
                DecodedTypeParameterBound::Lifetime(name) if name.is_empty() => {
                    return Err(ExtensionPoolFault::EmptyLifetime { bound: position });
                }
                DecodedTypeParameterBound::Type(_) | DecodedTypeParameterBound::Lifetime(_) => {}
            }
            cursor = next;
        }
        // Re-read the compact parameter header rows only to prove every
        // range. The pool is bounded and this occurs once at reopen.
        let mut parameter_cursor = 4;
        for ordinal in 0..type_parameter_count {
            let (parameter, next) = decode_type_parameter_at(schema, payload, parameter_cursor)?;
            if let DecodedTypeParameterSemantics::Exact {
                bounds,
                requirements,
                ..
            } = parameter.semantics
            {
                if !requirements.is_valid() {
                    return Err(ExtensionPoolFault::TypeParameterRequirements {
                        ordinal,
                        primary: requirements.primary,
                        constructor: requirements.constructor,
                        allows_ref_like: requirements.allows_ref_like,
                    });
                }
                let end = bounds.start.checked_add(bounds.length).ok_or(
                    ExtensionPoolFault::TypeParameterBounds {
                        ordinal,
                        start: bounds.start,
                        length: bounds.length,
                        bound_count,
                    },
                )?;
                if end > bound_count {
                    return Err(ExtensionPoolFault::TypeParameterBounds {
                        ordinal,
                        start: bounds.start,
                        length: bounds.length,
                        bound_count,
                    });
                }
            }
            parameter_cursor = next;
        }
    }
    if schema >= 4 {
        let range_count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        for list in 0..range_count {
            let start = read_u32(payload, cursor)?;
            let length = read_u32(payload, advance(cursor, 4)?)?;
            let end = start
                .checked_add(length)
                .ok_or(ExtensionPoolFault::TypeParameterRange {
                    list,
                    start,
                    length,
                    element_count: type_parameter_count,
                })?;
            if end > type_parameter_count {
                return Err(ExtensionPoolFault::TypeParameterRange {
                    list,
                    start,
                    length,
                    element_count: type_parameter_count,
                });
            }
            cursor = advance(cursor, 8)?;
        }
    }
    for (lane, limit) in [
        (ExtensionPoolListLane::Atoms, atom_count),
        (ExtensionPoolListLane::Types, type_count),
        (ExtensionPoolListLane::Entities, entity_count),
    ] {
        let count = read_u32(payload, cursor)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        cursor = advance(cursor, 4)?;
        for list in 0..count {
            let len = usize::try_from(read_u32(payload, cursor)?)
                .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
            let words_start = advance(cursor, 4)?;
            for position in 0..len {
                let word_offset = position
                    .checked_mul(4)
                    .and_then(|width| words_start.checked_add(width))
                    .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
                let raw = read_u32(payload, word_offset)?;
                if raw >= limit {
                    return Err(ExtensionPoolFault::Reference {
                        lane,
                        list,
                        position: u32::try_from(position).unwrap_or(u32::MAX),
                        raw,
                        limit,
                    });
                }
            }
            let words = len
                .checked_mul(4)
                .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
            cursor = advance(advance(cursor, 4)?, words)?;
        }
    }
    if schema >= 7 {
        let count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        for predicate in 0..count {
            let subject = read_u32(payload, cursor)?;
            if subject >= type_count {
                return Err(ExtensionPoolFault::FreePredicateSubject {
                    predicate,
                    raw: subject,
                    limit: type_count,
                });
            }
            let start = read_u32(payload, advance(cursor, 4)?)?;
            let length = read_u32(payload, advance(cursor, 8)?)?;
            cursor = advance(cursor, 12)?;
            let end = start
                .checked_add(length)
                .ok_or(ExtensionPoolFault::FreePredicateBounds {
                    predicate,
                    start,
                    length,
                    bound_count: bound_total,
                })?;
            if end > bound_total {
                return Err(ExtensionPoolFault::FreePredicateBounds {
                    predicate,
                    start,
                    length,
                    bound_count: bound_total,
                });
            }
        }
        let list_count = read_u32(payload, cursor)?;
        cursor = advance(cursor, 4)?;
        for list in 0..list_count {
            let start = read_u32(payload, cursor)?;
            let length = read_u32(payload, advance(cursor, 4)?)?;
            let end = start
                .checked_add(length)
                .ok_or(ExtensionPoolFault::FreePredicateList {
                    list,
                    start,
                    length,
                    predicate_count: count,
                })?;
            if end > count {
                return Err(ExtensionPoolFault::FreePredicateList {
                    list,
                    start,
                    length,
                    predicate_count: count,
                });
            }
            cursor = advance(cursor, 8)?;
        }
    }
    if cursor != payload.len() {
        return Err(ExtensionPoolFault::TrailingBytes);
    }
    Ok(())
}

pub(super) fn read_u32(bytes: &[u8], at: usize) -> Result<u32, ExtensionPoolFault> {
    let end = at
        .checked_add(4)
        .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
    let word: [u8; 4] = bytes
        .get(at..end)
        .ok_or(ExtensionPoolFault::Truncated { needed: at })?
        .try_into()
        .map_err(|_| ExtensionPoolFault::Truncated { needed: at })?;
    Ok(u32::from_le_bytes(word))
}

pub(super) fn skip_type_parameter(
    schema: u16,
    bytes: &[u8],
    cursor: usize,
) -> Result<usize, ExtensionPoolFault> {
    if bytes.get(cursor).copied() != Some(PRESENCE_SOME) {
        return Err(ExtensionPoolFault::Presence {
            actual: bytes.get(cursor).copied().unwrap_or(PRESENCE_NONE),
        });
    }
    let name_len = usize::try_from(read_u32(bytes, advance(cursor, 1)?)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let mut next = advance(advance(cursor, 5)?, name_len)?;
    if schema >= 5 {
        next = advance(next, 8)?;
        let _ = read_optional(bytes, &mut next)?;
        let _variance = bytes
            .get(next)
            .ok_or(ExtensionPoolFault::Truncated { needed: next })?;
        next = advance(next, 1)?;
        let kind = *bytes
            .get(next)
            .ok_or(ExtensionPoolFault::Truncated { needed: next })?;
        next = advance(next, 1)?;
        if kind == TYPE_PARAMETER_KIND_CONST_VALUE {
            next = advance(next, 4)?;
        }
        next = advance(next, 2)?;
    } else {
        // Optional operands are variable-width: `None` occupies its tag only,
        // while `Some` carries four more bytes.
        let _ = read_optional(bytes, &mut next)?;
        let _ = read_optional(bytes, &mut next)?;
    }
    Ok(next)
}

pub(super) fn skip_type_parameter_bound(
    bytes: &[u8],
    cursor: usize,
) -> Result<usize, ExtensionPoolFault> {
    match bytes.get(cursor).copied() {
        Some(TYPE_PARAMETER_BOUND_TYPE) => advance(cursor, 5),
        Some(TYPE_PARAMETER_BOUND_LIFETIME) => {
            let name_len = usize::try_from(read_u32(bytes, advance(cursor, 2)?)?)
                .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
            advance(cursor, 6 + name_len)
        }
        Some(actual) => Err(ExtensionPoolFault::TypeParameterTag {
            ordinal: 0,
            field: TypeParameterTagField::Bound,
            actual,
        }),
        None => Err(ExtensionPoolFault::Truncated { needed: cursor }),
    }
}

pub(super) fn skip_ref_list(bytes: &[u8], cursor: usize) -> Result<usize, ExtensionPoolFault> {
    let len = usize::try_from(read_u32(bytes, cursor)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let words = len
        .checked_mul(4)
        .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
    advance(advance(cursor, 4)?, words)
}

fn read_optional(bytes: &[u8], at: &mut usize) -> Result<Option<u32>, ExtensionPoolFault> {
    match bytes.get(*at).copied() {
        Some(PRESENCE_NONE) => {
            *at = advance(*at, 1)?;
            Ok(None)
        }
        Some(PRESENCE_SOME) => {
            let raw = read_u32(bytes, advance(*at, 1)?)?;
            *at = advance(*at, 5)?;
            Ok(Some(raw))
        }
        actual => Err(ExtensionPoolFault::Presence {
            actual: actual.unwrap_or(PRESENCE_NONE),
        }),
    }
}

pub(super) fn advance(at: usize, width: usize) -> Result<usize, ExtensionPoolFault> {
    at.checked_add(width)
        .ok_or(ExtensionPoolFault::StructuralOverflow { at })
}

pub(super) fn decode_type_parameter<'payload>(
    schema: u16,
    bytes: &'payload [u8],
    start: usize,
    count: u32,
    ordinal: u32,
) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
    if ordinal >= count {
        return Err(ExtensionPoolFault::TypeParameterElement { ordinal, count });
    }
    let mut cursor = start;
    for _ in 0..ordinal {
        cursor = skip_type_parameter(schema, bytes, cursor)?;
    }
    decode_type_parameter_at(schema, bytes, cursor).map(|(parameter, _)| parameter)
}

pub(super) fn decode_type_parameter_at<'payload>(
    schema: u16,
    bytes: &'payload [u8],
    cursor: usize,
) -> Result<(DecodedTypeParameter<'payload>, usize), ExtensionPoolFault> {
    let name_len = usize::try_from(read_u32(bytes, advance(cursor, 1)?)?)
        .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
    let name_start = advance(cursor, 5)?;
    let name_end = advance(name_start, name_len)?;
    let name = bytes
        .get(name_start..name_end)
        .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
    let mut at = name_end;
    if schema < 5 {
        let constraint = read_optional(bytes, &mut at)?;
        let default = read_optional(bytes, &mut at)?;
        return Ok((
            DecodedTypeParameter {
                name,
                default,
                semantics: DecodedTypeParameterSemantics::Legacy { constraint },
            },
            at,
        ));
    }
    let bounds = ExtensionTypeParameterBoundRange {
        start: read_u32(bytes, at)?,
        length: read_u32(bytes, advance(at, 4)?)?,
    };
    at = advance(at, 8)?;
    let default = read_optional(bytes, &mut at)?;
    let variance = decode_variance(
        0,
        *bytes
            .get(at)
            .ok_or(ExtensionPoolFault::Truncated { needed: at })?,
    )?;
    at = advance(at, 1)?;
    let kind_tag = *bytes
        .get(at)
        .ok_or(ExtensionPoolFault::Truncated { needed: at })?;
    at = advance(at, 1)?;
    let kind = match kind_tag {
        TYPE_PARAMETER_KIND_TYPE => DecodedTypeParameterKind::Type {
            inference: TypeParameterInference::Ordinary,
        },
        TYPE_PARAMETER_KIND_CONST_INFERENCE => DecodedTypeParameterKind::Type {
            inference: TypeParameterInference::Const,
        },
        TYPE_PARAMETER_KIND_CONST_VALUE => {
            let value_type = read_u32(bytes, at)?;
            at = advance(at, 4)?;
            DecodedTypeParameterKind::ConstValue { value_type }
        }
        TYPE_PARAMETER_KIND_LIFETIME => DecodedTypeParameterKind::Lifetime,
        actual => {
            return Err(ExtensionPoolFault::TypeParameterTag {
                ordinal: 0,
                field: TypeParameterTagField::Kind,
                actual,
            });
        }
    };
    let primary = decode_primary_requirement(
        0,
        *bytes
            .get(at)
            .ok_or(ExtensionPoolFault::Truncated { needed: at })?,
    )?;
    at = advance(at, 1)?;
    let requirements = decode_requirement_flags(
        0,
        *bytes
            .get(at)
            .ok_or(ExtensionPoolFault::Truncated { needed: at })?,
    )?;
    at = advance(at, 1)?;
    Ok((
        DecodedTypeParameter {
            name,
            default,
            semantics: DecodedTypeParameterSemantics::Exact {
                bounds,
                variance,
                kind,
                requirements: TypeParameterRequirements {
                    primary,
                    ..requirements
                },
            },
        },
        at,
    ))
}

pub(super) fn decode_type_parameter_bound<'payload>(
    bytes: &'payload [u8],
    start: usize,
    count: u32,
    ordinal: u32,
) -> Result<DecodedTypeParameterBound<'payload>, ExtensionPoolFault> {
    if ordinal >= count {
        return Err(ExtensionPoolFault::TypeParameterElement { ordinal, count });
    }
    let mut cursor = start;
    for _ in 0..ordinal {
        cursor = skip_type_parameter_bound(bytes, cursor)?;
    }
    decode_type_parameter_bound_at(bytes, cursor).map(|(bound, _)| bound)
}

pub(super) fn decode_type_parameter_bound_at<'payload>(
    bytes: &'payload [u8],
    cursor: usize,
) -> Result<(DecodedTypeParameterBound<'payload>, usize), ExtensionPoolFault> {
    match bytes.get(cursor).copied() {
        Some(TYPE_PARAMETER_BOUND_TYPE) => Ok((
            DecodedTypeParameterBound::Type(read_u32(bytes, advance(cursor, 1)?)?),
            advance(cursor, 5)?,
        )),
        Some(TYPE_PARAMETER_BOUND_LIFETIME) => {
            let name_len = usize::try_from(read_u32(bytes, advance(cursor, 2)?)?)
                .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
            let start = advance(cursor, 6)?;
            let end = advance(start, name_len)?;
            let name = bytes
                .get(start..end)
                .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
            Ok((DecodedTypeParameterBound::Lifetime(name), end))
        }
        Some(actual) => Err(ExtensionPoolFault::TypeParameterTag {
            ordinal: 0,
            field: TypeParameterTagField::Bound,
            actual,
        }),
        None => Err(ExtensionPoolFault::Truncated { needed: cursor }),
    }
}

fn decode_variance(ordinal: u32, actual: u8) -> Result<Variance, ExtensionPoolFault> {
    match actual {
        VARIANCE_INVARIANT => Ok(Variance::Invariant),
        VARIANCE_COVARIANT => Ok(Variance::Covariant),
        VARIANCE_CONTRAVARIANT => Ok(Variance::Contravariant),
        VARIANCE_BIVARIANT => Ok(Variance::Bivariant),
        _ => Err(ExtensionPoolFault::TypeParameterTag {
            ordinal,
            field: TypeParameterTagField::Variance,
            actual,
        }),
    }
}

fn decode_primary_requirement(
    ordinal: u32,
    actual: u8,
) -> Result<TypeParameterPrimaryRequirement, ExtensionPoolFault> {
    match actual {
        PRIMARY_REQUIREMENT_NONE => Ok(TypeParameterPrimaryRequirement::None),
        PRIMARY_REQUIREMENT_REFERENCE => {
            Ok(TypeParameterPrimaryRequirement::Reference { nullable: false })
        }
        PRIMARY_REQUIREMENT_NULLABLE_REFERENCE => {
            Ok(TypeParameterPrimaryRequirement::Reference { nullable: true })
        }
        PRIMARY_REQUIREMENT_VALUE => Ok(TypeParameterPrimaryRequirement::Value),
        PRIMARY_REQUIREMENT_UNMANAGED => Ok(TypeParameterPrimaryRequirement::Unmanaged),
        PRIMARY_REQUIREMENT_NOT_NULL => Ok(TypeParameterPrimaryRequirement::NotNull),
        PRIMARY_REQUIREMENT_DEFAULT => Ok(TypeParameterPrimaryRequirement::Default),
        _ => Err(ExtensionPoolFault::TypeParameterTag {
            ordinal,
            field: TypeParameterTagField::PrimaryRequirement,
            actual,
        }),
    }
}

fn decode_requirement_flags(
    ordinal: u32,
    actual: u8,
) -> Result<TypeParameterRequirements, ExtensionPoolFault> {
    if actual & !(REQUIREMENT_CONSTRUCTOR | REQUIREMENT_ALLOWS_REF_LIKE) != 0 {
        return Err(ExtensionPoolFault::TypeParameterTag {
            ordinal,
            field: TypeParameterTagField::RequirementFlags,
            actual,
        });
    }
    Ok(TypeParameterRequirements {
        primary: TypeParameterPrimaryRequirement::None,
        constructor: actual & REQUIREMENT_CONSTRUCTOR != 0,
        allows_ref_like: actual & REQUIREMENT_ALLOWS_REF_LIKE != 0,
    })
}
