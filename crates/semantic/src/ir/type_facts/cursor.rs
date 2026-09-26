//! Borrowed cursors over an already admitted type-fact section.
use super::{Reader, TypeFactCounts, TypeFactFault, decode_child, decode_record};
use crate::ir_vocabulary::{EntityId, SemanticTypeChild, SemanticTypeRecord};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Identifies whether a decoded row came from the declared or computed segment.
pub enum TypeFactSegment {
    Declared,
    Computed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeFact<'fragment> {
    pub owner: EntityId,
    pub record: SemanticTypeRecord<'fragment>,
    pub segment: TypeFactSegment,
}

/// One decoded ordered child from the durable type-fact child pool.
///
/// Its ordinal is the common type-child coordinate referenced by each
/// `SemanticTypeRecord::children` span; it is not an entity or image row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedTypeFactChild<'fragment> {
    pub ordinal: u32,
    pub child: SemanticTypeChild<'fragment>,
}

pub struct TypeFactCursor<'fragment> {
    payload: &'fragment [u8],
    at: usize,
    remaining: u32,
    declared_remaining: u32,
    schema: u16,
    ordinal: u32,
}
impl<'fragment> TypeFactCursor<'fragment> {
    pub(crate) fn new(payload: &'fragment [u8], schema: u16) -> Self {
        let remaining = payload
            .get(..4)
            .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            .unwrap_or(0);
        let computed = if schema >= 2 {
            payload
                .get(4..8)
                .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                .unwrap_or(0)
        } else {
            0
        };
        Self {
            payload,
            at: if schema >= 2 { 8 } else { 4 },
            remaining: remaining.saturating_add(computed),
            declared_remaining: remaining,
            schema,
            ordinal: 0,
        }
    }

    /// Opens the exact common child pool referenced by decoded type-record
    /// list spans.  The fragment validator has already proved its offset,
    /// cardinality, tags, and targets; this only establishes a borrowed lazy
    /// cursor and never reconstructs source syntax.
    pub fn children(&self) -> Result<TypeFactChildCursor<'fragment>, TypeFactFault> {
        let declared = self
            .payload
            .get(..4)
            .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            .unwrap_or(0);
        let computed = if self.schema >= 2 {
            self.payload
                .get(4..8)
                .map(|raw| u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
                .unwrap_or(0)
        } else {
            0
        };
        let count = TypeFactCounts::new(declared, computed).total()?;
        let mut reader = Reader {
            bytes: self.payload,
            at: if self.schema >= 2 { 8 } else { 4 },
            ordinal: 0,
        };
        for ordinal in 0..count {
            reader.ordinal = ordinal;
            let owner = EntityId::new(reader.u32()?);
            let _record = decode_record(&mut reader, owner)?;
        }
        let child_count = reader.u32()?;
        Ok(TypeFactChildCursor {
            payload: self.payload,
            at: reader.at,
            remaining: child_count,
            ordinal: 0,
        })
    }
}

/// Lazy borrowed traversal of the validated common type-child pool.
pub struct TypeFactChildCursor<'fragment> {
    payload: &'fragment [u8],
    at: usize,
    remaining: u32,
    ordinal: u32,
}

impl<'fragment> Iterator for TypeFactChildCursor<'fragment> {
    type Item = Result<DecodedTypeFactChild<'fragment>, TypeFactFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let ordinal = self.ordinal;
        self.ordinal += 1;
        let mut reader = Reader {
            bytes: self.payload,
            at: self.at,
            ordinal,
        };
        let child = decode_child(&mut reader, ordinal, 0);
        self.at = reader.at;
        Some(child.map(|child| DecodedTypeFactChild { ordinal, child }))
    }
}

impl<'fragment> Iterator for TypeFactCursor<'fragment> {
    type Item = Result<DecodedTypeFact<'fragment>, TypeFactFault>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let segment = if self.schema >= 2 && self.ordinal >= self.declared_remaining {
            TypeFactSegment::Computed
        } else {
            TypeFactSegment::Declared
        };
        let mut reader = Reader {
            bytes: self.payload,
            at: self.at,
            ordinal: self.ordinal,
        };
        self.ordinal += 1;
        let owner = match reader.u32() {
            Ok(value) => EntityId::new(value),
            Err(error) => return Some(Err(error)),
        };
        let result = decode_record(&mut reader, owner);
        self.at = reader.at;
        Some(result.map(|mut decoded| {
            decoded.segment = segment;
            decoded
        }))
    }
}
