use crate::{
    CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, CanonicalRelation, ID_BYTES,
    IdAdmissionError, StateRoot, UntrustedId,
};
use core::marker::PhantomData;

use super::{
    NODE_MAGIC,
    build::node_from_bytes,
    cut::DEFAULT_CUT_POLICY,
    node::{
        CanonicalRootAdmissionError, CheckedCanonicalRoot, ChildCommitment, CommittedChild,
        NodeError,
    },
};

struct NodeReader<'a> {
    remaining: &'a [u8],
}

impl<'a> NodeReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], NodeError> {
        if self.remaining.len() < length {
            return Err(NodeError::MalformedEncoding);
        }
        let (head, tail) = self.remaining.split_at(length);
        self.remaining = tail;
        Ok(head)
    }

    fn byte(&mut self) -> Result<u8, NodeError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(NodeError::MalformedEncoding)
    }

    fn u16(&mut self) -> Result<u16, NodeError> {
        let bytes = self.take(2)?;
        let mut value = [0; 2];
        value.copy_from_slice(bytes);
        Ok(u16::from_be_bytes(value))
    }

    fn field(&mut self) -> Result<&'a [u8], NodeError> {
        let bytes = self.take(8)?;
        let mut length = [0; 8];
        length.copy_from_slice(bytes);
        let length = usize::try_from(u64::from_be_bytes(length))
            .map_err(|_| NodeError::MalformedEncoding)?;
        self.take(length)
    }

    fn finish(self) -> Result<(), NodeError> {
        if self.remaining.is_empty() {
            Ok(())
        } else {
            Err(NodeError::MalformedEncoding)
        }
    }
}

fn decode_key<R: CanonicalRelation>(bytes: &[u8]) -> Result<R::Key, NodeError> {
    let key = R::decode_key(bytes).map_err(NodeError::RelationDecode)?;
    let mut encoded = Vec::new();
    R::encode_key(&key, &mut encoded);
    if encoded == bytes {
        Ok(key)
    } else {
        Err(NodeError::NonCanonicalEncoding)
    }
}

fn decode_value<R: CanonicalRelation>(bytes: &[u8]) -> Result<R::Value, NodeError> {
    let value = R::decode_value(bytes).map_err(NodeError::RelationDecode)?;
    let mut encoded = Vec::new();
    R::encode_value(&value, &mut encoded);
    if encoded == bytes {
        Ok(value)
    } else {
        Err(NodeError::NonCanonicalEncoding)
    }
}

fn admit_leaf_body<R: CanonicalRelation>(body: &[u8]) -> Result<(Option<R::Key>, u64), NodeError> {
    let mut reader = NodeReader::new(body);
    let mut first_key = None;
    let mut previous = None;
    let mut count = 0usize;
    while !reader.remaining.is_empty() {
        count = count.checked_add(1).ok_or(NodeError::OversizedNode)?;
        if count > usize::from(DEFAULT_CUT_POLICY.max_entries) {
            return Err(NodeError::OversizedNode);
        }
        let key = decode_key::<R>(reader.field()?)?;
        let _value = decode_value::<R>(reader.field()?)?;
        if previous.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(NodeError::UnsortedOrDuplicate);
        }
        if first_key.is_none() {
            first_key = Some(key.clone());
        }
        previous = Some(key);
    }
    reader.finish()?;
    Ok((
        first_key,
        u64::try_from(count).map_err(|_| NodeError::OversizedNode)?,
    ))
}

fn admit_branch_body<R: CanonicalRelation>(
    body: &[u8],
) -> Result<(Option<R::Key>, u64), NodeError> {
    let mut reader = NodeReader::new(body);
    let mut first_key = None;
    let mut previous = None;
    let mut count = 0usize;
    let mut total = 0u64;
    while !reader.remaining.is_empty() {
        count = count.checked_add(1).ok_or(NodeError::OversizedNode)?;
        if count > usize::from(DEFAULT_CUT_POLICY.max_entries) {
            return Err(NodeError::OversizedNode);
        }
        let key = decode_key::<R>(reader.field()?)?;
        if reader.field()?.len() != ID_BYTES {
            return Err(NodeError::MalformedEncoding);
        }
        let count_bytes = reader.take(8)?;
        let row_count = u64::from_be_bytes(
            count_bytes
                .try_into()
                .map_err(|_| NodeError::MalformedEncoding)?,
        );
        if row_count == 0 {
            return Err(NodeError::MalformedEncoding);
        }
        if previous.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(NodeError::UnsortedOrDuplicate);
        }
        if first_key.is_none() {
            first_key = Some(key.clone());
        }
        previous = Some(key);
        total = total
            .checked_add(row_count)
            .ok_or(NodeError::OversizedNode)?;
    }
    reader.finish()?;
    if first_key.is_none() {
        return Err(NodeError::InvalidBranch);
    }
    Ok((first_key, total))
}

/// Admits one bounded canonical relation node from wire bytes.
///
/// Admission validates the fixed header, relation schema, length-delimited
/// fields, canonical key/value round trips, strict key ordering, branch
/// anchor ordering, child commitment widths, node kind/level, and the hard
/// encoded-byte/entry bounds. Branch children are immutable references and
/// are validated separately when their object records are admitted.
///
/// # Errors
///
/// Returns [`NodeError::MalformedEncoding`] for truncation, trailing bytes,
/// invalid lengths, or an invalid child commitment field;
/// [`NodeError::SchemaMismatch`] for a wrong canonical ABI, cut policy, or
/// relation schema; and the remaining [`NodeError`] variants for ordering,
/// decoding, or bound failures.
pub fn admit_canonical_root<R: CanonicalRelation>(
    bytes: &[u8],
) -> Result<CheckedCanonicalRoot<R>, NodeError> {
    if bytes.len() > DEFAULT_CUT_POLICY.max_encoded_bytes() {
        return Err(NodeError::OversizedNode);
    }
    let mut reader = NodeReader::new(bytes);
    if reader.take(NODE_MAGIC.len())? != NODE_MAGIC {
        return Err(NodeError::SchemaMismatch);
    }
    if reader.byte()? != CANONICAL_TREE_ABI || reader.byte()? != CANONICAL_CUT_POLICY_VERSION {
        return Err(NodeError::SchemaMismatch);
    }
    let kind = reader.byte()?;
    if reader.byte()? != R::VERSION || reader.byte()? != R::DOMAIN || reader.u16()? != R::TYPE {
        return Err(NodeError::SchemaMismatch);
    }
    let level = reader.u16()?;
    let body = reader.field()?;
    reader.finish()?;

    let (first_key, row_count) = match kind {
        0x01 => {
            if level != 0 {
                return Err(NodeError::LevelMismatch);
            }
            admit_leaf_body::<R>(body)?
        }
        0x02 => {
            if level == 0 {
                return Err(NodeError::LevelMismatch);
            }
            admit_branch_body::<R>(body)?
        }
        _ => return Err(NodeError::MalformedEncoding),
    };

    let node = node_from_bytes::<R>(bytes.to_vec(), first_key, level, row_count);
    Ok(CheckedCanonicalRoot::from_parts(node.commitment(), node))
}

/// Admits canonical node bytes and binds them to an untrusted typed root
/// claim in one operation.
///
/// This is the preferred storage/replication boundary: the caller supplies
/// the relation context and claimed digest received from the wire, while this
/// function performs grammar admission before accepting the identity.  The
/// returned evidence owns the validated node and cannot be paired with a
/// different root by the caller.
///
/// # Errors
///
/// Returns [`CanonicalRootAdmissionError::Node`] when canonical bytes fail
/// grammar admission, or [`CanonicalRootAdmissionError::Identity`] when the
/// wire context or claimed digest does not match the validated node.
pub fn admit_canonical_root_claim<R: CanonicalRelation>(
    claim: UntrustedId<R>,
    bytes: &[u8],
) -> Result<CheckedCanonicalRoot<R>, CanonicalRootAdmissionError> {
    let admitted = admit_canonical_root::<R>(bytes).map_err(CanonicalRootAdmissionError::Node)?;
    let expected = StateRoot::<R>::admit_canonical_bytes(claim, bytes)
        .map_err(CanonicalRootAdmissionError::Identity)?;
    if expected != admitted.root() {
        return Err(CanonicalRootAdmissionError::Identity(
            IdAdmissionError::DigestMismatch,
        ));
    }
    Ok(admitted)
}

impl<R: CanonicalRelation> CheckedCanonicalRoot<R> {
    /// Returns a borrowed iterator over authenticated branch child summaries.
    ///
    /// The iterator retains the canonical node bytes by reference and decodes
    /// one anchor at a time, allowing storage adapters to inspect a branch
    /// without allocating the complete child list.
    ///
    /// # Errors
    /// Returns a grammar error if the admitted bytes cannot be traversed as a
    /// canonical node.
    pub fn child_iter(&self) -> Result<CanonicalChildIter<'_, R>, NodeError> {
        if self.node().level() == 0 {
            return Ok(CanonicalChildIter {
                reader: NodeReader::new(&[]),
                child_level: 0,
                marker: PhantomData,
            });
        }
        let mut reader = NodeReader::new(self.bytes());
        if reader.take(NODE_MAGIC.len())? != NODE_MAGIC
            || reader.byte()? != CANONICAL_TREE_ABI
            || reader.byte()? != CANONICAL_CUT_POLICY_VERSION
            || reader.byte()? != 0x02
        {
            return Err(NodeError::SchemaMismatch);
        }
        let _version = reader.byte()?;
        let _domain = reader.byte()?;
        let _type = reader.u16()?;
        let level = reader.u16()?;
        let body = reader.field()?;
        reader.finish()?;
        Ok(CanonicalChildIter {
            reader: NodeReader::new(body),
            child_level: level.checked_sub(1).ok_or(NodeError::LevelMismatch)?,
            marker: PhantomData,
        })
    }

    /// Decodes the entries of an admitted leaf without exposing its wire
    /// parser or accepting an unchecked byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::InvalidBranch`] for a branch and a grammar error
    /// if the admitted bytes cannot be traversed as a leaf.
    #[allow(
        clippy::type_complexity,
        reason = "the borrowed canonical row pair is the relation's public shape"
    )]
    pub fn leaf_entries(&self) -> Result<Vec<(R::Key, R::Value)>, NodeError> {
        if self.node().level() != 0 {
            return Err(NodeError::InvalidBranch);
        }
        let mut reader = NodeReader::new(self.bytes());
        if reader.take(NODE_MAGIC.len())? != NODE_MAGIC {
            return Err(NodeError::SchemaMismatch);
        }
        if reader.byte()? != CANONICAL_TREE_ABI
            || reader.byte()? != CANONICAL_CUT_POLICY_VERSION
            || reader.byte()? != 0x01
        {
            return Err(NodeError::SchemaMismatch);
        }
        let _version = reader.byte()?;
        let _domain = reader.byte()?;
        let _type = reader.u16()?;
        let level = reader.u16()?;
        if level != 0 {
            return Err(NodeError::LevelMismatch);
        }
        let body = reader.field()?;
        reader.finish()?;
        let mut body_reader = NodeReader::new(body);
        let mut entries = Vec::new();
        while !body_reader.remaining.is_empty() {
            let key = decode_key::<R>(body_reader.field()?)?;
            let value = decode_value::<R>(body_reader.field()?)?;
            entries.push((key, value));
        }
        body_reader.finish()?;
        Ok(entries)
    }

    /// Returns authenticated child summaries from an admitted branch.
    ///
    /// The summaries are sufficient for a lazy path walk: a caller can ask a
    /// [`TreeNodeLoader`](crate::TreeNodeLoader) for only the selected child,
    /// while retaining the other commitments as proof boundaries.  A leaf
    /// returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns [`NodeError::MalformedEncoding`] if the admitted node cannot be
    /// decoded into its already validated branch grammar.
    pub fn child_summaries(&self) -> Result<Vec<CommittedChild<R>>, NodeError> {
        if self.node().level() == 0 {
            return Ok(Vec::new());
        }
        let mut reader = NodeReader::new(self.bytes());
        if reader.take(NODE_MAGIC.len())? != NODE_MAGIC {
            return Err(NodeError::SchemaMismatch);
        }
        if reader.byte()? != CANONICAL_TREE_ABI
            || reader.byte()? != CANONICAL_CUT_POLICY_VERSION
            || reader.byte()? != 0x02
        {
            return Err(NodeError::SchemaMismatch);
        }
        let _version = reader.byte()?;
        let _domain = reader.byte()?;
        let _type = reader.u16()?;
        let level = reader.u16()?;
        let body = reader.field()?;
        reader.finish()?;
        let child_level = level.checked_sub(1).ok_or(NodeError::LevelMismatch)?;
        let mut body_reader = NodeReader::new(body);
        let mut children = Vec::new();
        while !body_reader.remaining.is_empty() {
            let first_key = decode_key::<R>(body_reader.field()?)?;
            let bytes = body_reader.field()?;
            let bytes: [u8; ID_BYTES] =
                bytes.try_into().map_err(|_| NodeError::MalformedEncoding)?;
            let count_bytes = body_reader.take(8)?;
            let row_count = u64::from_be_bytes(
                count_bytes
                    .try_into()
                    .map_err(|_| NodeError::MalformedEncoding)?,
            );
            if row_count == 0 {
                return Err(NodeError::MalformedEncoding);
            }
            children.push(CommittedChild {
                first_key,
                commitment: ChildCommitment::from_bytes(&bytes)?,
                level: child_level,
                row_count,
            });
        }
        body_reader.finish()?;
        Ok(children)
    }
}

/// Borrowed iterator over one admitted branch's authenticated children.
pub struct CanonicalChildIter<'a, R: CanonicalRelation> {
    reader: NodeReader<'a>,
    child_level: u16,
    marker: PhantomData<R>,
}

impl<R: CanonicalRelation> Iterator for CanonicalChildIter<'_, R> {
    type Item = Result<CommittedChild<R>, NodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.reader.remaining.is_empty() {
            return None;
        }
        let item = (|| {
            let first_key = decode_key::<R>(self.reader.field()?)?;
            let bytes: [u8; ID_BYTES] = self
                .reader
                .field()?
                .try_into()
                .map_err(|_| NodeError::MalformedEncoding)?;
            let count_bytes = self.reader.take(8)?;
            let row_count = u64::from_be_bytes(
                count_bytes
                    .try_into()
                    .map_err(|_| NodeError::MalformedEncoding)?,
            );
            if row_count == 0 {
                return Err(NodeError::MalformedEncoding);
            }
            Ok(CommittedChild {
                first_key,
                commitment: ChildCommitment::from_bytes(&bytes)?,
                level: self.child_level,
                row_count,
            })
        })();
        Some(item)
    }
}
