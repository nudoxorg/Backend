//! Executable laws for the canonical single-row byte capacity.
//!
//! A canonical relation row lives inside one leaf node, and a leaf node has a
//! hard encoded-byte ceiling. A relation whose value can exceed that ceiling
//! therefore has rows that are not representable at any tree shape: the
//! failure is a producer contract violation, not a storage accident.
//!
//! Before these laws existed the bound was implicit. `backend-local-service`
//! admitted source records up to 1 MiB while the tree could never store one
//! above 64 KiB, so indexing `memchr 2.8.3` failed on
//! `src/arch/x86_64/avx2/memchr.rs` - a 65 686 byte row against a 65 464 byte
//! capacity - with an opaque `workspace relation node was rejected` that named
//! neither the file, the relation, nor the reason.
//!
//! The oracle here re-derives the encoded leaf size from the published wire
//! grammar rather than from any production helper, so a change to the framing
//! that silently shifts the capacity is observable. Every law is also run as a
//! red mutation case: the value one byte past the published capacity must be
//! rejected by *both* admission paths, because a producer that reaches the
//! tree through the bulk builder must not be able to route around the check
//! that the incremental builder applies.

use backend_version::{
    CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, CutPolicy, DEFAULT_CUT_POLICY,
    PersistentTree, Relation, canonical_leaf,
};

/// A relation whose key and value are both opaque byte strings.
///
/// The laws vary both lengths independently, which a fixed-width test
/// relation cannot do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ByteRelation;

impl Relation for ByteRelation {
    const DOMAIN: u8 = 0x5a;
    const TYPE: u16 = 7;
    type Key = Vec<u8>;
    type Value = Vec<u8>;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(key);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Independently encodes the canonical leaf byte length for one entry.
///
/// This mirrors the published grammar - `V2NODE\0`, the ABI, policy, kind and
/// relation version tags, the relation domain, the big-endian relation type
/// and level, an outer big-endian `u64` body length, and then one
/// length-delimited key field and one length-delimited value field - without
/// calling the production encoder.
fn oracle_leaf_bytes(key_bytes: usize, value_bytes: usize) -> usize {
    let magic = b"V2NODE\0".len();
    let header = 1 // canonical tree ABI
        + 1 // cut policy version
        + 1 // node kind
        + 1 // relation version
        + 1 // relation domain
        + 2 // relation type, big endian
        + 2; // node level, big endian
    let outer_frame = 8;
    let key_field = 8 + key_bytes;
    let value_field = 8 + value_bytes;
    magic + header + outer_frame + key_field + value_field
}

fn row(key_bytes: usize, value_bytes: usize) -> (Vec<u8>, Vec<u8>) {
    (vec![0x11; key_bytes], vec![0x22; value_bytes])
}

#[test]
fn the_published_capacity_is_exactly_the_largest_admissible_row() {
    for key_bytes in [1usize, 8, 32, 64, 256] {
        let capacity = DEFAULT_CUT_POLICY
            .max_row_value_bytes(key_bytes)
            .unwrap_or_else(|| panic!("no capacity published for a {key_bytes} byte key"));

        assert_eq!(
            oracle_leaf_bytes(key_bytes, capacity),
            CutPolicy::MAX_ENCODED_BYTES,
            "a row at the published capacity must fill the node exactly, \
             for a {key_bytes} byte key"
        );

        let (key, value) = row(key_bytes, capacity);
        let entries = [(key.clone(), value)];
        let leaf = canonical_leaf::<ByteRelation>(&entries);
        assert!(
            leaf.is_ok(),
            "a row at the published {capacity} byte capacity must be admitted \
             for a {key_bytes} byte key, got {:?}",
            leaf.err()
        );

        let (_, oversized) = row(key_bytes, capacity.saturating_add(1));
        let entries = [(key, oversized)];
        assert!(
            canonical_leaf::<ByteRelation>(&entries).is_err(),
            "a row one byte past the published {capacity} byte capacity must be \
             rejected for a {key_bytes} byte key"
        );
    }
}

#[test]
fn the_bulk_builder_enforces_the_same_capacity_as_the_leaf_builder() {
    // A producer that reaches the tree through the bulk path must not be able
    // to store a row the incremental path rejects. This is the exact seam the
    // source-ingest defect travelled through: the record was constructed and
    // budgeted far from the tree, and only the bulk builder saw it.
    let key_bytes = 32;
    let capacity = DEFAULT_CUT_POLICY
        .max_row_value_bytes(key_bytes)
        .unwrap_or_else(|| panic!("no capacity published for a {key_bytes} byte key"));

    let (key, value) = row(key_bytes, capacity);
    let items = [(key.clone(), value)];
    assert!(
        PersistentTree::<ByteRelation>::from_sorted_items(&items).is_ok(),
        "the bulk builder must admit a row at the published capacity"
    );

    let (_, oversized) = row(key_bytes, capacity.saturating_add(1));
    let items = [(key, oversized)];
    assert!(
        PersistentTree::<ByteRelation>::from_sorted_items(&items).is_err(),
        "the bulk builder must reject a row one byte past the published capacity, \
         otherwise a producer can route around the incremental check"
    );
}

#[test]
fn capacity_shrinks_by_exactly_one_byte_for_each_key_byte() {
    // The capacity is a function of the key length alone. A regression that
    // forgot one of the two length frames would still look plausible at a
    // single key width, so the law checks the slope rather than a constant.
    let mut previous = DEFAULT_CUT_POLICY
        .max_row_value_bytes(0)
        .unwrap_or_else(|| panic!("no capacity published for an empty key"));
    for key_bytes in 1usize..=64 {
        let capacity = DEFAULT_CUT_POLICY
            .max_row_value_bytes(key_bytes)
            .unwrap_or_else(|| panic!("no capacity published for a {key_bytes} byte key"));
        assert_eq!(
            capacity.saturating_add(1),
            previous,
            "one more key byte must cost exactly one value byte at {key_bytes}"
        );
        previous = capacity;
    }
}

#[test]
fn a_key_that_cannot_fit_a_node_publishes_no_capacity() {
    assert_eq!(
        DEFAULT_CUT_POLICY.max_row_value_bytes(CutPolicy::MAX_ENCODED_BYTES),
        None,
        "a key wider than one node must publish no capacity rather than zero, \
         so a producer cannot read the absence of room as room for nothing"
    );
}

#[test]
fn the_policy_tags_the_capacity_belongs_to_are_the_published_ones() {
    // The capacity is part of the canonical cut policy. If the ABI or policy
    // version moves without the capacity being rechecked, rows admitted under
    // the old bound would silently change shape.
    assert_eq!(DEFAULT_CUT_POLICY.abi(), CANONICAL_TREE_ABI);
    assert_eq!(DEFAULT_CUT_POLICY.version(), CANONICAL_CUT_POLICY_VERSION);
    assert_eq!(
        DEFAULT_CUT_POLICY.max_encoded_bytes(),
        CutPolicy::MAX_ENCODED_BYTES
    );
}
