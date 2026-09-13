//! Borrowed runtime-schema view of the canonical node wire grammar.

use crate::{
    CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, ID_BYTES, NodeError, SchemaIdentity,
};

use super::{DEFAULT_MAX_ENCODED_BYTES, NODE_MAGIC};

struct Reader<'a> {
    bytes: &'a [u8],
}
impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], NodeError> {
        let (head, tail) = self
            .bytes
            .split_at_checked(count)
            .ok_or(NodeError::MalformedEncoding)?;
        self.bytes = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, NodeError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(NodeError::MalformedEncoding)
    }
    fn u16(&mut self) -> Result<u16, NodeError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| NodeError::MalformedEncoding)?,
        ))
    }
    fn field(&mut self) -> Result<&'a [u8], NodeError> {
        let len = u64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| NodeError::MalformedEncoding)?,
        );
        self.take(usize::try_from(len).map_err(|_| NodeError::MalformedEncoding)?)
    }
    fn finish(self) -> Result<(), NodeError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(NodeError::MalformedEncoding)
        }
    }
}

/// A borrowed, runtime-schema checked canonical node view.
pub struct CanonicalNodeView<'a> {
    body: &'a [u8],
    level: u16,
    kind: u8,
    schema: SchemaIdentity,
    row_count: u64,
}

/// One borrowed canonical leaf key/value field pair.
pub type CanonicalLeafField<'a> = (&'a [u8], &'a [u8]);

impl<'a> CanonicalNodeView<'a> {
    /// Checks framing, schema, lengths, ordering, and bounded node shape.
    ///
    /// # Errors
    /// Returns a node admission error for a wrong schema, malformed field,
    /// trailing byte, oversized node, or invalid branch shape.
    pub fn parse(bytes: &'a [u8], schema: SchemaIdentity) -> Result<Self, NodeError> {
        if bytes.len() > DEFAULT_MAX_ENCODED_BYTES {
            return Err(NodeError::OversizedNode);
        }
        let mut reader = Reader::new(bytes);
        if reader.take(NODE_MAGIC.len())? != NODE_MAGIC
            || reader.byte()? != CANONICAL_TREE_ABI
            || reader.byte()? != CANONICAL_CUT_POLICY_VERSION
        {
            return Err(NodeError::SchemaMismatch);
        }
        let kind = reader.byte()?;
        let version = reader.byte()?;
        let domain = reader.byte()?;
        let ty = reader.u16()?;
        let level = reader.u16()?;
        if (domain, ty, version) != (schema.domain(), schema.ty(), schema.version()) {
            return Err(NodeError::SchemaMismatch);
        }
        if kind == 0x01 && level != 0 || kind == 0x02 && level == 0 {
            return Err(NodeError::LevelMismatch);
        }
        if kind != 0x01 && kind != 0x02 {
            return Err(NodeError::MalformedEncoding);
        }
        let body = reader.field()?;
        reader.finish()?;
        let mut body_reader = Reader::new(body);
        let mut previous = None;
        let mut count = 0usize;
        let mut row_count = 0u64;
        while !body_reader.bytes.is_empty() {
            count = count.checked_add(1).ok_or(NodeError::OversizedNode)?;
            if count > usize::from(crate::DEFAULT_CUT_POLICY.max_entries) {
                return Err(NodeError::OversizedNode);
            }
            let key = body_reader.field()?;
            if previous.is_some_and(|previous: &[u8]| previous >= key) {
                return Err(NodeError::UnsortedOrDuplicate);
            }
            previous = Some(key);
            if kind == 0x01 {
                let _ = body_reader.field()?;
                row_count = row_count.checked_add(1).ok_or(NodeError::OversizedNode)?;
            } else if body_reader.field()?.len() != ID_BYTES {
                return Err(NodeError::MalformedEncoding);
            } else {
                let count_bytes = body_reader.take(8)?;
                let child_count = u64::from_be_bytes(
                    count_bytes
                        .try_into()
                        .map_err(|_| NodeError::MalformedEncoding)?,
                );
                if child_count == 0 {
                    return Err(NodeError::MalformedEncoding);
                }
                row_count = row_count
                    .checked_add(child_count)
                    .ok_or(NodeError::OversizedNode)?;
            }
        }
        body_reader.finish()?;
        if kind == 0x02 && count == 0 {
            return Err(NodeError::InvalidBranch);
        }
        Ok(Self {
            body,
            level,
            kind,
            schema,
            row_count,
        })
    }

    /// Returns the runtime schema checked by this view.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        self.schema
    }
    /// Returns the canonical node level.
    #[must_use]
    pub const fn level(&self) -> u16 {
        self.level
    }
    /// Returns whether this view is a leaf.
    #[must_use]
    pub const fn is_leaf(&self) -> bool {
        self.kind == 0x01
    }

    /// Returns the authenticated number of logical rows below this node.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }
    /// Returns borrowed key/value fields from a leaf.
    ///
    /// # Errors
    /// Returns [`NodeError::InvalidBranch`] for a branch or a malformed field
    /// error if the admitted body cannot be traversed.
    pub fn leaf_fields(&self) -> Result<Vec<CanonicalLeafField<'a>>, NodeError> {
        if !self.is_leaf() {
            return Err(NodeError::InvalidBranch);
        }
        let mut reader = Reader::new(self.body);
        let mut fields = Vec::new();
        while !reader.bytes.is_empty() {
            fields.push((reader.field()?, reader.field()?));
        }
        reader.finish()?;
        Ok(fields)
    }
    /// Returns a borrowed iterator over branch anchors and raw commitments.
    ///
    /// # Errors
    /// Returns [`NodeError::InvalidBranch`] for a leaf.
    pub fn branch_children(&self) -> Result<CanonicalChildWireIter<'a>, NodeError> {
        if self.is_leaf() {
            return Err(NodeError::InvalidBranch);
        }
        Ok(CanonicalChildWireIter {
            reader: Reader::new(self.body),
            level: self.level.saturating_sub(1),
        })
    }
}

/// One borrowed branch child anchor and its exact 32-byte raw commitment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CanonicalChildWireRef<'a> {
    /// Borrowed canonical key anchor bytes.
    pub first_key: &'a [u8],
    /// Borrowed fixed-width child commitment bytes.
    pub commitment: &'a [u8; ID_BYTES],
    /// Child node level.
    pub level: u16,
    /// Authenticated number of rows covered by the child subtree.
    pub row_count: u64,
}

/// Borrowed iterator over canonical branch child references.
pub struct CanonicalChildWireIter<'a> {
    reader: Reader<'a>,
    level: u16,
}
impl<'a> Iterator for CanonicalChildWireIter<'a> {
    type Item = Result<CanonicalChildWireRef<'a>, NodeError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.reader.bytes.is_empty() {
            return None;
        }
        Some((|| {
            let first_key = self.reader.field()?;
            let commitment = self
                .reader
                .field()?
                .try_into()
                .map_err(|_| NodeError::MalformedEncoding)?;
            let row_count = u64::from_be_bytes(
                self.reader
                    .take(8)?
                    .try_into()
                    .map_err(|_| NodeError::MalformedEncoding)?,
            );
            if row_count == 0 {
                return Err(NodeError::MalformedEncoding);
            }
            Ok(CanonicalChildWireRef {
                first_key,
                commitment,
                level: self.level,
                row_count,
            })
        })())
    }
}
