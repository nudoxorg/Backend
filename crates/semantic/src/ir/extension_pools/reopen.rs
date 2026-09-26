//! Trusted reopen validation and borrowed discovery for pooled extension
//! reference lanes shared by a fragment's language-extension section.
//!
//! The extension section's typed facts reference six pooled lanes by dense
//! coordinate: type-parameter elements, ordered type-parameter bounds,
//! type-parameter ranges, atom lists, type lists, and entity lists.
//! This plane carries those lanes and proves every reference stays inside
//! the carrying fragment's atom, type-fact, and entity lanes.

use core::ops::Deref;

use super::model::*;
use crate::ir::{TypeParameterInference, TypeParameterRequirements, Variance};

mod decode;

pub use decode::reopen_extension_pools;
pub(crate) use decode::reopen_validated_extension_pools;
use decode::{
    advance, decode_type_parameter, decode_type_parameter_at, decode_type_parameter_bound,
    decode_type_parameter_bound_at, read_u32, skip_ref_list, skip_type_parameter,
    skip_type_parameter_bound,
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
    free_predicates: (usize, u32),
    free_predicate_lists: (usize, u32),
}

/// One decoded Rust free predicate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedFreePredicate {
    /// Subject type-fact coordinate.
    pub subject: u32,
    /// Ordered bound run in the shared bound lane.
    pub bounds: ExtensionTypeParameterBoundRange,
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

    /// Number of Rust free-predicate list rows available in this schema.
    #[must_use]
    pub fn free_predicate_list_count(&self) -> u32 {
        self.free_predicate_lists.1
    }

    /// Reopens one Rust free-predicate list's exact `(start, length)` run.
    pub fn free_predicate_list(
        &self,
        list: crate::ir::FreePredicateListId,
    ) -> Result<ExtensionTypeParameterRange, ExtensionPoolFault> {
        let (start, count) = self.free_predicate_lists;
        if list.raw >= count {
            return Err(ExtensionPoolFault::FreePredicateList {
                list: list.raw,
                start: 0,
                length: 0,
                predicate_count: count,
            });
        }
        let offset = usize::try_from(list.raw)
            .ok()
            .and_then(|ordinal| ordinal.checked_mul(8))
            .and_then(|width| start.checked_add(width))
            .ok_or(ExtensionPoolFault::StructuralOverflow { at: start })?;
        Ok(ExtensionTypeParameterRange {
            start: read_u32(self.bytes, offset)?,
            length: read_u32(
                self.bytes,
                offset
                    .checked_add(4)
                    .ok_or(ExtensionPoolFault::StructuralOverflow { at: offset })?,
            )?,
        })
    }

    /// Decodes one Rust free predicate from the row table.
    pub fn free_predicate(
        &self,
        ordinal: u32,
    ) -> Result<DecodedFreePredicate, ExtensionPoolFault> {
        let (start, count) = self.free_predicates;
        if ordinal >= count {
            return Err(ExtensionPoolFault::FreePredicateList {
                list: ordinal,
                start: 0,
                length: 0,
                predicate_count: count,
            });
        }
        let mut cursor = start;
        for _ in 0..ordinal {
            cursor = advance(cursor, 12)?;
        }
        Ok(DecodedFreePredicate {
            subject: read_u32(self.bytes, cursor)?,
            bounds: ExtensionTypeParameterBoundRange {
                start: read_u32(self.bytes, advance(cursor, 4)?)?,
                length: read_u32(self.bytes, advance(cursor, 8)?)?,
            },
        })
    }

    /// Reopens the ordered bounds owned by one free predicate.
    pub fn free_predicate_bounds(
        &self,
        predicate: DecodedFreePredicate,
    ) -> Result<DecodedTypeParameterBoundList<'payload>, ExtensionPoolFault> {
        let Some((start, count)) = self.type_parameter_bounds else {
            return Err(ExtensionPoolFault::StructuralOverflow {
                at: self.type_parameters.0,
            });
        };
        let range = predicate.bounds;
        let end = range.start.checked_add(range.length).ok_or(
            ExtensionPoolFault::FreePredicateBounds {
                predicate: 0,
                start: range.start,
                length: range.length,
                bound_count: count,
            },
        )?;
        if end > count {
            return Err(ExtensionPoolFault::FreePredicateBounds {
                predicate: 0,
                start: range.start,
                length: range.length,
                bound_count: count,
            });
        }
        Ok(DecodedTypeParameterBoundList {
            bytes: self.bytes,
            bounds_start: start,
            bound_count: count,
            range,
        })
    }
}

