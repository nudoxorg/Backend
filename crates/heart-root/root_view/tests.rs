//! Defines root-view tests behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the root-view tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
#![allow(
    clippy::as_conversions,
    clippy::cognitive_complexity,
    clippy::indexing_slicing,
    clippy::type_complexity,
    reason = "fault fixtures mutate exact proven wire offsets and the large-cardinality control uses compile-time bounded values"
)]

use alloc::{collections::TryReserveError, vec, vec::Vec};
use core::mem::size_of;

use backend_version::{ContentAuthorityError, ContentId, DomainCode, ObjectDomain};
use backend_version::object::{
    ObjectDescriptorDecodeError, ObjectDescriptorWireRecord, ObjectKind, ObjectLength, ObjectRef,
};
use backend_version::schema::SchemaId;
use thiserror::Error;
use zerocopy::IntoBytes;

use super::{
    parse::{ROOT_HEADER_BYTES, RootValidationWork, parse_root},
    *,
};
use crate::{
    ClosureError, ClosureScratch, EntryKey, EntryRange, EntryRangeError, GenerationRoot,
    GenerationView, LocalityError, LocalityWriteError, PreparedLocality, RootBuildError, RootEntry,
    RootEntryCount, RootWriteError, ValidatedLocality,
    encode::{ParentWire, ROOT_ROW_RECORD_BYTES, RootWireRecord},
};

const KEY_OFFSET: usize = core::mem::offset_of!(RootWireRecord, key);
const PARENT_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_present);
const PARENT_KEY_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_key);
const DESCRIPTOR_OFFSET: usize = core::mem::offset_of!(RootWireRecord, descriptor);
const CONTENT_OFFSET: usize =
    DESCRIPTOR_OFFSET + core::mem::offset_of!(ObjectDescriptorWireRecord, content);
const SCHEMA_OFFSET: usize =
    DESCRIPTOR_OFFSET + core::mem::offset_of!(ObjectDescriptorWireRecord, schema);

#[derive(Debug, Error)]
enum BorrowedRootTestError {
    #[error(transparent)]
    Build(#[from] RootBuildError),
    #[error(transparent)]
    Write(#[from] RootWriteError),
    #[error(transparent)]
    Read(#[from] RootReadError),
    #[error(transparent)]
    Locality(#[from] LocalityError),
    #[error(transparent)]
    LocalityWrite(#[from] LocalityWriteError),
    #[error(transparent)]
    Range(#[from] EntryRangeError),
    #[error(transparent)]
    Closure(#[from] ClosureError),
    #[error("closure scratch reservation failed")]
    Scratch(#[source] TryReserveError),
}

fn object(seed: u8) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::from_digest([seed; 32]),
        length: ObjectLength::from(u64::from(seed)),
        schema: SchemaId::Object,
        kind: ObjectKind::from(u16::from(seed)),
    }
}

fn entry(key: u64, parent: Option<u64>, seed: u8) -> RootEntry<ObjectDomain> {
    RootEntry {
        key: EntryKey::from(key),
        parent: parent.map(EntryKey::from),
        object: object(seed),
    }
}

fn canonical_root(
    entries: Vec<RootEntry<ObjectDomain>>,
) -> Result<(GenerationRoot<ObjectDomain>, Vec<u8>), BorrowedRootTestError> {
    let root = GenerationRoot::new(entries)?;
    let mut bytes = vec![0; usize::from(root.canonical_len())];
    root.write_canonical(&mut bytes)?;
    Ok((root, bytes))
}

const fn row_start(ordinal: usize) -> usize {
    ROOT_HEADER_BYTES + ordinal * ROOT_ROW_RECORD_BYTES
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_be_bytes());
}

fn constant_memory_forward_chain_steps(rows: usize) -> u64 {
    let mut steps = 0_u64;
    for start in 0..rows {
        for _ in start..rows {
            steps += 1;
        }
    }
    steps
}

fn exact_forward_chain_steps(rows: usize) -> u64 {
    let rows = rows as u64;
    rows * (rows + 1) / 2
}

#[test]
fn borrowed_root_is_pointer_backed_and_selection_matches_owned_view()
-> Result<(), BorrowedRootTestError> {
    let (owned, bytes) = canonical_root(Vec::from([
        entry(3, Some(2), 3),
        entry(1, None, 1),
        entry(2, Some(1), 2),
    ]))?;
    let borrowed = ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice())?;
    assert_eq!(borrowed.bytes.as_ptr(), bytes.as_ptr());
    assert_eq!(
        borrowed.rows.rows().as_ptr().cast::<u8>(),
        bytes.as_ptr().wrapping_add(8)
    );
    assert_eq!(borrowed.id, owned.id);
    assert_eq!(borrowed.entry_count, owned.entry_count);
    assert_eq!(
        borrowed.get(EntryKey::from(2)),
        owned.get(EntryKey::from(2))
    );

    let prepared = PreparedLocality::prepare(&owned, &[])?;
    let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
    let locality = prepared.write(&mut locality_bytes)?;
    let owned_view = GenerationView::new(&owned, &locality)?;
    let borrowed_view = BorrowedGenerationView::new(&borrowed, &locality)?;
    assert_eq!(
        borrowed_view.closure().collect::<Vec<_>>(),
        owned_view.closure().collect::<Vec<_>>()
    );

    let range = EntryRange::new(EntryKey::from(3), EntryKey::from(3))?;
    let mut owned_scratch =
        ClosureScratch::new(owned.len()).map_err(BorrowedRootTestError::Scratch)?;
    let mut borrowed_scratch =
        ClosureScratch::new(owned.len()).map_err(BorrowedRootTestError::Scratch)?;
    let owned_selected = owned_view.select_closure(Some(range), &mut owned_scratch)?;
    let borrowed_selected = borrowed_view.select_closure(Some(range), &mut borrowed_scratch)?;
    assert_eq!(borrowed_selected.count(), owned_selected.count());
    assert_eq!(
        borrowed_selected.iter().collect::<Vec<_>>(),
        owned_selected.iter().collect::<Vec<_>>()
    );
    assert_eq!(
        borrowed_scratch.work.projected_rows,
        owned_scratch.work.projected_rows
    );
    assert_eq!(
        borrowed_scratch.work.ancestor_edges,
        owned_scratch.work.ancestor_edges
    );
    assert_eq!(owned_scratch.work.parent_search_comparisons, 0);
    assert!(borrowed_scratch.work.parent_search_comparisons > 0);
    Ok(())
}

#[test]
fn exact_extent_and_compact_count_rejections_precede_row_semantics()
-> Result<(), BorrowedRootTestError> {
    let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
    for length in 0..ROOT_HEADER_BYTES {
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(&bytes[..length]),
            Err(RootReadError::HeaderTruncated { required: 8, available }) if available == length
        ));
    }
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(&bytes[..bytes.len() - 1]),
        Err(RootReadError::Truncated { required, available })
            if required == bytes.len() && available + 1 == bytes.len()
    ));
    let mut trailing = bytes.clone();
    trailing.push(0xa5);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(trailing.as_slice()),
        Err(RootReadError::TrailingBytes { expected, actual })
            if expected == bytes.len() && actual == trailing.len()
    ));
    let mut wide = bytes;
    wide[..ROOT_HEADER_BYTES].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(wide.as_slice()),
        Err(RootReadError::CountOutOfRange {
            declared: u64::MAX,
            ..
        })
    ));
    Ok(())
}

#[test]
fn typed_rows_report_parent_schema_and_authority_operands_exactly()
-> Result<(), BorrowedRootTestError> {
    let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
    let base = row_start(0);

    let mut bad_parent = bytes.clone();
    bad_parent[base + PARENT_OFFSET] = 7;
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(bad_parent.as_slice()),
        Err(RootReadError::ParentTag {
            ordinal: 0,
            observed: 7
        })
    ));

    let mut bad_schema = bytes.clone();
    bad_schema[base + SCHEMA_OFFSET..base + SCHEMA_OFFSET + size_of::<u32>()]
        .copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(bad_schema.as_slice()),
        Err(RootReadError::Descriptor {
            ordinal: 0,
            source: ObjectDescriptorDecodeError::Schema(backend_version::schema::UnknownSchemaId(u32::MAX)),
        })
    ));

    let mut bad_domain = bytes;
    let observed = u8::from(DomainCode::DependencySet);
    bad_domain[base + CONTENT_OFFSET] = observed;
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(bad_domain.as_slice()),
        Err(RootReadError::ContentAuthority {
            ordinal: 0,
            source: ContentAuthorityError {
                expected: DomainCode::Object,
                observed: actual,
            },
        }) if actual == observed
    ));
    Ok(())
}

#[test]
fn key_and_every_parent_invariant_have_distinct_rejections() -> Result<(), BorrowedRootTestError> {
    let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1), entry(2, Some(1), 2)]))?;
    let first = row_start(0);
    let second = row_start(1);

    let mut duplicate = bytes.clone();
    set_u64(&mut duplicate, second + KEY_OFFSET, 1);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(duplicate.as_slice()),
        Err(RootReadError::KeyOrder { ordinal: 1, previous, current })
            if previous == EntryKey::from(1) && current == EntryKey::from(1)
    ));

    let mut unsorted = bytes.clone();
    set_u64(&mut unsorted, second + KEY_OFFSET, 0);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(unsorted.as_slice()),
        Err(RootReadError::KeyOrder { ordinal: 1, previous, current })
            if previous == EntryKey::from(1) && current == EntryKey::from(0)
    ));

    let mut absent_key = bytes.clone();
    set_u64(&mut absent_key, first + PARENT_KEY_OFFSET, 9);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(absent_key.as_slice()),
        Err(RootReadError::AbsentParentKey { ordinal: 0, key, observed })
            if key == EntryKey::from(1) && observed == EntryKey::from(9)
    ));

    let mut self_parent = bytes.clone();
    self_parent[first + PARENT_OFFSET] = u8::from(ParentWire::Present);
    set_u64(&mut self_parent, first + PARENT_KEY_OFFSET, 1);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(self_parent.as_slice()),
        Err(RootReadError::SelfParent { ordinal: 0, key }) if key == EntryKey::from(1)
    ));

    let mut missing = bytes.clone();
    set_u64(&mut missing, second + PARENT_KEY_OFFSET, 99);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(missing.as_slice()),
        Err(RootReadError::MissingParent { ordinal: 1, key, parent, insertion: 2 })
            if key == EntryKey::from(2) && parent == EntryKey::from(99)
    ));

    let mut colliding_parent_faults = bytes.clone();
    colliding_parent_faults[first + PARENT_OFFSET] = u8::from(ParentWire::Present);
    set_u64(&mut colliding_parent_faults, first + PARENT_KEY_OFFSET, 99);
    colliding_parent_faults[second + PARENT_OFFSET] = u8::from(ParentWire::Absent);
    set_u64(&mut colliding_parent_faults, second + PARENT_KEY_OFFSET, 7);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(colliding_parent_faults.as_slice()),
        Err(RootReadError::AbsentParentKey { ordinal: 1, key, observed })
            if key == EntryKey::from(2) && observed == EntryKey::from(7)
    ));

    let mut cycle = bytes;
    cycle[first + PARENT_OFFSET] = u8::from(ParentWire::Present);
    set_u64(&mut cycle, first + PARENT_KEY_OFFSET, 2);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(cycle.as_slice()),
        Err(RootReadError::HierarchyCycle { key }) if key == EntryKey::from(1)
    ));
    Ok(())
}

#[test]
fn empty_root_and_branching_hierarchies_keep_closed_behavior() -> Result<(), BorrowedRootTestError>
{
    let (empty_owned, empty_bytes) = canonical_root(Vec::new())?;
    let empty = ValidatedRoot::<ObjectDomain>::try_from(empty_bytes.as_slice())?;
    assert!(empty.is_empty());
    assert_eq!(empty.get(EntryKey::from(1)), None);
    let prepared = PreparedLocality::prepare(&empty_owned, &[])?;
    let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
    let locality = prepared.write(&mut locality_bytes)?;
    let view = BorrowedGenerationView::new(&empty, &locality)?;
    assert!(view.is_empty());
    assert_eq!(view.get(EntryKey::from(1)), None);
    assert_eq!(view.closure().next(), None);
    let mut scratch = ClosureScratch::new(0).map_err(BorrowedRootTestError::Scratch)?;
    assert!(view.select_closure(None, &mut scratch)?.is_empty());

    let (_, joined) = canonical_root(Vec::from([
        entry(1, Some(3), 1),
        entry(2, Some(3), 2),
        entry(3, None, 3),
    ]))?;
    ValidatedRoot::<ObjectDomain>::try_from(joined.as_slice())?;

    let (_, mut incoming_cycle) = canonical_root(Vec::from([
        entry(1, Some(3), 1),
        entry(2, Some(3), 2),
        entry(3, None, 3),
    ]))?;
    let second = row_start(1);
    let third = row_start(2);
    set_u64(&mut incoming_cycle, second + PARENT_KEY_OFFSET, 3);
    incoming_cycle[third + PARENT_OFFSET] = u8::from(ParentWire::Present);
    set_u64(&mut incoming_cycle, third + PARENT_KEY_OFFSET, 2);
    assert!(matches!(
        ValidatedRoot::<ObjectDomain>::try_from(incoming_cycle.as_slice()),
        Err(RootReadError::HierarchyCycle { .. })
    ));
    Ok(())
}

#[test]
fn deep_borrowed_range_reports_parent_search_work() -> Result<(), BorrowedRootTestError> {
    const ROWS: usize = 4_096;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(ROWS)
        .map_err(BorrowedRootTestError::Scratch)?;
    for ordinal in 1..=ROWS {
        entries.push(entry(
            ordinal as u64,
            (ordinal < ROWS).then_some((ordinal + 1) as u64),
            1,
        ));
    }
    let (owned, bytes) = canonical_root(entries)?;
    let borrowed = ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice())?;
    let prepared = PreparedLocality::prepare(&owned, &[])?;
    let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
    let locality = prepared.write(&mut locality_bytes)?;
    let view = BorrowedGenerationView::new(&borrowed, &locality)?;
    let mut scratch = ClosureScratch::new(ROWS).map_err(BorrowedRootTestError::Scratch)?;
    let first = EntryKey::from(1);
    let selected = view.select_closure(Some(EntryRange::new(first, first)?), &mut scratch)?;
    assert_eq!(selected.len(), ROWS);
    assert_eq!(scratch.work.projected_rows, 1);
    assert_eq!(scratch.work.ancestor_edges, ROWS - 1);
    assert!(scratch.work.parent_search_comparisons >= scratch.work.ancestor_edges);
    Ok(())
}

#[test]
fn borrowed_composition_preserves_generation_and_count_mismatch_operands()
-> Result<(), BorrowedRootTestError> {
    let (owned, root_bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
    let root = ValidatedRoot::<ObjectDomain>::try_from(root_bytes.as_slice())?;
    let prepared = PreparedLocality::prepare(&owned, &[])?;
    let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
    prepared.write(&mut locality_bytes)?;

    let (_, other_bytes) = canonical_root(Vec::from([entry(1, None, 2)]))?;
    let other = ValidatedRoot::<ObjectDomain>::try_from(other_bytes.as_slice())?;
    {
        let locality = ValidatedLocality::<ObjectDomain>::try_from(locality_bytes.as_slice())?;
        assert!(matches!(
            BorrowedGenerationView::new(&other, &locality),
            Err(LocalityError::GenerationMismatch {
                locality_generation,
                root_generation,
            }) if locality_generation == root.id && root_generation == other.id
        ));
    }

    let count_offset = 32 + size_of::<u8>();
    locality_bytes[count_offset..count_offset + size_of::<u32>()]
        .copy_from_slice(&2_u32.to_be_bytes());
    let locality = ValidatedLocality::<ObjectDomain>::try_from(locality_bytes.as_slice())?;
    assert!(matches!(
        BorrowedGenerationView::new(&root, &locality),
        Err(LocalityError::RootCountMismatch {
            locality_count,
            root_count,
        }) if locality_count == RootEntryCount::from(2) && root_count == RootEntryCount::from(1)
    ));
    Ok(())
}

#[test]
fn hundred_thousand_forward_parent_rows_use_one_linear_transient_lane()
-> Result<(), BorrowedRootTestError> {
    const ROWS: usize = 100_000;
    let mut bytes = Vec::with_capacity(ROOT_HEADER_BYTES + ROWS * ROOT_ROW_RECORD_BYTES);
    bytes.extend_from_slice(&(ROWS as u64).to_be_bytes());
    let descriptor = ObjectDescriptorWireRecord::from(&object(1));
    for ordinal in 0..ROWS {
        let last = ordinal + 1 == ROWS;
        let row = RootWireRecord {
            key: zerocopy::byteorder::U64::new((ordinal + 1) as u64),
            parent_present: if last {
                ParentWire::Absent
            } else {
                ParentWire::Present
            },
            parent_key: zerocopy::byteorder::U64::new(if last { 0 } else { (ordinal + 2) as u64 }),
            descriptor,
        };
        bytes.extend_from_slice(row.as_bytes());
    }
    let mut work = RootValidationWork::default();
    let root = parse_root::<ObjectDomain>(bytes.as_slice(), &mut work)?;
    assert_eq!(root.len(), ROWS);
    assert_eq!(work.authority_rows, ROWS);
    assert_eq!(work.order_rows, ROWS - 1);
    assert_eq!(work.parent_rows, ROWS);
    assert_eq!(work.scratch_high_water_bytes, ROWS * size_of::<u32>());
    assert!(work.hierarchy_steps <= ROWS * 4);
    assert_eq!(
        constant_memory_forward_chain_steps(2_048),
        exact_forward_chain_steps(2_048)
    );
    assert_eq!(exact_forward_chain_steps(ROWS), 5_000_050_000);
    assert!(exact_forward_chain_steps(ROWS) > work.hierarchy_steps as u64);

    let (_, one) = canonical_root(Vec::from([entry(1, None, 1)]))?;
    let mut one_work = RootValidationWork::default();
    let one_root = parse_root::<ObjectDomain>(one.as_slice(), &mut one_work)?;
    assert_eq!(one_root.len(), 1);
    assert_eq!(one_work.scratch_high_water_bytes, size_of::<u32>());
    assert_eq!(one_work.hierarchy_steps, 2);
    assert_eq!(constant_memory_forward_chain_steps(1), 1);
    Ok(())
}
