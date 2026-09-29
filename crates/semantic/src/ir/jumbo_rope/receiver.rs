//! Proof admission, resumable closure, and verified reassembly.
use alloc::vec::Vec;
use core::ops::Range;
use std::io::Write;

use super::descriptor::descriptor_identity;
use super::wire::{RopeObjectKind, RopeObjectRef, leaf_identity};
use super::*;

/// Complete verified descriptor token. This type can only be minted by a
/// successful local write or a complete receiver closure check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedJumboRope {
    pub(super) descriptor: CheckedJumboValueDescriptor,
}

impl VerifiedJumboRope {
    /// Descriptor whose complete object closure was verified.
    #[must_use]
    pub const fn descriptor(&self) -> &CheckedJumboValueDescriptor {
        &self.descriptor
    }

    /// Content identity of the typed descriptor.
    #[must_use]
    pub fn descriptor_id(&self) -> JumboValueDescriptorId {
        self.descriptor.id()
    }

    /// Reads the complete ordered value into a caller-owned writer while
    /// checking every stored leaf again. Scratch remains bounded to one leaf.
    pub fn write_value_to<S, W>(
        &self,
        source: &mut S,
        writer: &mut W,
    ) -> Result<u64, JumboOperationError<S::Error>>
    where
        S: JumboRopeObjectSource + ?Sized,
        W: Write,
    {
        stream_verified_value(&self.descriptor, source, writer)
    }
}

/// Leaf bytes whose ordinal, byte offset, content ID, and rope path have been
/// checked against one exact descriptor.
pub struct CheckedJumboLeaf<'bytes> {
    descriptor_id: JumboValueDescriptorId,
    id: JumboRopeObjectId,
    ordinal: u64,
    byte_offset: u64,
    bytes: &'bytes [u8],
    path_nodes: [Option<JumboRopeNode>; MAX_PROOF_DEPTH],
    path_node_count: usize,
}

impl<'bytes> CheckedJumboLeaf<'bytes> {
    /// Content identity of the checked leaf bytes.
    #[must_use]
    pub const fn id(&self) -> JumboRopeObjectId {
        self.id
    }

    /// Authenticated leaf ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }

    /// Authenticated first byte offset in the complete value.
    #[must_use]
    pub const fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    /// Canonical bytes borrowed for this admission call only.
    #[must_use]
    pub const fn bytes(&self) -> &'bytes [u8] {
        self.bytes
    }
}

/// Resumable receiver for one checked descriptor. It records only bounded
/// per-leaf receipts and never exposes a complete token while leaves are
/// missing.
pub struct JumboRopeClosure {
    descriptor: CheckedJumboValueDescriptor,
    slots: Vec<Option<LeafReceipt>>,
    present_count: u64,
    missing_count: u64,
}

impl JumboRopeClosure {
    /// Checks and reserves bounded receipt metadata for a remote descriptor.
    pub fn new(
        descriptor: UntrustedJumboValueDescriptor,
        limits: JumboRopeLimits,
    ) -> Result<Self, JumboRopeError> {
        let limits = limits.validate()?;
        let descriptor = descriptor.check(limits)?;
        let leaf_count =
            usize::try_from(descriptor.leaf_count).map_err(|_| JumboRopeError::LengthOverflow)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(leaf_count)
            .map_err(|_| JumboRopeError::Allocation)?;
        let allocated_metadata = slots
            .capacity()
            .checked_mul(size_of::<Option<LeafReceipt>>())
            .ok_or(JumboRopeError::LengthOverflow)?;
        if allocated_metadata > limits.max_metadata_bytes {
            return Err(JumboRopeError::MetadataTooLarge {
                observed: allocated_metadata,
                maximum: limits.max_metadata_bytes,
            });
        }
        slots.resize(leaf_count, None);
        let missing_count = descriptor.leaf_count;
        Ok(Self {
            descriptor,
            slots,
            present_count: 0,
            missing_count,
        })
    }

    /// Structurally checked descriptor for this transfer.
    #[must_use]
    pub const fn descriptor(&self) -> &CheckedJumboValueDescriptor {
        &self.descriptor
    }

    /// Number of authenticated leaf objects admitted so far.
    #[must_use]
    pub const fn present_leaf_count(&self) -> u64 {
        self.present_count
    }

    /// Number of leaf ordinals still missing.
    #[must_use]
    pub const fn missing_leaf_count(&self) -> u64 {
        self.missing_count
    }

    /// Ordered missing ordinal ranges suitable for resumable range grants.
    #[must_use]
    pub fn missing_ranges(&self) -> MissingJumboLeafRanges<'_> {
        MissingJumboLeafRanges {
            slots: &self.slots,
            next: 0,
        }
    }

    /// Verifies one borrowed leaf against its content hash and complete rope
    /// path. The returned value is leaf-checked only; the whole descriptor is
    /// still unpublished until admission and final closure verification.
    pub fn check_leaf<'bytes>(
        &self,
        ordinal: u64,
        bytes: &'bytes [u8],
        proof: &JumboRopeProof,
    ) -> Result<CheckedJumboLeaf<'bytes>, JumboRopeError> {
        let index = usize::try_from(ordinal).map_err(|_| JumboRopeError::LengthOverflow)?;
        if self.slots.get(index).is_none() {
            return Err(JumboRopeError::LeafOrdinalOutOfRange {
                ordinal,
                leaf_count: self.descriptor.leaf_count,
            });
        }
        validate_leaf_length(ordinal, self.descriptor.leaf_count, bytes.len())?;
        if proof.siblings().len() > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: proof.siblings().len(),
                maximum: MAX_PROOF_DEPTH,
            });
        }
        let byte_length = u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?;
        let id = JumboRopeObjectId(leaf_identity(bytes));
        let mut current = RopeObjectRef {
            kind: RopeObjectKind::Leaf,
            id,
            first_leaf: ordinal,
            leaf_count: 1,
            byte_length,
        };
        let mut byte_offset = 0_u64;
        let mut path_nodes = [None; MAX_PROOF_DEPTH];
        for (path_index, step) in proof.siblings().iter().copied().enumerate() {
            let sibling = RopeObjectRef::from_claim(step.sibling());
            validate_ref(sibling)?;
            let node = match step.side() {
                JumboRopeProofSide::Left => {
                    let sibling_end = sibling
                        .first_leaf
                        .checked_add(sibling.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    let current_end = current
                        .first_leaf
                        .checked_add(current.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    if sibling_end != current.first_leaf
                        || ordinal < sibling.first_leaf
                        || ordinal < current.first_leaf
                        || ordinal >= current_end
                    {
                        return Err(JumboRopeError::NonContiguousRopeRange);
                    }
                    byte_offset = byte_offset
                        .checked_add(sibling.byte_length)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    JumboRopeNode::create(sibling, current)?
                }
                JumboRopeProofSide::Right => {
                    let current_end = current
                        .first_leaf
                        .checked_add(current.leaf_count)
                        .ok_or(JumboRopeError::LengthOverflow)?;
                    if current_end != sibling.first_leaf
                        || ordinal < current.first_leaf
                        || ordinal >= current_end
                    {
                        return Err(JumboRopeError::NonContiguousRopeRange);
                    }
                    JumboRopeNode::create(current, sibling)?
                }
            };
            current = node.as_ref();
            path_nodes[path_index] = Some(node);
        }
        if current != self.descriptor.root_ref()? {
            return Err(JumboRopeError::ProofRootMismatch);
        }
        Ok(CheckedJumboLeaf {
            descriptor_id: self.descriptor.id(),
            id,
            ordinal,
            byte_offset,
            bytes,
            path_nodes,
            path_node_count: proof.siblings().len(),
        })
    }

    /// Persists one checked leaf and its authenticated parent path. An exact
    /// retry is idempotent; a conflicting receipt for the same ordinal fails.
    pub fn admit_leaf<S: JumboRopeObjectSink + ?Sized>(
        &mut self,
        leaf: CheckedJumboLeaf<'_>,
        sink: &mut S,
    ) -> Result<(), JumboOperationError<S::Error>> {
        if leaf.descriptor_id != self.descriptor.id() {
            return Err(JumboRopeError::LeafDescriptorMismatch.into());
        }
        let index = usize::try_from(leaf.ordinal).map_err(|_| JumboRopeError::LengthOverflow)?;
        let Some(slot) = self.slots.get(index) else {
            return Err(JumboRopeError::LeafOrdinalOutOfRange {
                ordinal: leaf.ordinal,
                leaf_count: self.descriptor.leaf_count,
            }
            .into());
        };
        let receipt = LeafReceipt {
            id: leaf.id,
            byte_offset: leaf.byte_offset,
            byte_length: u64::try_from(leaf.bytes.len())
                .map_err(|_| JumboRopeError::LengthOverflow)?,
        };
        if let Some(previous) = slot {
            return if *previous == receipt {
                Ok(())
            } else {
                Err(JumboRopeError::ConflictingDuplicateLeaf.into())
            };
        }
        sink.write_leaf(JumboRopeLeafRef {
            id: leaf.id,
            ordinal: leaf.ordinal,
            byte_offset: leaf.byte_offset,
            bytes: leaf.bytes,
        })
        .map_err(JumboOperationError::Store)?;
        for node in leaf.path_nodes[..leaf.path_node_count].iter().flatten() {
            sink.write_interior(node)
                .map_err(JumboOperationError::Store)?;
        }
        self.slots[index] = Some(receipt);
        self.present_count += 1;
        self.missing_count -= 1;
        Ok(())
    }

    /// Verifies every ordinal, byte range, stored node, stored leaf, and
    /// complete UTF-8 value before minting the publication token.
    pub fn finish<S: JumboRopeObjectSource + ?Sized>(
        &self,
        source: &mut S,
    ) -> Result<VerifiedJumboRope, JumboOperationError<S::Error>> {
        if self.missing_count != 0 {
            return Err(JumboRopeError::MissingLeaves {
                missing: self.missing_count,
            }
            .into());
        }
        verify_complete_closure(&self.descriptor, &self.slots, source)?;
        Ok(VerifiedJumboRope {
            descriptor: self.descriptor,
        })
    }
}

/// Iterator over maximal adjacent missing leaf ranges.
pub struct MissingJumboLeafRanges<'closure> {
    slots: &'closure [Option<LeafReceipt>],
    next: usize,
}

impl Iterator for MissingJumboLeafRanges<'_> {
    type Item = Range<u64>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.slots.len() && self.slots[self.next].is_some() {
            self.next += 1;
        }
        if self.next == self.slots.len() {
            return None;
        }
        let start = self.next;
        while self.next < self.slots.len() && self.slots[self.next].is_none() {
            self.next += 1;
        }
        Some(start as u64..self.next as u64)
    }
}

/// Builds an authenticated inclusion path for one leaf from stored interior
/// objects. Every traversed node is rehashed and checked against its parent.
pub fn prove_jumbo_leaf<S: JumboRopeObjectSource + ?Sized>(
    descriptor: &CheckedJumboValueDescriptor,
    ordinal: u64,
    source: &mut S,
) -> Result<JumboRopeProof, JumboOperationError<S::Error>> {
    if ordinal >= descriptor.leaf_count {
        return Err(JumboRopeError::LeafOrdinalOutOfRange {
            ordinal,
            leaf_count: descriptor.leaf_count,
        }
        .into());
    }
    let mut current = descriptor.root_ref()?;
    let mut siblings = Vec::new();
    siblings
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    while current.kind == RopeObjectKind::Interior {
        if siblings.len() == MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: siblings.len() + 1,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        let bytes = source
            .read_interior(current.id)
            .map_err(JumboOperationError::Store)?
            .ok_or(JumboRopeError::MissingStoredInterior)?;
        let node = JumboRopeNode::decode_wire(&bytes)?;
        if node.id() != current.id || node.as_ref() != current {
            return Err(JumboRopeError::InteriorObjectCorrupt.into());
        }
        let target_end = ordinal
            .checked_add(1)
            .ok_or(JumboRopeError::LengthOverflow)?;
        let left_end = node
            .left
            .first_leaf
            .checked_add(node.left.leaf_count)
            .ok_or(JumboRopeError::LengthOverflow)?;
        if ordinal >= node.left.first_leaf && target_end <= left_end {
            siblings.push(JumboRopeProofSibling::new(
                JumboRopeProofSide::Right,
                node.right.to_claim(),
            ));
            current = node.left;
        } else if ordinal >= node.right.first_leaf
            && target_end
                <= node
                    .right
                    .first_leaf
                    .checked_add(node.right.leaf_count)
                    .ok_or(JumboRopeError::LengthOverflow)?
        {
            siblings.push(JumboRopeProofSibling::new(
                JumboRopeProofSide::Left,
                node.left.to_claim(),
            ));
            current = node.right;
        } else {
            return Err(JumboRopeError::InteriorObjectCorrupt.into());
        }
    }
    if current.first_leaf != ordinal || current.leaf_count != 1 {
        return Err(JumboRopeError::InteriorObjectCorrupt.into());
    }
    siblings.reverse();
    JumboRopeProof::new(siblings).map_err(JumboOperationError::Rope)
}

fn verify_complete_closure<S: JumboRopeObjectSource + ?Sized>(
    descriptor: &CheckedJumboValueDescriptor,
    slots: &[Option<LeafReceipt>],
    source: &mut S,
) -> Result<(), JumboOperationError<S::Error>> {
    if descriptor.leaf_count == 0 {
        if descriptor.byte_length == 0 && descriptor.root == empty_rope_root() {
            return Ok(());
        }
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    let root = descriptor.root_ref()?;
    let mut stack = Vec::new();
    stack
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    stack.push((root, 0_usize));
    let mut next_leaf = 0_u64;
    let mut byte_offset = 0_u64;
    let mut node_count = 0_u64;
    let mut utf8 = Utf8Validator::default();
    let mut leaf_buffer = [0_u8; JUMBO_ROPE_MAX_LEAF_BYTES];
    while let Some((reference, depth)) = stack.pop() {
        if depth > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: depth,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        match reference.kind {
            RopeObjectKind::Leaf => {
                if reference.first_leaf != next_leaf || reference.leaf_count != 1 {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let index =
                    usize::try_from(next_leaf).map_err(|_| JumboRopeError::LengthOverflow)?;
                let receipt = slots
                    .get(index)
                    .and_then(|slot| *slot)
                    .ok_or(JumboRopeError::ClosureCensusMismatch)?;
                if receipt.id != reference.id
                    || receipt.byte_offset != byte_offset
                    || receipt.byte_length != reference.byte_length
                {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let byte_count = source
                    .read_leaf(reference.id, &mut leaf_buffer)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredLeaf)?;
                let bytes = leaf_buffer
                    .get(..byte_count)
                    .ok_or(JumboRopeError::OversizedStoredLeaf)?;
                validate_leaf_length(next_leaf, descriptor.leaf_count, bytes.len())?;
                if u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?
                    != reference.byte_length
                    || leaf_identity(bytes) != reference.id.0
                {
                    return Err(JumboRopeError::LeafObjectCorrupt.into());
                }
                if descriptor.encoding == JumboValueEncoding::Utf8 {
                    for byte in bytes.iter().copied() {
                        utf8.push(byte)?;
                    }
                }
                next_leaf = next_leaf
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                byte_offset = byte_offset
                    .checked_add(reference.byte_length)
                    .ok_or(JumboRopeError::LengthOverflow)?;
            }
            RopeObjectKind::Interior => {
                let bytes = source
                    .read_interior(reference.id)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredInterior)?;
                let node = JumboRopeNode::decode_wire(&bytes)?;
                if node.id() != reference.id || node.as_ref() != reference {
                    return Err(JumboRopeError::InteriorObjectCorrupt.into());
                }
                node_count = node_count
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                stack.push((node.right, depth + 1));
                stack.push((node.left, depth + 1));
            }
        }
    }
    if next_leaf != descriptor.leaf_count
        || byte_offset != descriptor.byte_length
        || node_count != descriptor.leaf_count.saturating_sub(1)
    {
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    if descriptor.encoding == JumboValueEncoding::Utf8 {
        utf8.finish()?;
    }
    Ok(())
}

fn stream_verified_value<S, W>(
    descriptor: &CheckedJumboValueDescriptor,
    source: &mut S,
    writer: &mut W,
) -> Result<u64, JumboOperationError<S::Error>>
where
    S: JumboRopeObjectSource + ?Sized,
    W: Write,
{
    if descriptor.leaf_count == 0 {
        return Ok(0);
    }
    let mut stack = Vec::new();
    stack
        .try_reserve(MAX_PROOF_DEPTH)
        .map_err(|_| JumboRopeError::Allocation)?;
    stack.push((descriptor.root_ref()?, 0_usize));
    let mut next_leaf = 0_u64;
    let mut byte_offset = 0_u64;
    let mut utf8 = Utf8Validator::default();
    let mut leaf_buffer = [0_u8; JUMBO_ROPE_MAX_LEAF_BYTES];
    while let Some((reference, depth)) = stack.pop() {
        if depth > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::ProofTooDeep {
                observed: depth,
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        match reference.kind {
            RopeObjectKind::Leaf => {
                if reference.first_leaf != next_leaf || reference.leaf_count != 1 {
                    return Err(JumboRopeError::ClosureCensusMismatch.into());
                }
                let byte_count = source
                    .read_leaf(reference.id, &mut leaf_buffer)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredLeaf)?;
                let bytes = leaf_buffer
                    .get(..byte_count)
                    .ok_or(JumboRopeError::OversizedStoredLeaf)?;
                validate_leaf_length(next_leaf, descriptor.leaf_count, bytes.len())?;
                if u64::try_from(bytes.len()).map_err(|_| JumboRopeError::LengthOverflow)?
                    != reference.byte_length
                    || leaf_identity(bytes) != reference.id.0
                {
                    return Err(JumboRopeError::LeafObjectCorrupt.into());
                }
                if descriptor.encoding == JumboValueEncoding::Utf8 {
                    for byte in bytes.iter().copied() {
                        utf8.push(byte)?;
                    }
                }
                writer
                    .write_all(&bytes)
                    .map_err(JumboOperationError::Output)?;
                next_leaf = next_leaf
                    .checked_add(1)
                    .ok_or(JumboRopeError::LengthOverflow)?;
                byte_offset = byte_offset
                    .checked_add(reference.byte_length)
                    .ok_or(JumboRopeError::LengthOverflow)?;
            }
            RopeObjectKind::Interior => {
                let bytes = source
                    .read_interior(reference.id)
                    .map_err(JumboOperationError::Store)?
                    .ok_or(JumboRopeError::MissingStoredInterior)?;
                let node = JumboRopeNode::decode_wire(&bytes)?;
                if node.id() != reference.id || node.as_ref() != reference {
                    return Err(JumboRopeError::InteriorObjectCorrupt.into());
                }
                stack.push((node.right, depth + 1));
                stack.push((node.left, depth + 1));
            }
        }
    }
    if next_leaf != descriptor.leaf_count || byte_offset != descriptor.byte_length {
        return Err(JumboRopeError::ClosureCensusMismatch.into());
    }
    if descriptor.encoding == JumboValueEncoding::Utf8 {
        utf8.finish()?;
    }
    Ok(byte_offset)
}
