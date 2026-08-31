use super::*;

#[test]
fn maximum_admitted_ordinal_is_exactly_bounded_by_validated_geometry() -> Result<(), ScenarioError>
{
    use crate::capacity::{MAX_INDEX_SLOTS, StoreLayout};
    use crate::index::EntryOrdinal;

    let capacity = StoreCapacity {
        bytes: 0_u64.into(),
        slots: MAX_INDEX_SLOTS.into(),
    };
    let layout = StoreLayout::new(capacity)?;
    let maximum_slots = usize::try_from(MAX_INDEX_SLOTS).map_err(ScenarioError::Geometry)?;
    assert_eq!(layout.entry_capacity, maximum_slots);

    let last_position = layout.entry_capacity - 1;
    let last = match EntryOrdinal::for_append(last_position, capacity.slots) {
        Ok(last) => last,
        Err(observed) => {
            return Err(ScenarioError::OrdinalGeometry {
                position: *observed,
            });
        }
    };
    assert_eq!(last.one_based, MAX_INDEX_SLOTS);
    assert_eq!(
        EntryOrdinal::for_append(layout.entry_capacity, capacity.slots)
            .map(|entry| entry.one_based),
        Err(maximum_slots.into())
    );
    #[cfg(target_pointer_width = "64")]
    {
        let truncation_candidate =
            usize::try_from(u64::from(u32::MAX) + 1).map_err(ScenarioError::Geometry)?;
        assert_eq!(
            EntryOrdinal::for_append(truncation_candidate, capacity.slots)
                .map(|entry| entry.one_based),
            Err(truncation_candidate.into())
        );
    }
    Ok(())
}

#[test]
fn exact_boundary_and_rejection_preserve_accounting_and_owned_buffer() -> Result<(), ScenarioError>
{
    let mut store = store(4, 1)?;
    require_outcome(
        store.insert_owned(reference(b"abc")?, Box::from(*b"abc")),
        InsertOutcome::Inserted,
        ScenarioStep::ExactBoundary,
    )?;
    let full = *store;
    let submitted = reference(b"z")?;
    let rejected = rejected_insert(store.insert_owned(submitted, Box::from(*b"z")))?;
    assert_eq!(rejected.bytes.as_ref(), b"z");
    assert_eq!(rejected.reference, submitted);
    match rejected.error {
        StoreError::SlotCapacityExceeded {
            current,
            requested,
            maximum,
        } => {
            assert_eq!(current, full.occupied_slots);
            assert_eq!(requested, 1_u32.into());
            assert_eq!(maximum, 1_u32.into());
        }
        error => return Err(unexpected_error(ScenarioStep::ExactBoundary, &error)),
    }
    assert_eq!(*store, full);
    Ok(())
}

#[test]
fn first_write_wins_over_bytes_and_all_descriptor_metadata() -> Result<(), ScenarioError> {
    let mut store = store(10, 2)?;
    let first = reference(b"same")?;
    require_outcome(
        store.insert_owned(first, Box::from(*b"same")),
        InsertOutcome::Inserted,
        ScenarioStep::FirstWrite,
    )?;
    require_outcome(
        store.insert_owned(first, Box::from(*b"same")),
        InsertOutcome::AlreadyPresent,
        ScenarioStep::Replay,
    )?;
    let conflict = ObjectRef {
        content: first.content,
        length: first.length,
        schema: SchemaId::Frame,
        kind: first.kind,
    };
    let rejected = rejected_insert(store.insert_owned(conflict, Box::from(*b"same")))?;
    assert_eq!(rejected.reference, conflict);
    match rejected.error {
        StoreError::IntegrityConflict {
            existing,
            bytes_differ,
        } => {
            assert_eq!(existing, first);
            assert!(!bytes_differ);
        }
        error => return Err(unexpected_error(ScenarioStep::FirstWrite, &error)),
    }
    Ok(())
}

#[test]
fn admission_probe_records_closed_success_and_failure_outcomes() -> Result<(), ScenarioError> {
    let mut store = store(1, 1)?;
    let mut probe = FlightRecorder::<StoreProbeEvent, DropNewest, 2>::new();
    require_outcome(
        store.insert_owned_with_probe(reference(b"a")?, Box::from(*b"a"), &mut probe),
        InsertOutcome::Inserted,
        ScenarioStep::FirstWrite,
    )?;
    let _rejected = rejected_insert(store.insert_owned_with_probe(
        reference(b"b")?,
        Box::from(*b"b"),
        &mut probe,
    ))?;
    let mut events = probe.events();
    assert_eq!(
        events.next(),
        Some(&StoreProbeEvent {
            admission: StoreAdmission::Inserted,
        })
    );
    assert_eq!(
        events.next(),
        Some(&StoreProbeEvent {
            admission: StoreAdmission::ByteCapacityExceeded,
        })
    );
    assert_eq!(events.next(), None);
    Ok(())
}

#[test]
fn borrowed_view_has_no_lock_or_copy_and_exposes_store_lifetime() -> Result<(), ScenarioError> {
    let mut store = store(4, 1)?;
    let object = reference(b"data")?;
    match store.insert_owned(object, Box::from(*b"data")) {
        Ok(InsertOutcome::Inserted) => {}
        Ok(InsertOutcome::AlreadyPresent) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::BorrowedViewAdmission,
                expected: ScenarioExpectation::Inserted,
                observed: ScenarioObservation::AlreadyPresent,
            });
        }
        Err(rejected) => {
            return Err(unexpected_rejection(
                ScenarioStep::BorrowedViewAdmission,
                &rejected,
            ));
        }
    }
    let view = store
        .get(object.content)
        .ok_or(ScenarioError::MissingObject {
            step: ScenarioStep::BorrowedView,
            content: object.content,
        })?;
    assert_eq!(view.reference, object);
    assert_eq!(view.bytes, b"data");
    Ok(())
}

#[test]
fn generic_payload_owner_is_stored_or_rejected_without_conversion() -> Result<(), ScenarioError> {
    let mut accepted: MemoryStore<ObjectDomain, [u8; 4]> = MemoryStore::new(StoreCapacity {
        bytes: 4_u64.into(),
        slots: 1_u32.into(),
    })?;
    let owner = *b"data";
    require_outcome(
        accepted.insert_owned(reference(&owner)?, owner),
        InsertOutcome::Inserted,
        ScenarioStep::FirstWrite,
    )?;
    assert_eq!(
        accepted
            .get(reference(&owner)?.content)
            .map(|view| view.bytes),
        Some(owner.as_slice())
    );

    let mut rejected_store: MemoryStore<ObjectDomain, Vec<u8>> = MemoryStore::new(StoreCapacity {
        bytes: 0_u64.into(),
        slots: 1_u32.into(),
    })?;
    let owner = Vec::from(*b"data");
    let pointer = owner.as_ptr();
    let rejected = match rejected_store.insert_owned(reference(&owner)?, owner) {
        Err(rejected) => rejected,
        Ok(observed) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::ExactBoundary,
                expected: ScenarioExpectation::ByteCapacityExceeded,
                observed: match observed {
                    InsertOutcome::Inserted => ScenarioObservation::Inserted,
                    InsertOutcome::AlreadyPresent => ScenarioObservation::AlreadyPresent,
                },
            });
        }
    };
    assert_eq!(rejected.bytes.as_ptr(), pointer);
    assert_eq!(rejected.bytes, b"data");
    Ok(())
}

#[test]
fn byte_and_slot_n_minus_one_n_n_plus_one_transitions_roll_back_exactly()
-> Result<(), ScenarioError> {
    byte_capacity_transition()?;
    slot_capacity_transition()
}

fn byte_capacity_transition() -> Result<(), ScenarioError> {
    let mut bytes = store(3, 3)?;
    require_outcome(
        bytes.insert_owned(reference(b"aa")?, Box::from(*b"aa")),
        InsertOutcome::Inserted,
        ScenarioStep::ExactBoundary,
    )?;
    assert_eq!(bytes.retained_bytes, 2_u64.into());
    require_outcome(
        bytes.insert_owned(reference(b"b")?, Box::from(*b"b")),
        InsertOutcome::Inserted,
        ScenarioStep::ExactBoundary,
    )?;
    let exact_byte_full = *bytes;
    let byte_submitted = reference(b"c")?;
    let byte_rejected = rejected_insert(bytes.insert_owned(byte_submitted, Box::from(*b"c")))?;
    assert_eq!(byte_rejected.reference, byte_submitted);
    match &byte_rejected.error {
        StoreError::ByteCapacityExceeded {
            current,
            requested,
            maximum,
        } => {
            assert_eq!(*current, exact_byte_full.retained_bytes);
            assert_eq!(*requested, byte_submitted.length);
            assert_eq!(*maximum, 3_u64.into());
        }
        error => return Err(unexpected_error(ScenarioStep::ExactBoundary, error)),
    }
    assert_eq!(*bytes, exact_byte_full);
    Ok(())
}

fn slot_capacity_transition() -> Result<(), ScenarioError> {
    let mut slots = store(10, 2)?;
    require_outcome(
        slots.insert_owned(reference(b"a")?, Box::from(*b"a")),
        InsertOutcome::Inserted,
        ScenarioStep::ExactBoundary,
    )?;
    assert_eq!(slots.occupied_slots, 1_usize.into());
    require_outcome(
        slots.insert_owned(reference(b"b")?, Box::from(*b"b")),
        InsertOutcome::Inserted,
        ScenarioStep::ExactBoundary,
    )?;
    let exact_slot_full = *slots;
    let slot_submitted = reference(b"c")?;
    let slot_rejected = rejected_insert(slots.insert_owned(slot_submitted, Box::from(*b"c")))?;
    assert_eq!(slot_rejected.reference, slot_submitted);
    match &slot_rejected.error {
        StoreError::SlotCapacityExceeded {
            current,
            requested,
            maximum,
        } => {
            assert_eq!(*current, exact_slot_full.occupied_slots);
            assert_eq!(*requested, 1_u32.into());
            assert_eq!(*maximum, 2_u32.into());
        }
        error => return Err(unexpected_error(ScenarioStep::ExactBoundary, error)),
    }
    assert_eq!(*slots, exact_slot_full);
    Ok(())
}

#[test]
fn one_hundred_thousand_object_index_has_bounded_probe_work() -> Result<(), ScenarioError> {
    let mut store = store(
        SUSTAINED_OBJECT_COUNT * OBJECT_PAYLOAD_BYTES,
        SUSTAINED_OBJECT_SLOTS,
    )?;
    let mut highest_probe = 0_usize;
    for key in FIRST_OBJECT_SEQUENCE..SUSTAINED_OBJECT_COUNT {
        let bytes: Box<[u8]> = Box::from(key.to_le_bytes());
        let object = ObjectRef {
            content: ContentId::from_canonical_bytes(&bytes),
            length: OBJECT_PAYLOAD_BYTES.into(),
            schema: SchemaId::Object,
            kind: 1_u16.into(),
        };
        match store.insert_owned(object, bytes) {
            Ok(InsertOutcome::Inserted) => {}
            Ok(InsertOutcome::AlreadyPresent) => {
                return Err(ScenarioError::Transition {
                    step: ScenarioStep::WorkloadAdmission,
                    expected: ScenarioExpectation::Inserted,
                    observed: ScenarioObservation::AlreadyPresent,
                });
            }
            Err(rejected) => {
                return Err(unexpected_rejection(
                    ScenarioStep::WorkloadAdmission,
                    &rejected,
                ));
            }
        }
        highest_probe = highest_probe.max(lookup_probes(store.lookup(object.content)));
    }
    let target = ContentId::from_canonical_bytes(&(SUSTAINED_OBJECT_COUNT - 1).to_le_bytes());
    let lookup = store.lookup(target);
    match lookup {
        Lookup::Present { probes } => assert!(probes < 64),
        Lookup::Absent { .. } => {
            return Err(ScenarioError::MissingObject {
                step: ScenarioStep::WorkloadAdmission,
                content: target,
            });
        }
    }
    assert!(highest_probe < 64);
    assert!(lookup_probes(lookup) < 64);
    assert_eq!(store.index_buckets, 262_144_usize.into());
    assert_eq!(
        store.index_bytes,
        (*store.index_buckets * size_of::<u32>()).into()
    );
    Ok(())
}

#[test]
fn colliding_identities_return_direct_occupied_borrows_with_bounded_work()
-> Result<(), ScenarioError> {
    let mut store = store(16, 2)?;
    let first_bytes = 0_u64.to_le_bytes();
    let first = reference(&first_bytes)?;
    let bucket = u64::from(ContentRoutingWord::from(first.content)) % 4;
    let second_bytes = (1_u64..=u64::from(u16::MAX))
        .map(u64::to_le_bytes)
        .find(|bytes| {
            u64::from(ContentRoutingWord::from(
                ContentId::<ObjectDomain>::from_canonical_bytes(bytes),
            )) % 4
                == bucket
        })
        .ok_or(ScenarioError::NoCollision)?;
    let second = reference(&second_bytes)?;
    let first_owner: Box<[u8]> = Box::from(first_bytes);
    let second_owner: Box<[u8]> = Box::from(second_bytes);
    let first_pointer = first_owner.as_ptr();
    let second_pointer = second_owner.as_ptr();
    require_outcome(
        store.insert_owned(first, first_owner),
        InsertOutcome::Inserted,
        ScenarioStep::WorkloadAdmission,
    )?;
    require_outcome(
        store.insert_owned(second, second_owner),
        InsertOutcome::Inserted,
        ScenarioStep::WorkloadAdmission,
    )?;
    assert_eq!(
        store.get(first.content).map(|view| view.bytes.as_ptr()),
        Some(first_pointer)
    );
    assert_eq!(
        store.get(second.content).map(|view| view.bytes.as_ptr()),
        Some(second_pointer)
    );
    assert_eq!(store.lookup(first.content), Lookup::Present { probes: 0 });
    assert_eq!(store.lookup(second.content), Lookup::Present { probes: 1 });
    Ok(())
}
