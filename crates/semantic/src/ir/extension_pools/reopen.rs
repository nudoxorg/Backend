//! Trusted reopen validation and borrowed discovery for pooled extension
//! reference lanes shared by a fragment's language-extension section.
//!
//! The extension section's typed facts reference six pooled lanes by dense
//! coordinate: type-parameter elements, ordered type-parameter bounds,
//! type-parameter ranges, atom lists, type lists, and entity lists.
//! This plane carries those lanes and proves every reference stays inside
//! the carrying fragment's atom, type-fact, and entity lanes.

use core::ops::Deref;

use super::{grammar::*, model::*};
use crate::ir::{
    TypeParameterInference, TypeParameterPrimaryRequirement, TypeParameterRequirements, Variance,
};

/// One decoded pooled reference list, read back in bounded pieces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedRefList<'payload> {
    /// The list bytes, a dense run of little-endian `u32` references.
    pub words: &'payload [u8],
}

impl DecodedRefList<'_> {
    /// Number of references in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len() / 4
    }

    /// True when the list carries no references.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Decodes one reference by position.
    #[must_use]
    pub fn get(&self, position: usize) -> Option<u32> {
        let start = position.checked_mul(4)?;
        let end = start.checked_add(4)?;
        let raw = self.words.get(start..end)?;
        Some(u32::from_le_bytes(raw.try_into().ok()?))
    }

    /// Iterates every reference in list order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    }
}

/// One decoded type parameter, lending the section bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeParameter<'payload> {
    /// Parameter name spelling.
    pub name: &'payload [u8],
    /// Default type-fact ordinal, when present.
    pub default: Option<u32>,
    /// Schema-aware semantic payload. Legacy wire rows deliberately expose
    /// only what they actually encoded rather than fabricating modern facts.
    pub semantics: DecodedTypeParameterSemantics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodedTypeParameterSemantics {
    Exact {
        bounds: ExtensionTypeParameterBoundRange,
        variance: Variance,
        kind: DecodedTypeParameterKind,
        requirements: TypeParameterRequirements,
    },
    Legacy {
        constraint: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodedTypeParameterKind {
    Type { inference: TypeParameterInference },
    ConstValue { value_type: u32 },
    Lifetime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodedTypeParameterBound<'payload> {
    Type(u32),
    Lifetime(&'payload [u8]),
}

/// Borrowed exact range of ordered generic bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeParameterBoundList<'payload> {
    bytes: &'payload [u8],
    bounds_start: usize,
    bound_count: u32,
    range: ExtensionTypeParameterBoundRange,
}

impl<'payload> DecodedTypeParameterBoundList<'payload> {
    pub fn get(
        &self,
        position: u32,
    ) -> Result<DecodedTypeParameterBound<'payload>, ExtensionPoolFault> {
        if position >= self.range.length {
            return Err(ExtensionPoolFault::TypeParameterBoundPosition {
                position,
                length: self.range.length,
            });
        }
        let ordinal = self.range.start.checked_add(position).ok_or(
            ExtensionPoolFault::StructuralOverflow {
                at: self.bounds_start,
            },
        )?;
        decode_type_parameter_bound(self.bytes, self.bounds_start, self.bound_count, ordinal)
    }

    /// Opens one allocation-free cursor. It seeks the range start once and
    /// decodes every variable-width bound exactly once thereafter.
    pub fn cursor(&self) -> Result<DecodedTypeParameterBoundCursor<'payload>, ExtensionPoolFault> {
        let mut cursor = self.bounds_start;
        for _ in 0..self.range.start {
            cursor = skip_type_parameter_bound(self.bytes, cursor)?;
        }
        Ok(DecodedTypeParameterBoundCursor {
            bytes: self.bytes,
            cursor,
            remaining: self.range.length,
        })
    }
}

impl Deref for DecodedTypeParameterBoundList<'_> {
    type Target = ExtensionTypeParameterBoundRange;

    fn deref(&self) -> &Self::Target {
        &self.range
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DecodedTypeParameterBoundCursor<'payload> {
    bytes: &'payload [u8],
    cursor: usize,
    remaining: u32,
}

impl<'payload> Iterator for DecodedTypeParameterBoundCursor<'payload> {
    type Item = Result<DecodedTypeParameterBound<'payload>, ExtensionPoolFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        match decode_type_parameter_bound_at(self.bytes, self.cursor) {
            Ok((bound, next)) => {
                self.cursor = next;
                self.remaining -= 1;
                Some(Ok(bound))
            }
            Err(fault) => {
                self.remaining = 0;
                Some(Err(fault))
            }
        }
    }
}

/// One exact borrowed type-parameter list reopened from a schema-4+ range
/// table.  Its `get` method proves the returned member stays in the captured
/// range; callers never infer an end from a global element cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeParameterList<'payload> {
    schema: u16,
    bytes: &'payload [u8],
    elements_start: usize,
    element_count: u32,
    range: ExtensionTypeParameterRange,
}

impl<'payload> DecodedTypeParameterList<'payload> {
    /// Decodes one member of this exact list.
    pub fn get(&self, position: u32) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
        if position >= self.range.length {
            return Err(ExtensionPoolFault::TypeParameterPosition {
                position,
                length: self.range.length,
            });
        }
        let ordinal = self.range.start.checked_add(position).ok_or(
            ExtensionPoolFault::StructuralOverflow {
                at: self.elements_start,
            },
        )?;
        decode_type_parameter(
            self.schema,
            self.bytes,
            self.elements_start,
            self.element_count,
            ordinal,
        )
    }

    /// Opens one sequential cursor over this exact range.  The cursor seeks
    /// the global first element once, then advances each variable-width row
    /// exactly once; full-list discovery is linear and allocation-free.
    pub fn cursor(&self) -> Result<DecodedTypeParameterCursor<'payload>, ExtensionPoolFault> {
        let mut cursor = self.elements_start;
        for _ in 0..self.range.start {
            cursor = skip_type_parameter(self.schema, self.bytes, cursor)?;
        }
        Ok(DecodedTypeParameterCursor {
            schema: self.schema,
            bytes: self.bytes,
            cursor,
            remaining: self.range.length,
        })
    }
}

impl Deref for DecodedTypeParameterList<'_> {
    type Target = ExtensionTypeParameterRange;

    fn deref(&self) -> &Self::Target {
        &self.range
    }
}

/// Fallible sequential view of one validated exact type-parameter range.
#[derive(Clone, Copy, Debug)]
pub struct DecodedTypeParameterCursor<'payload> {
    schema: u16,
    bytes: &'payload [u8],
    cursor: usize,
    remaining: u32,
}

impl<'payload> Iterator for DecodedTypeParameterCursor<'payload> {
    type Item = Result<DecodedTypeParameter<'payload>, ExtensionPoolFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let decoded = decode_type_parameter_at(self.schema, self.bytes, self.cursor);
        match decoded {
            Ok((parameter, next)) => {
                self.cursor = next;
                self.remaining -= 1;
                Some(Ok(parameter))
            }
            Err(fault) => {
                self.remaining = 0;
                Some(Err(fault))
            }
        }
    }
}

/// Schema-aware reopened interpretation of one extension fact's
/// `TypeParameterListId`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReopenedTypeParameterList<'payload> {
    /// Schema-4 exact `(start, length)` membership.
    Exact(DecodedTypeParameterList<'payload>),
    /// Legacy schema records name only an element start.  No list membership
    /// is implied or fabricated.
    LegacyStartOnly { start: u32 },
}

/// A once-validated reopened pooled-lane section.
#[derive(Clone, Copy, Debug)]
pub struct ReopenedExtensionPools<'payload> {
    schema: u16,
    bytes: &'payload [u8],
    type_parameters: (usize, u32),
    type_parameter_bounds: Option<(usize, u32)>,
    type_parameter_lists: TypeParameterListLayout,
    atom_lists: (usize, u32),
    type_lists: (usize, u32),
    entity_lists: (usize, u32),
}

#[derive(Clone, Copy, Debug)]
enum TypeParameterListLayout {
    Exact { start: usize, count: u32 },
    LegacyStarts,
}

impl<'payload> ReopenedExtensionPools<'payload> {
    /// Number of pooled type parameters.
    #[must_use]
    pub fn type_parameter_count(&self) -> u32 {
        self.type_parameters.1
    }

    /// The schema-aware meaning of `TypeParameterListId` references in the
    /// paired language-extension section.
    #[must_use]
    pub const fn type_parameter_list_bounds(&self) -> TypeParameterListBounds {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { count, .. } => {
                TypeParameterListBounds::ExactRanges { count }
            }
            TypeParameterListLayout::LegacyStarts => TypeParameterListBounds::LegacyStarts {
                element_count: self.type_parameters.1,
            },
        }
    }

    /// Number of exact type-parameter lists when this schema carries range
    /// membership. Legacy start-only pools deliberately report `None`.
    #[must_use]
    pub const fn type_parameter_list_count(&self) -> Option<u32> {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { count, .. } => Some(count),
            TypeParameterListLayout::LegacyStarts => None,
        }
    }

    /// Number of pooled atom-reference lists.
    #[must_use]
    pub fn atom_list_count(&self) -> u32 {
        self.atom_lists.1
    }

    /// Number of pooled type-reference lists.
    #[must_use]
    pub fn type_list_count(&self) -> u32 {
        self.type_lists.1
    }

    /// Number of pooled entity-reference lists.
    #[must_use]
    pub fn entity_list_count(&self) -> u32 {
        self.entity_lists.1
    }

    /// Decodes one pooled type parameter.
    pub fn type_parameter(
        &self,
        ordinal: u32,
    ) -> Result<DecodedTypeParameter<'payload>, ExtensionPoolFault> {
        decode_type_parameter(
            self.schema,
            self.bytes,
            self.type_parameters.0,
            self.type_parameters.1,
            ordinal,
        )
    }

    /// Reopens the exact ordered bounds owned by one schema-5 type parameter.
    /// Legacy parameter records retain only their explicit singleton optional
    /// constraint and intentionally cannot masquerade as an ordered list.
    pub fn type_parameter_bounds(
        &self,
        parameter: DecodedTypeParameter<'payload>,
    ) -> Result<Option<DecodedTypeParameterBoundList<'payload>>, ExtensionPoolFault> {
        let DecodedTypeParameterSemantics::Exact { bounds: range, .. } = parameter.semantics else {
            return Ok(None);
        };
        let Some((start, count)) = self.type_parameter_bounds else {
            return Err(ExtensionPoolFault::StructuralOverflow {
                at: self.type_parameters.0,
            });
        };
        let end = range.start.checked_add(range.length).ok_or(
            ExtensionPoolFault::TypeParameterBounds {
                ordinal: 0,
                start: range.start,
                length: range.length,
                bound_count: count,
            },
        )?;
        if end > count {
            return Err(ExtensionPoolFault::TypeParameterBounds {
                ordinal: 0,
                start: range.start,
                length: range.length,
                bound_count: count,
            });
        }
        Ok(Some(DecodedTypeParameterBoundList {
            bytes: self.bytes,
            bounds_start: start,
            bound_count: count,
            range,
        }))
    }

    /// Reopens one `TypeParameterListId` under the schema's explicit
    /// membership contract. See [`ReopenedTypeParameterList`] for legacy
    /// behavior.
    pub fn type_parameter_list(
        &self,
        list: crate::ir::TypeParameterListId,
    ) -> Result<ReopenedTypeParameterList<'payload>, ExtensionPoolFault> {
        match self.type_parameter_lists {
            TypeParameterListLayout::Exact { start, count } => {
                if list.raw >= count {
                    return Err(ExtensionPoolFault::TypeParameterList {
                        list: list.raw,
                        count,
                    });
                }
                let offset = usize::try_from(list.raw)
                    .ok()
                    .and_then(|ordinal| ordinal.checked_mul(8))
                    .and_then(|width| start.checked_add(width))
                    .ok_or(ExtensionPoolFault::StructuralOverflow { at: start })?;
                let range = ExtensionTypeParameterRange {
                    start: read_u32(self.bytes, offset)?,
                    length: read_u32(
                        self.bytes,
                        offset
                            .checked_add(4)
                            .ok_or(ExtensionPoolFault::StructuralOverflow { at: offset })?,
                    )?,
                };
                Ok(ReopenedTypeParameterList::Exact(DecodedTypeParameterList {
                    schema: self.schema,
                    bytes: self.bytes,
                    elements_start: self.type_parameters.0,
                    element_count: self.type_parameters.1,
                    range,
                }))
            }
            TypeParameterListLayout::LegacyStarts => {
                if list.raw > self.type_parameters.1 {
                    return Err(ExtensionPoolFault::LegacyTypeParameterStart {
                        start: list.raw,
                        element_count: self.type_parameters.1,
                    });
                }
                Ok(ReopenedTypeParameterList::LegacyStartOnly { start: list.raw })
            }
        }
    }

    /// Decodes one pooled reference list from its closed homogeneous lane.
    pub fn reference_list(
        &self,
        lane: ExtensionPoolListLane,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        let (start, count) = match lane {
            ExtensionPoolListLane::Atoms => self.atom_lists,
            ExtensionPoolListLane::Types => self.type_lists,
            ExtensionPoolListLane::Entities => self.entity_lists,
        };
        if ordinal >= count {
            return Err(ExtensionPoolFault::Reference {
                lane,
                list: ordinal,
                position: 0,
                raw: ordinal,
                limit: count,
            });
        }
        let mut cursor = start;
        for _ in 0..ordinal {
            cursor = skip_ref_list(self.bytes, cursor)?;
        }
        let len = usize::try_from(read_u32(self.bytes, cursor)?)
            .map_err(|_| ExtensionPoolFault::Truncated { needed: cursor })?;
        let words_start = cursor
            .checked_add(4)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words_len = len
            .checked_mul(4)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words_end = words_start
            .checked_add(words_len)
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: cursor })?;
        let words = self
            .bytes
            .get(words_start..words_end)
            .ok_or(ExtensionPoolFault::Truncated { needed: cursor })?;
        Ok(DecodedRefList { words })
    }

    /// Reads one homogeneous pooled list without admitting a stringly lane
    /// selection at a render or discovery call site.
    pub fn list(
        &self,
        lane: ExtensionPoolListLane,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.reference_list(lane, ordinal)
    }

    pub fn atom_list(&self, ordinal: u32) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Atoms, ordinal)
    }

    pub fn type_list(&self, ordinal: u32) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Types, ordinal)
    }

    pub fn entity_list(
        &self,
        ordinal: u32,
    ) -> Result<DecodedRefList<'payload>, ExtensionPoolFault> {
        self.list(ExtensionPoolListLane::Entities, ordinal)
    }
}

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
    Ok(ReopenedExtensionPools {
        schema,
        bytes: payload,
        type_parameters,
        type_parameter_bounds,
        type_parameter_lists,
        atom_lists,
        type_lists,
        entity_lists,
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
    if schema >= 5 {
        let bound_count = read_u32(payload, cursor)?;
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
    if cursor != payload.len() {
        return Err(ExtensionPoolFault::TrailingBytes);
    }
    Ok(())
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, ExtensionPoolFault> {
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

fn skip_type_parameter(
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

fn skip_type_parameter_bound(bytes: &[u8], cursor: usize) -> Result<usize, ExtensionPoolFault> {
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

fn skip_ref_list(bytes: &[u8], cursor: usize) -> Result<usize, ExtensionPoolFault> {
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

fn advance(at: usize, width: usize) -> Result<usize, ExtensionPoolFault> {
    at.checked_add(width)
        .ok_or(ExtensionPoolFault::StructuralOverflow { at })
}

fn decode_type_parameter<'payload>(
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

fn decode_type_parameter_at<'payload>(
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

fn decode_type_parameter_bound<'payload>(
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

fn decode_type_parameter_bound_at<'payload>(
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
