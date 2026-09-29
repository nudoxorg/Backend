//! Bounded streaming chunk writer and build accounting.
use alloc::vec::Vec;
use core::mem::size_of;
use std::io::Read;

use super::receiver::VerifiedJumboRope;
use super::wire::{RopeObjectKind, RopeObjectRef};
use super::*;

const CUT_MASK: u64 = (1 << 17) - 1;
const GEAR_WINDOW_BYTES: usize = 64;
const GEAR_ROLLING_BASE: u64 = 257;
const GEAR_ROLLING_POWER: u64 = 257_u64.wrapping_pow(GEAR_WINDOW_BYTES as u32);

/// Work counters from one streaming rope write.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct JumboRopeBuildMetrics {
    input_bytes: u64,
    leaf_count: u64,
    interior_node_count: u64,
    payload_hash_bytes: u64,
    chunk_scratch_capacity_bytes: u64,
    peak_live_scratch_bytes: u64,
    peak_frontier_entries: u64,
    input_buffer_bytes: u64,
}

impl JumboRopeBuildMetrics {
    /// Exact bytes read from the borrowed slice or input stream.
    #[must_use]
    pub const fn input_bytes(self) -> u64 {
        self.input_bytes
    }

    /// Number of content-addressed leaf objects emitted.
    #[must_use]
    pub const fn leaf_count(self) -> u64 {
        self.leaf_count
    }

    /// Number of authenticated interior objects emitted.
    #[must_use]
    pub const fn interior_node_count(self) -> u64 {
        self.interior_node_count
    }

    /// Exact source bytes passed through leaf hashing.
    #[must_use]
    pub const fn payload_hash_bytes(self) -> u64 {
        self.payload_hash_bytes
    }

    /// Capacity of the one reused bounded leaf buffer.
    #[must_use]
    pub const fn chunk_scratch_capacity_bytes(self) -> u64 {
        self.chunk_scratch_capacity_bytes
    }

    /// Peak internal scratch estimated from the live writer state, reserved
    /// leaf/frontier capacities, and (for reader writes) fixed input buffer.
    /// Caller-owned input and sink-owned storage are excluded.
    #[must_use]
    pub const fn peak_live_scratch_bytes(self) -> u64 {
        self.peak_live_scratch_bytes
    }

    /// Largest number of pending Merkle frontier spans retained at once.
    #[must_use]
    pub const fn peak_frontier_entries(self) -> u64 {
        self.peak_frontier_entries
    }

    /// Fixed reader buffer charged by the stream-based writer.
    #[must_use]
    pub const fn input_buffer_bytes(self) -> u64 {
        self.input_buffer_bytes
    }
}

/// Complete local write result. The descriptor token is produced only after
/// every leaf and interior write succeeded and the exact input was checked.
#[derive(Clone, Debug)]
pub struct JumboRopeWriteReceipt {
    verified: VerifiedJumboRope,
    metrics: JumboRopeBuildMetrics,
}

impl JumboRopeWriteReceipt {
    /// Complete verified value descriptor.
    #[must_use]
    pub const fn verified(&self) -> &VerifiedJumboRope {
        &self.verified
    }

    /// Work and bounded scratch counters from this write.
    #[must_use]
    pub const fn metrics(&self) -> JumboRopeBuildMetrics {
        self.metrics
    }
}

/// Writes a borrowed canonical value through a bounded chunk buffer and
/// object sink. The caller can put the returned descriptor in a future typed
/// Docs or SourceProvenance row schema.
pub fn write_jumbo_value<S: JumboRopeObjectSink + ?Sized>(
    context: JumboValueContext,
    bytes: &[u8],
    limits: JumboRopeLimits,
    sink: &mut S,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
    let mut writer = JumboRopeStreamWriter::new(context, limits, sink)?;
    writer.push(bytes)?;
    writer.finish()
}

/// Incremental content-defined writer for a canonical value assembled from
/// borrowed pieces. Its scratch is bounded to one leaf and the Merkle
/// frontier, regardless of how many pieces the caller supplies.
pub struct JumboRopeStreamWriter<'sink, S: JumboRopeObjectSink + ?Sized> {
    inner: RopeWriter<'sink, S>,
}

impl<'sink, S: JumboRopeObjectSink + ?Sized> JumboRopeStreamWriter<'sink, S> {
    /// Starts one value without retaining or copying the caller's input.
    pub fn new(
        context: JumboValueContext,
        limits: JumboRopeLimits,
        sink: &'sink mut S,
    ) -> Result<Self, JumboOperationError<S::Error>> {
        let limits = limits.validate()?;
        Ok(Self {
            inner: RopeWriter::new(context, limits, sink)?,
        })
    }

    /// Adds the next borrowed bytes in canonical order.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), JumboOperationError<S::Error>> {
        self.inner.push(bytes)
    }

    /// Finishes the value and returns its descriptor only after all leaf and
    /// interior writes succeeded and exact length/encoding checks passed.
    pub fn finish(self) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
        self.inner.finish(0)
    }

    pub(super) fn finish_with_input_buffer(
        self,
        input_buffer_bytes: u64,
    ) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
        self.inner.finish(input_buffer_bytes)
    }
}

/// Streams a canonical value from a reader using a fixed input buffer and one
/// reusable bounded leaf buffer. The complete value is never materialized.
pub fn write_jumbo_value_from_reader<R, S>(
    context: JumboValueContext,
    reader: &mut R,
    limits: JumboRopeLimits,
    sink: &mut S,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>>
where
    R: Read + ?Sized,
    S: JumboRopeObjectSink + ?Sized,
{
    let mut writer = JumboRopeStreamWriter::new(context, limits, sink)?;
    let mut input = [0_u8; JUMBO_ROPE_STREAM_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut input)
            .map_err(JumboOperationError::Input)?;
        if read == 0 {
            break;
        }
        writer.push(&input[..read])?;
    }
    writer.finish_with_input_buffer(JUMBO_ROPE_STREAM_BUFFER_BYTES as u64)
}

struct RopeWriter<'sink, S: JumboRopeObjectSink + ?Sized> {
    context: JumboValueContext,
    limits: JumboRopeLimits,
    sink: &'sink mut S,
    scratch: Vec<u8>,
    frontier: Vec<RopeObjectRef>,
    gear: [u64; 256],
    gear_window: [u8; GEAR_WINDOW_BYTES],
    gear_window_next: usize,
    gear_window_full: bool,
    rolling: u64,
    utf8: Utf8Validator,
    input_bytes: u64,
    emitted_bytes: u64,
    leaf_count: u64,
    interior_node_count: u64,
    peak_frontier_entries: usize,
}

impl<'sink, S: JumboRopeObjectSink + ?Sized> RopeWriter<'sink, S> {
    fn new(
        context: JumboValueContext,
        limits: JumboRopeLimits,
        sink: &'sink mut S,
    ) -> Result<Self, JumboOperationError<S::Error>> {
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(JUMBO_ROPE_MAX_LEAF_BYTES)
            .map_err(|_| JumboRopeError::Allocation)?;
        if scratch.capacity() > JUMBO_ROPE_MAX_LEAF_BYTES {
            return Err(JumboRopeError::ScratchCapacity {
                observed: scratch.capacity(),
                maximum: JUMBO_ROPE_MAX_LEAF_BYTES,
            }
            .into());
        }
        let mut frontier = Vec::new();
        frontier
            .try_reserve_exact(MAX_PROOF_DEPTH)
            .map_err(|_| JumboRopeError::Allocation)?;
        if frontier.capacity() > MAX_PROOF_DEPTH {
            return Err(JumboRopeError::FrontierCapacity {
                observed: frontier.capacity(),
                maximum: MAX_PROOF_DEPTH,
            }
            .into());
        }
        Ok(Self {
            context,
            limits,
            sink,
            scratch,
            frontier,
            gear: gear_table(),
            gear_window: [0; GEAR_WINDOW_BYTES],
            gear_window_next: 0,
            gear_window_full: false,
            rolling: 0,
            utf8: Utf8Validator::default(),
            input_bytes: 0,
            emitted_bytes: 0,
            leaf_count: 0,
            interior_node_count: 0,
            peak_frontier_entries: 0,
        })
    }

    fn push(&mut self, input: &[u8]) -> Result<(), JumboOperationError<S::Error>> {
        for byte in input.iter().copied() {
            let next = self
                .input_bytes
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            if next > self.limits.max_value_bytes {
                return Err(JumboRopeError::ValueTooLarge {
                    observed: next,
                    maximum: self.limits.max_value_bytes,
                }
                .into());
            }
            if self.context.encoding == JumboValueEncoding::Utf8 {
                self.utf8.push(byte)?;
            }
            self.scratch.push(byte);
            self.input_bytes = next;
            self.update_rolling_hash(byte);
            let reached_max = self.scratch.len() == JUMBO_ROPE_MAX_LEAF_BYTES;
            let target_cut =
                self.scratch.len() >= JUMBO_ROPE_MIN_LEAF_BYTES && (self.rolling & CUT_MASK) == 0;
            if reached_max || target_cut {
                self.flush_leaf()?;
            }
        }
        Ok(())
    }

    fn update_rolling_hash(&mut self, byte: u8) {
        let incoming = self.gear[byte as usize];
        self.rolling = self
            .rolling
            .wrapping_mul(GEAR_ROLLING_BASE)
            .wrapping_add(incoming);
        if self.gear_window_full {
            let outgoing = self.gear[self.gear_window[self.gear_window_next] as usize];
            self.rolling = self
                .rolling
                .wrapping_sub(outgoing.wrapping_mul(GEAR_ROLLING_POWER));
        }
        self.gear_window[self.gear_window_next] = byte;
        self.gear_window_next = (self.gear_window_next + 1) % GEAR_WINDOW_BYTES;
        if self.gear_window_next == 0 {
            self.gear_window_full = true;
        }
    }

    fn flush_leaf(&mut self) -> Result<(), JumboOperationError<S::Error>> {
        if self.scratch.is_empty() {
            return Ok(());
        }
        if self.leaf_count >= self.limits.max_leaf_count {
            return Err(JumboRopeError::TooManyLeaves {
                observed: self.leaf_count.saturating_add(1),
                maximum: self.limits.max_leaf_count,
            }
            .into());
        }
        let leaf_length =
            u64::try_from(self.scratch.len()).map_err(|_| JumboRopeError::LengthOverflow)?;
        let id = JumboRopeObjectId(leaf_identity(&self.scratch));
        self.sink
            .write_leaf(JumboRopeLeafRef {
                id,
                ordinal: self.leaf_count,
                byte_offset: self.emitted_bytes,
                bytes: &self.scratch,
            })
            .map_err(JumboOperationError::Store)?;
        let leaf = RopeObjectRef {
            kind: RopeObjectKind::Leaf,
            id,
            first_leaf: self.leaf_count,
            leaf_count: 1,
            byte_length: leaf_length,
        };
        self.leaf_count = self
            .leaf_count
            .checked_add(1)
            .ok_or(JumboRopeError::LengthOverflow)?;
        self.emitted_bytes = self
            .emitted_bytes
            .checked_add(leaf_length)
            .ok_or(JumboRopeError::LengthOverflow)?;
        self.frontier.push(leaf);
        self.peak_frontier_entries = self.peak_frontier_entries.max(self.frontier.len());
        self.scratch.clear();
        self.merge_equal_frontier()?;
        Ok(())
    }

    fn merge_equal_frontier(&mut self) -> Result<(), JumboOperationError<S::Error>> {
        while self.frontier.len() >= 2 {
            let right_index = self.frontier.len() - 1;
            let left_index = right_index - 1;
            if self.frontier[left_index].leaf_count != self.frontier[right_index].leaf_count {
                break;
            }
            let right = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let left = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let node = JumboRopeNode::create(left, right)?;
            self.sink
                .write_interior(&node)
                .map_err(JumboOperationError::Store)?;
            self.interior_node_count = self
                .interior_node_count
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            self.frontier.push(node.as_ref());
        }
        self.peak_frontier_entries = self.peak_frontier_entries.max(self.frontier.len());
        Ok(())
    }

    fn finish(
        mut self,
        input_buffer_bytes: u64,
    ) -> Result<JumboRopeWriteReceipt, JumboOperationError<S::Error>> {
        if self.context.encoding == JumboValueEncoding::Utf8 {
            self.utf8.finish()?;
        }
        self.flush_leaf()?;
        while self.frontier.len() > 1 {
            let right = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let left = self
                .frontier
                .pop()
                .ok_or(JumboRopeError::ClosureCensusMismatch)?;
            let node = JumboRopeNode::create(left, right)?;
            self.sink
                .write_interior(&node)
                .map_err(JumboOperationError::Store)?;
            self.interior_node_count = self
                .interior_node_count
                .checked_add(1)
                .ok_or(JumboRopeError::LengthOverflow)?;
            self.frontier.push(node.as_ref());
        }
        if self.input_bytes != self.emitted_bytes {
            return Err(JumboRopeError::ClosureCensusMismatch.into());
        }
        let root = self
            .frontier
            .first()
            .map_or_else(empty_rope_root, |root| root.id);
        let untrusted = UntrustedJumboValueDescriptor::from_fields(
            self.context.owner,
            self.context.family,
            self.context.field_ordinal,
            self.context.encoding,
            self.input_bytes,
            self.leaf_count,
            root.0,
        );
        let descriptor = untrusted.check(self.limits)?;
        let verified = VerifiedJumboRope { descriptor };
        let frontier_bytes = self
            .frontier
            .capacity()
            .checked_mul(size_of::<RopeObjectRef>())
            .ok_or(JumboRopeError::LengthOverflow)?;
        let peak_live_scratch_bytes = size_of::<Self>()
            .checked_add(self.scratch.capacity())
            .and_then(|bytes| bytes.checked_add(frontier_bytes))
            .and_then(|bytes| bytes.checked_add(input_buffer_bytes as usize))
            .ok_or(JumboRopeError::LengthOverflow)?;
        let metrics = JumboRopeBuildMetrics {
            input_bytes: self.input_bytes,
            leaf_count: self.leaf_count,
            interior_node_count: self.interior_node_count,
            payload_hash_bytes: self.input_bytes,
            chunk_scratch_capacity_bytes: self.scratch.capacity() as u64,
            peak_live_scratch_bytes: peak_live_scratch_bytes as u64,
            peak_frontier_entries: self.peak_frontier_entries as u64,
            input_buffer_bytes,
        };
        Ok(JumboRopeWriteReceipt { verified, metrics })
    }
}

fn gear_table() -> [u64; 256] {
    core::array::from_fn(|byte| {
        let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.jumbo.gear.v1");
        hasher.update(&[byte as u8]);
        let digest = hasher.finalize();
        u64::from_le_bytes(digest.as_bytes()[..8].try_into().unwrap_or([0; 8]))
    })
}
