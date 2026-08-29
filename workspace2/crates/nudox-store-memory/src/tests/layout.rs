use super::*;

#[test]
fn rejected_insert_is_inline_and_has_no_error_path_allocation() {
    assert_eq!(size_of::<StoreError<ObjectDomain>>(), 56);
    assert_eq!(
        size_of::<Result<InsertOutcome, RejectedInsert<ObjectDomain>>>(),
        120
    );
}

#[test]
fn capacity_rejection_returns_owner_with_zero_allocator_activity() -> Result<(), ScenarioError> {
    let mut store = store(0, 1)?;
    let reference = reference(b"data")?;
    let bytes: Box<[u8]> = Box::from(*b"data");
    let pointer = bytes.as_ptr();
    let mut submission = Some((reference, bytes));
    let mut result = None;
    let allocations = measure(|| {
        if let Some((reference, bytes)) = submission.take() {
            result = Some(store.insert_owned(reference, bytes));
        }
    });
    assert_eq!(allocations, allocation_info(0, 0));
    let rejected = match result {
        Some(Err(rejected)) => rejected,
        Some(Ok(observed)) => {
            return Err(ScenarioError::Transition {
                step: ScenarioStep::ExactBoundary,
                expected: ScenarioExpectation::ByteCapacityExceeded,
                observed: match observed {
                    InsertOutcome::Inserted => ScenarioObservation::Inserted,
                    InsertOutcome::AlreadyPresent => ScenarioObservation::AlreadyPresent,
                },
            });
        }
        None => return Err(ScenarioError::AllocationMeasurementDidNotRun),
    };
    assert_eq!(rejected.bytes.as_ptr(), pointer);
    assert_eq!(rejected.reference, reference);
    Ok(())
}

#[test]
fn static_backings_have_exact_layout_and_allocation_cliffs() -> Result<(), ScenarioError> {
    assert_eq!(size_of::<crate::index::BucketSlot>(), size_of::<u32>());
    assert_eq!(size_of::<MemoryStore<ObjectDomain>>(), 104);
    assert_eq!(size_of::<LeanMemoryStore<ObjectDomain>>(), 360);

    let inline_zero = measure_inline_construction::<4, 8>(0)?;
    assert_eq!(inline_zero, allocation_info(0, 0));

    let inline_exact = measure_inline_construction::<4, 8>(4)?;
    assert_eq!(inline_exact, allocation_info(0, 0));

    match InlineMemoryStore::<ObjectDomain, 4, 8>::new(StoreCapacity {
        bytes: 0_u64.into(),
        slots: 5_u32.into(),
    }) {
        Err(StoreInitError::InlineEntriesTooSmall {
            required,
            available,
        }) => {
            assert_eq!(required, 5);
            assert_eq!(available, 4);
        }
        Err(error) => return Err(ScenarioError::Store(error)),
        Ok(_) => return Err(ScenarioError::AllocationMeasurementDidNotRun),
    }
    match InlineMemoryStore::<ObjectDomain, 5, 8>::new(StoreCapacity {
        bytes: 0_u64.into(),
        slots: 5_u32.into(),
    }) {
        Err(StoreInitError::InlineBucketsTooSmall {
            required,
            available,
        }) => {
            assert_eq!(required, 16);
            assert_eq!(available, 8);
        }
        Err(error) => return Err(ScenarioError::Store(error)),
        Ok(_) => return Err(ScenarioError::AllocationMeasurementDidNotRun),
    }

    let heap_exact = measure_heap_construction(4)?;
    let entry_bytes =
        u64::try_from(4 * size_of::<crate::store::StoredObject<ObjectDomain, Box<[u8]>>>())
            .map_err(ScenarioError::FixtureLength)?;
    let index_bytes = u64::try_from(8 * size_of::<crate::index::BucketSlot>())
        .map_err(ScenarioError::FixtureLength)?;
    assert_eq!(heap_exact, allocation_info(2, entry_bytes + index_bytes));

    let large = measure_heap_construction(SUSTAINED_OBJECT_SLOTS)?;
    let large_entry_bytes = u64::from(SUSTAINED_OBJECT_SLOTS)
        * u64::try_from(size_of::<crate::store::StoredObject<ObjectDomain, Box<[u8]>>>())
            .map_err(ScenarioError::FixtureLength)?;
    let large_index_bytes = u64::from(262_144_u32)
        * u64::try_from(size_of::<crate::index::BucketSlot>())
            .map_err(ScenarioError::FixtureLength)?;
    assert_eq!(
        large,
        allocation_info(2, large_entry_bytes + large_index_bytes)
    );
    Ok(())
}

fn measure_inline_construction<const INLINE_ENTRIES: usize, const INLINE_BUCKETS: usize>(
    slots: u32,
) -> Result<AllocationInfo, ScenarioError> {
    let mut construction = None;
    let info = measure(|| {
        construction = Some(
            InlineMemoryStore::<ObjectDomain, INLINE_ENTRIES, INLINE_BUCKETS>::new(StoreCapacity {
                bytes: 0_u64.into(),
                slots: slots.into(),
            })
            .map(drop),
        );
    });
    match construction {
        Some(Ok(())) => Ok(info),
        Some(Err(error)) => Err(ScenarioError::Store(error)),
        None => Err(ScenarioError::AllocationMeasurementDidNotRun),
    }
}

fn measure_heap_construction(slots: u32) -> Result<AllocationInfo, ScenarioError> {
    let mut construction = None;
    let info = measure(|| {
        construction = Some(
            Store::<ObjectDomain, Box<[u8]>, HeapBacking, HeapBacking>::new(StoreCapacity {
                bytes: 0_u64.into(),
                slots: slots.into(),
            })
            .map(drop),
        );
    });
    match construction {
        Some(Ok(())) => Ok(info),
        Some(Err(error)) => Err(ScenarioError::Store(error)),
        None => Err(ScenarioError::AllocationMeasurementDidNotRun),
    }
}

const fn allocation_info(count: u64, bytes: u64) -> AllocationInfo {
    AllocationInfo {
        count_total: count,
        count_current: 0,
        count_max: count,
        bytes_total: bytes,
        bytes_current: 0,
        bytes_max: bytes,
    }
}
