use alloc::{collections::BTreeMap, vec};
use core::convert::Infallible;
use std::io::{Cursor, Read};

use super::descriptor::empty_rope_root;
use super::wire::{RopeObjectKind, RopeObjectRef, leaf_identity};
use super::*;

#[derive(Default)]
struct MemoryObjects {
    leaves: BTreeMap<JumboRopeObjectId, Vec<u8>>,
    interiors: BTreeMap<JumboRopeObjectId, [u8; ROPE_NODE_WIRE_BYTES]>,
    leaf_order: Vec<JumboRopeObjectId>,
    report_oversized_leaf: bool,
}

impl JumboRopeObjectSink for MemoryObjects {
    type Error = Infallible;

    fn write_leaf(&mut self, leaf: JumboRopeLeafRef<'_>) -> Result<(), Self::Error> {
        assert_eq!(leaf.id.0, leaf_identity(leaf.bytes));
        let bytes = leaf.bytes.to_vec();
        if let Some(previous) = self.leaves.insert(leaf.id, bytes.clone()) {
            assert_eq!(previous, bytes);
        }
        self.leaf_order.push(leaf.id);
        Ok(())
    }

    fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
        let id = node.id();
        let bytes = node.encode_wire();
        if let Some(previous) = self.interiors.insert(id, bytes) {
            assert_eq!(previous, bytes);
        }
        Ok(())
    }
}

impl JumboRopeObjectSource for MemoryObjects {
    type Error = Infallible;

    fn read_leaf(
        &mut self,
        id: JumboRopeObjectId,
        output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
    ) -> Result<Option<usize>, Self::Error> {
        if self.report_oversized_leaf {
            return Ok(Some(output.len() + 1));
        }
        let Some(bytes) = self.leaves.get(&id) else {
            return Ok(None);
        };
        if bytes.len() > output.len() {
            return Ok(Some(output.len() + 1));
        }
        output[..bytes.len()].copy_from_slice(bytes);
        Ok(Some(bytes.len()))
    }

    fn read_interior(
        &mut self,
        id: JumboRopeObjectId,
    ) -> Result<Option<[u8; ROPE_NODE_WIRE_BYTES]>, Self::Error> {
        Ok(self.interiors.get(&id).copied())
    }
}

fn context(encoding: JumboValueEncoding) -> JumboValueContext {
    JumboValueContext::new([0x5a; 32], JumboValueFamily::Documentation, 3, encoding)
}

fn deterministic_bytes(length: usize, seed: u64) -> Vec<u8> {
    let mut result = vec![0; length];
    let mut state = seed;
    for byte in &mut result {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    result
}

fn build(bytes: &[u8], encoding: JumboValueEncoding) -> (MemoryObjects, VerifiedJumboRope) {
    let mut objects = MemoryObjects::default();
    let written = write_jumbo_value(
        context(encoding),
        bytes,
        JumboRopeLimits::default(),
        &mut objects,
    )
    .expect("value should be admitted");
    (objects, written.verified().clone())
}

fn chunks_with_pattern<'bytes>(bytes: &'bytes [u8], pattern: &[usize]) -> Vec<&'bytes [u8]> {
    let mut chunks = Vec::new();
    let mut offset = 0;
    let mut pattern_index = 0;
    while offset < bytes.len() {
        let length = pattern[pattern_index % pattern.len()].max(1);
        let end = offset.saturating_add(length).min(bytes.len());
        chunks.push(&bytes[offset..end]);
        offset = end;
        pattern_index += 1;
    }
    chunks
}

fn write_with_ascii_mode(
    bytes: &[u8],
    encoding: JumboValueEncoding,
    pattern: &[usize],
    allow_ascii_fast_path: bool,
    objects: &mut MemoryObjects,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<Infallible>> {
    let chunks = chunks_with_pattern(bytes, pattern);
    write_slices_with_ascii_mode(&chunks, encoding, allow_ascii_fast_path, objects)
}

fn write_slices_with_ascii_mode(
    chunks: &[&[u8]],
    encoding: JumboValueEncoding,
    allow_ascii_fast_path: bool,
    objects: &mut MemoryObjects,
) -> Result<JumboRopeWriteReceipt, JumboOperationError<Infallible>> {
    super::writer::write_jumbo_value_with_ascii_mode(
        context(encoding),
        chunks,
        JumboRopeLimits::default(),
        allow_ascii_fast_path,
        objects,
    )
}

fn assert_same_written_rope(scalar: &MemoryObjects, fast: &MemoryObjects) {
    assert_eq!(scalar.leaf_order, fast.leaf_order, "ordered leaf IDs");
    assert_eq!(scalar.leaves, fast.leaves, "leaf ID-to-payload mapping");
    assert_eq!(scalar.interiors, fast.interiors, "interior wire objects");
}

#[test]
fn utf8_ascii_fast_path_matches_scalar_ids_for_unaligned_and_mixed_chunks() {
    let ascii = vec![b'x'; MAX_SEMANTIC_SEGMENT_BYTES + 173_003];
    let mixed_text = "# Notes\n\nA café in 東京 uses λ, ∑, and 🧪. UTF-8 code points may straddle any input boundary.\n\n";
    let mixed = mixed_text
        .repeat((MAX_SEMANTIC_SEGMENT_BYTES + 120_000) / mixed_text.len() + 2)
        .into_bytes();

    for (payload, offset, pattern) in [
        (ascii.as_slice(), 1_usize, &[65_537, 1, 2, 3, 65_535][..]),
        (mixed.as_slice(), 3_usize, &[65_537, 1, 2, 3, 65_535][..]),
    ] {
        let mut backing = vec![0xa5; offset];
        backing.extend_from_slice(payload);
        let unaligned = &backing[offset..];
        let mut scalar_objects = MemoryObjects::default();
        let scalar = write_with_ascii_mode(
            unaligned,
            JumboValueEncoding::Utf8,
            pattern,
            false,
            &mut scalar_objects,
        )
        .expect("scalar writer accepts valid UTF-8");
        let mut fast_objects = MemoryObjects::default();
        let fast = write_with_ascii_mode(
            unaligned,
            JumboValueEncoding::Utf8,
            pattern,
            true,
            &mut fast_objects,
        )
        .expect("fast writer accepts valid UTF-8");

        assert_eq!(scalar.verified().descriptor(), fast.verified().descriptor());
        assert_same_written_rope(&scalar_objects, &fast_objects);
    }

    let emoji = "🧪".as_bytes();
    let emoji_offset = mixed
        .windows(emoji.len())
        .position(|window| window == emoji)
        .expect("fixture contains its four-byte marker");
    let mut backing = vec![0xa5; 3];
    backing.extend_from_slice(&mixed);
    let unaligned = &backing[3..];
    let split_codepoint = [
        &unaligned[..emoji_offset + 1],
        &unaligned[emoji_offset + 1..emoji_offset + 3],
        &unaligned[emoji_offset + 3..],
    ];
    let mut scalar_objects = MemoryObjects::default();
    let scalar = write_slices_with_ascii_mode(
        &split_codepoint,
        JumboValueEncoding::Utf8,
        false,
        &mut scalar_objects,
    )
    .expect("scalar writer carries partial code points across pushes");
    let mut fast_objects = MemoryObjects::default();
    let fast = write_slices_with_ascii_mode(
        &split_codepoint,
        JumboValueEncoding::Utf8,
        true,
        &mut fast_objects,
    )
    .expect("fast writer carries partial code points across pushes");
    assert_eq!(scalar.verified().descriptor(), fast.verified().descriptor());
    assert_same_written_rope(&scalar_objects, &fast_objects);
}

#[test]
fn utf8_ascii_fast_path_matches_scalar_failures_and_partial_writes() {
    let prefix = vec![b'a'; 512 * 1024 + 17];
    let malformed: [&[u8]; 7] = [
        b"\x80",             // stray continuation
        b"\xc0\xaf",         // overlong two-byte sequence
        b"\xe2(\xa1",        // invalid continuation
        b"\xe0\x80\x80",     // overlong three-byte sequence
        b"\xed\xa0\x80",     // UTF-16 surrogate
        b"\xf4\x90\x80\x80", // above U+10FFFF
        b"\xf0\x9f\x92",     // truncated four-byte sequence
    ];

    for suffix in malformed {
        let mut bytes = prefix.clone();
        bytes.extend_from_slice(suffix);
        let mut chunks = chunks_with_pattern(&bytes[..prefix.len()], &[65_537, 1, 2, 3, 65_535]);
        for offset in prefix.len()..bytes.len() {
            chunks.push(&bytes[offset..offset + 1]);
        }
        let mut scalar_objects = MemoryObjects::default();
        let scalar = write_slices_with_ascii_mode(
            &chunks,
            JumboValueEncoding::Utf8,
            false,
            &mut scalar_objects,
        );
        let mut fast_objects = MemoryObjects::default();
        let fast = write_slices_with_ascii_mode(
            &chunks,
            JumboValueEncoding::Utf8,
            true,
            &mut fast_objects,
        );

        assert!(matches!(
            scalar,
            Err(JumboOperationError::Rope(JumboRopeError::InvalidUtf8))
        ));
        assert!(matches!(
            fast,
            Err(JumboOperationError::Rope(JumboRopeError::InvalidUtf8))
        ));
        assert_same_written_rope(&scalar_objects, &fast_objects);
    }
}

fn transfer_leaf(
    descriptor: CheckedJumboValueDescriptor,
    ordinal: u64,
    source: &mut MemoryObjects,
    closure: &mut JumboRopeClosure,
    destination: &mut MemoryObjects,
) {
    let proof = prove_jumbo_leaf(&descriptor, ordinal, source).expect("proof should be available");
    let leaf_id = *source
        .leaf_order
        .get(ordinal as usize)
        .expect("leaf ordinal should exist");
    let leaf_bytes = source
        .leaves
        .get(&leaf_id)
        .expect("source leaf should be present");
    let checked = closure
        .check_leaf(ordinal, &leaf_bytes, &proof)
        .expect("leaf path should be valid");
    closure
        .admit_leaf(checked, destination)
        .expect("checked leaf should be admitted");
}

#[test]
fn descriptor_rejects_false_length_census_and_unknown_wire_tags() {
    let impossible = UntrustedJumboValueDescriptor::from_fields(
        [1; 32],
        JumboValueFamily::Documentation,
        0,
        JumboValueEncoding::Bytes,
        32,
        0,
        [2; 32],
    );
    assert_eq!(
        impossible.check(JumboRopeLimits::default()),
        Err(JumboRopeError::InvalidLeafCount)
    );

    let mut wire = UntrustedJumboValueDescriptor::from_fields(
        [1; 32],
        JumboValueFamily::SourceProvenance,
        9,
        JumboValueEncoding::Utf8,
        0,
        0,
        empty_rope_root().0,
    )
    .encode_wire();
    wire[5] = 99;
    assert_eq!(
        UntrustedJumboValueDescriptor::decode_wire(&wire),
        Err(JumboRopeError::UnknownFamily(99))
    );
}

#[test]
fn empty_and_leaf_boundary_sizes_have_canonical_closures() {
    let sizes = [
        0,
        1,
        JUMBO_ROPE_MIN_LEAF_BYTES - 1,
        JUMBO_ROPE_MIN_LEAF_BYTES,
        JUMBO_ROPE_MAX_LEAF_BYTES,
        JUMBO_ROPE_MAX_LEAF_BYTES + 1,
    ];
    for size in sizes {
        let bytes = vec![0x41; size];
        let (mut source, verified) = build(&bytes, JumboValueEncoding::Bytes);
        let mut closure = JumboRopeClosure::new(
            UntrustedJumboValueDescriptor::decode_wire(&verified.descriptor().encode_wire())
                .expect("descriptor should decode"),
            JumboRopeLimits::default(),
        )
        .expect("descriptor should check");
        let mut received = MemoryObjects::default();
        if size == 0 {
            assert_eq!(verified.descriptor().leaf_count(), 0);
            assert!(closure.missing_ranges().next().is_none());
        } else {
            for ordinal in 0..verified.descriptor().leaf_count() {
                transfer_leaf(
                    *verified.descriptor(),
                    ordinal,
                    &mut source,
                    &mut closure,
                    &mut received,
                );
            }
        }
        assert!(closure.finish(&mut received).is_ok());
    }
}

#[test]
fn closure_rejects_a_source_that_reports_an_oversized_leaf() {
    let bytes = deterministic_bytes(700_000, 0xa11c_e55);
    let (mut original, verified) = build(&bytes, JumboValueEncoding::Bytes);
    let descriptor = *verified.descriptor();
    let mut closure = JumboRopeClosure::new(
        UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
            .expect("descriptor should decode"),
        JumboRopeLimits::default(),
    )
    .expect("descriptor should check");
    let mut received = MemoryObjects::default();
    for ordinal in 0..descriptor.leaf_count() {
        transfer_leaf(
            descriptor,
            ordinal,
            &mut original,
            &mut closure,
            &mut received,
        );
    }
    received.report_oversized_leaf = true;
    assert!(matches!(
        closure.finish(&mut received),
        Err(JumboOperationError::Rope(
            JumboRopeError::OversizedStoredLeaf
        ))
    ));
}

#[test]
fn reader_blocks_do_not_change_content_defined_leaf_boundaries() {
    struct FragmentedReader<'bytes> {
        bytes: &'bytes [u8],
        next: usize,
        block: usize,
    }

    impl Read for FragmentedReader<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.next == self.bytes.len() {
                return Ok(0);
            }
            let count = out.len().min(self.block).min(self.bytes.len() - self.next);
            out[..count].copy_from_slice(&self.bytes[self.next..self.next + count]);
            self.next += count;
            Ok(count)
        }
    }

    let bytes = deterministic_bytes(3 * 512 * 1024, 0xfeed_beef);
    let mut borrowed_objects = MemoryObjects::default();
    let borrowed = write_jumbo_value(
        context(JumboValueEncoding::Bytes),
        &bytes,
        JumboRopeLimits::default(),
        &mut borrowed_objects,
    )
    .expect("borrowed input should write");

    let mut reader_objects = MemoryObjects::default();
    let mut reader = FragmentedReader {
        bytes: &bytes,
        next: 0,
        block: 991,
    };
    let streamed = write_jumbo_value_from_reader(
        context(JumboValueEncoding::Bytes),
        &mut reader,
        JumboRopeLimits::default(),
        &mut reader_objects,
    )
    .expect("stream input should write");

    assert_eq!(
        borrowed.verified().descriptor(),
        streamed.verified().descriptor()
    );
    assert_eq!(borrowed_objects.leaf_order, reader_objects.leaf_order);
    assert!(streamed.metrics().chunk_scratch_capacity_bytes() <= JUMBO_ROPE_MAX_LEAF_BYTES as u64);
    let scratch_upper_bound = (JUMBO_ROPE_MAX_LEAF_BYTES
        + JUMBO_ROPE_STREAM_BUFFER_BYTES
        + MAX_PROOF_DEPTH * size_of::<RopeObjectRef>()
        + size_of::<super::writer::RopeWriter<'static, MemoryObjects>>())
        as u64;
    assert!(streamed.metrics().peak_live_scratch_bytes() <= scratch_upper_bound);
    assert_eq!(
        streamed.metrics().input_buffer_bytes(),
        JUMBO_ROPE_STREAM_BUFFER_BYTES as u64
    );
}

#[test]
fn complete_closure_rejects_noncanonical_content_defined_boundaries() {
    let bytes = deterministic_bytes(320 * 1024, 0x8e31_2fbb);
    let (canonical, _) = build(&bytes, JumboValueEncoding::Bytes);
    let canonical_first_length = canonical
        .leaves
        .get(canonical.leaf_order.first().expect("first leaf exists"))
        .expect("canonical first leaf exists")
        .len();
    let nominal_split = 160 * 1024;
    let split = if canonical_first_length == nominal_split {
        nominal_split + 1
    } else {
        nominal_split
    };
    assert_ne!(canonical_first_length, split);

    let first_bytes = &bytes[..split];
    let second_bytes = &bytes[split..];
    let first_id = JumboRopeObjectId(leaf_identity(first_bytes));
    let second_id = JumboRopeObjectId(leaf_identity(second_bytes));
    let first = RopeObjectRef {
        kind: RopeObjectKind::Leaf,
        id: first_id,
        first_leaf: 0,
        leaf_count: 1,
        byte_length: split as u64,
    };
    let second = RopeObjectRef {
        kind: RopeObjectKind::Leaf,
        id: second_id,
        first_leaf: 1,
        leaf_count: 1,
        byte_length: (bytes.len() - split) as u64,
    };
    let node = JumboRopeNode::create(first, second).expect("alternate tree is structurally valid");
    let mut objects = MemoryObjects::default();
    objects.leaves.insert(first_id, first_bytes.to_vec());
    objects.leaves.insert(second_id, second_bytes.to_vec());
    objects.interiors.insert(node.id(), node.encode_wire());
    let descriptor = UntrustedJumboValueDescriptor::from_fields(
        [0x5a; 32],
        JumboValueFamily::Documentation,
        3,
        JumboValueEncoding::Bytes,
        bytes.len() as u64,
        2,
        node.id().0,
    )
    .check(JumboRopeLimits::default())
    .expect("alternate descriptor has a valid structural census");

    assert!(matches!(
        descriptor.admit_stored_closure(&mut objects),
        Err(JumboOperationError::Rope(
            JumboRopeError::NonCanonicalLeafBoundary
        ))
    ));
}

#[test]
fn complete_closure_rejects_noncanonical_interior_tree_shape() {
    let bytes = deterministic_bytes(1_200 * 1024, 0xa492_0df1);
    let (mut objects, written) = build(&bytes, JumboValueEncoding::Bytes);
    assert!(objects.leaf_order.len() > 2);
    let mut right = None;
    for ordinal in (0..objects.leaf_order.len()).rev() {
        let id = objects.leaf_order[ordinal];
        let leaf_bytes = objects.leaves.get(&id).expect("canonical leaf exists");
        let leaf = RopeObjectRef {
            kind: RopeObjectKind::Leaf,
            id,
            first_leaf: ordinal as u64,
            leaf_count: 1,
            byte_length: leaf_bytes.len() as u64,
        };
        let Some(right_child) = right else {
            right = Some(leaf);
            continue;
        };
        let parent = JumboRopeNode::create(leaf, right_child)
            .expect("right-associated tree remains structurally valid");
        objects.interiors.insert(parent.id(), parent.encode_wire());
        right = Some(parent.as_ref());
    }
    let root = right.expect("at least two leaves produce an interior root");
    let descriptor = UntrustedJumboValueDescriptor::from_fields(
        [0x5a; 32],
        JumboValueFamily::Documentation,
        3,
        JumboValueEncoding::Bytes,
        bytes.len() as u64,
        objects.leaf_order.len() as u64,
        root.id.0,
    )
    .check(JumboRopeLimits::default())
    .expect("alternate tree has a valid structural census");
    assert_ne!(
        descriptor.root_claim(),
        written.descriptor().root_claim(),
        "right-associated tree differs from the writer frontier reduction"
    );

    assert!(matches!(
        descriptor.admit_stored_closure(&mut objects),
        Err(JumboOperationError::Rope(
            JumboRopeError::NonCanonicalTreeShape
        ))
    ));
}

#[test]
fn utf8_round_trips_when_a_codepoint_crosses_reader_blocks() {
    let mut bytes = vec![b'a'; JUMBO_ROPE_STREAM_BUFFER_BYTES - 1];
    bytes.extend_from_slice("🧠".as_bytes());
    bytes.extend(vec![b'z'; 1_100_000]);
    let text = core::str::from_utf8(&bytes).expect("fixture is UTF-8");
    let mut original = MemoryObjects::default();
    let mut reader = Cursor::new(text.as_bytes());
    let written = write_jumbo_value_from_reader(
        context(JumboValueEncoding::Utf8),
        &mut reader,
        JumboRopeLimits::default(),
        &mut original,
    )
    .expect("UTF-8 value should write");
    let descriptor = *written.verified().descriptor();

    let mut closure = JumboRopeClosure::new(
        UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
            .expect("descriptor should decode"),
        JumboRopeLimits::default(),
    )
    .expect("descriptor should check");
    let mut received = MemoryObjects::default();
    for ordinal in (0..descriptor.leaf_count()).rev() {
        transfer_leaf(
            descriptor,
            ordinal,
            &mut original,
            &mut closure,
            &mut received,
        );
    }
    let complete = closure
        .finish(&mut received)
        .expect("full ordered UTF-8 closure should verify");
    let mut reassembled = Vec::new();
    complete
        .write_value_to(&mut received, &mut reassembled)
        .expect("verified value should reassemble");
    assert_eq!(reassembled, bytes);
    assert_eq!(core::str::from_utf8(&reassembled).ok(), Some(text));
}

#[test]
fn missing_ranges_resume_and_bad_order_or_content_cannot_publish() {
    let bytes = deterministic_bytes(1536 * 1024, 0x1234_5678);
    let (mut original, verified) = build(&bytes, JumboValueEncoding::Bytes);
    let descriptor = *verified.descriptor();
    let mut closure = JumboRopeClosure::new(
        UntrustedJumboValueDescriptor::decode_wire(&descriptor.encode_wire())
            .expect("descriptor should decode"),
        JumboRopeLimits::default(),
    )
    .expect("descriptor should check");
    let mut received = MemoryObjects::default();
    let leaf_count = descriptor.leaf_count();

    let zero_proof =
        prove_jumbo_leaf(&descriptor, 0, &mut original).expect("first leaf proof should exist");
    let zero_id = original.leaf_order[0];
    let mut corrupt = original.leaves.get(&zero_id).expect("leaf exists").clone();
    corrupt[0] ^= 1;
    assert!(matches!(
        closure.check_leaf(0, &corrupt, &zero_proof),
        Err(JumboRopeError::ProofRootMismatch)
    ));

    let wrong_ordinal_id = original.leaf_order[1];
    let wrong_ordinal = original
        .leaves
        .get(&wrong_ordinal_id)
        .expect("leaf exists")
        .clone();
    let wrong_proof = prove_jumbo_leaf(&descriptor, 1, &mut original).expect("proof should exist");
    assert!(closure.check_leaf(0, &wrong_ordinal, &wrong_proof).is_err());

    let checked = closure
        .check_leaf(
            0,
            &original.leaves.get(&zero_id).expect("leaf exists"),
            &zero_proof,
        )
        .expect("first leaf path should verify");
    closure
        .admit_leaf(checked, &mut received)
        .expect("first leaf should persist");
    let retry_bytes = original.leaves.get(&zero_id).expect("leaf exists");
    let retry = closure
        .check_leaf(0, &retry_bytes, &zero_proof)
        .expect("duplicate path should verify");
    closure
        .admit_leaf(retry, &mut received)
        .expect("an exact retry is idempotent");
    assert_eq!(closure.present_leaf_count(), 1);
    assert_eq!(closure.missing_leaf_count(), leaf_count - 1);
    assert_eq!(closure.missing_ranges().next(), Some(1..leaf_count));
    assert!(matches!(
        closure.finish(&mut received),
        Err(JumboOperationError::Rope(JumboRopeError::MissingLeaves {
            missing
        })) if missing == leaf_count - 1
    ));

    for ordinal in (1..leaf_count).rev() {
        transfer_leaf(
            descriptor,
            ordinal,
            &mut original,
            &mut closure,
            &mut received,
        );
    }
    assert!(closure.missing_ranges().next().is_none());
    assert!(closure.finish(&mut received).is_ok());
}

#[test]
fn front_insertion_reuses_cdc_leaves_while_ordinal_blocks_shift() {
    fn ordinal_reused_bytes(before: &[u8], after: &[u8]) -> u64 {
        const BLOCK: usize = 512 * 1024;
        let old: BTreeMap<[u8; 32], usize> = before
            .chunks(BLOCK)
            .map(|chunk| (*blake3::hash(chunk).as_bytes(), chunk.len()))
            .collect();
        after
            .chunks(BLOCK)
            .filter_map(|chunk| {
                old.get(blake3::hash(chunk).as_bytes())
                    .filter(|length| **length == chunk.len())
                    .map(|length| *length as u64)
            })
            .sum()
    }

    let before = deterministic_bytes(3 * 512 * 1024, 0x5eed_cafe);
    let mut after = vec![b'!'; 30];
    after.extend_from_slice(&before);
    let (mut before_store, before_rope) = build(&before, JumboValueEncoding::Bytes);
    let (mut after_store, after_rope) = build(&after, JumboValueEncoding::Bytes);

    let mut old_counts = BTreeMap::<JumboRopeObjectId, (u64, u64)>::new();
    for id in &before_store.leaf_order {
        let length = before_store
            .leaves
            .get(id)
            .expect("written leaf exists")
            .len() as u64;
        let entry = old_counts.entry(*id).or_default();
        entry.0 += 1;
        entry.1 = length;
    }
    let mut after_counts = BTreeMap::<JumboRopeObjectId, u64>::new();
    for id in &after_store.leaf_order {
        *after_counts.entry(*id).or_default() += 1;
    }
    let cdc_reused_occurrence_bytes = after_counts
        .iter()
        .filter_map(|(id, new_count)| {
            old_counts
                .get(id)
                .map(|(old_count, length)| old_count.min(new_count) * length)
        })
        .sum::<u64>();
    let ordinal_shared_bytes = ordinal_reused_bytes(&before, &after);
    assert_eq!(ordinal_shared_bytes, 0);
    assert!(cdc_reused_occurrence_bytes > ordinal_shared_bytes);
    assert_ne!(
        before_rope.descriptor().root_claim(),
        after_rope.descriptor().root_claim()
    );
    assert!(before_store.leaf_order.len() > 1);
    assert!(after_store.leaf_order.len() > 1);
}

#[test]
fn repeated_and_low_entropy_inputs_stay_bounded_and_deterministic() {
    let repeated = vec![0x7f; 2 * 1024 * 1024];
    let (mut first_store, first) = build(&repeated, JumboValueEncoding::Bytes);
    let (mut second_store, second) = build(&repeated, JumboValueEncoding::Bytes);
    assert_eq!(first.descriptor(), second.descriptor());
    assert_eq!(first_store.leaf_order, second_store.leaf_order);
    assert!(first_store.leaf_order.iter().all(|id| {
        first_store
            .leaves
            .get(id)
            .is_some_and(|bytes| bytes.len() <= JUMBO_ROPE_MAX_LEAF_BYTES)
    }));
    let proof = prove_jumbo_leaf(first.descriptor(), 0, &mut first_store)
        .expect("repeated byte input still has a bounded proof");
    assert!(proof.siblings().len() <= 64);
}
