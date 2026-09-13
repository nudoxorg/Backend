//! Defines store behavior for `heart-memory`, whose purpose is to store immutable objects in bounded caller-selected memory.
//! This module owns the store invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::boxed::Box;
use core::ops::Deref;

use heart_identity::{ContentId, Domain};
use heart_object::{ObjectLength, ObjectRef};
use heart_observe::Probe;
use thiserror::Error;

use crate::backing::{BackingError, HeapBacking, InitializedTable, InlineBacking, MetadataBacking};
use crate::capacity::{
    ByteCapacity, OccupiedSlots, RetainedBytes, SlotCapacity, StoreCapacity, StoreInitError,
    StoreLayout, StoreStats,
};
use crate::index::{ContentIndex, EntryOrdinal, IndexProbe, VacantBucket};
use crate::view::{Lookup, StoredObjectView};

/// Small-client entry capacity carried directly by [`LeanMemoryStore`].
pub const INLINE_ENTRY_CAPACITY: usize = 4;
/// Small-client bucket capacity carried directly by [`LeanMemoryStore`].
pub const INLINE_BUCKET_CAPACITY: usize = 8;

/// Fallibly preallocated metadata store for remote or demand-sized use.
pub type MemoryStore<DomainTag, PayloadOwner = Box<[u8]>> =
    Store<DomainTag, PayloadOwner, HeapBacking, HeapBacking>;

/// Fixed small-client store with no metadata allocation.
pub type LeanMemoryStore<DomainTag, PayloadOwner = Box<[u8]>> = Store<
    DomainTag,
    PayloadOwner,
    InlineBacking<INLINE_ENTRY_CAPACITY>,
    InlineBacking<INLINE_BUCKET_CAPACITY>,
>;

/// Explicit compile-time inline metadata policy.
pub type InlineMemoryStore<
    DomainTag,
    const ENTRY_CAPACITY: usize,
    const BUCKET_CAPACITY: usize,
    PayloadOwner = Box<[u8]>,
> = Store<DomainTag, PayloadOwner, InlineBacking<ENTRY_CAPACITY>, InlineBacking<BUCKET_CAPACITY>>;

/// First-write-wins admission result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InsertOutcome {
    /// A new immutable object was admitted.
    Inserted,
    /// Identical coherent object already existed.
    AlreadyPresent,
}

/// Closed, low-cardinality disposition of one admission attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreAdmission {
    /// A new immutable object was retained.
    Inserted,
    /// An identical first write was already retained.
    AlreadyPresent,
    /// Descriptor length did not match submitted bytes.
    LengthMismatch,
    /// Submitted bytes did not match their claimed content ID.
    ContentMismatch,
    /// An existing first write conflicted with this submission.
    IntegrityConflict,
    /// The byte budget rejected this otherwise coherent object.
    ByteCapacityExceeded,
    /// The object-slot budget rejected this otherwise coherent object.
    SlotCapacityExceeded,
}

/// One completed store admission without content IDs or payload data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreProbeEvent {
    /// Closed outcome of the operation.
    pub admission: StoreAdmission,
}

/// Typed admission failure preserving exact accounting evidence.
#[derive(Debug, Error)]
pub enum StoreError<DomainTag: Domain> {
    /// Descriptor and submitted payload length disagree.
    #[error("submitted {actual:?} bytes did not match its returned descriptor length")]
    LengthMismatch {
        /// Actual byte length.
        actual: ObjectLength,
    },
    /// New payload does not hash to its claimed identity.
    #[error("submitted bytes hashed to {computed:?}, not the returned descriptor identity")]
    ContentMismatch {
        /// Identity computed from the submitted canonical bytes.
        computed: ContentId<DomainTag>,
    },
    /// Existing first write differs in bytes or descriptor metadata.
    #[error("first retained write conflicts with the returned submitted descriptor")]
    IntegrityConflict {
        /// First retained immutable descriptor.
        existing: ObjectRef<DomainTag>,
        /// Whether bytes differed in addition to descriptor facts.
        bytes_differ: bool,
    },
    /// Exact retained-byte budget would be exceeded before acceptance.
    #[error("store byte capacity {maximum:?} already holds {current:?} bytes")]
    ByteCapacityExceeded {
        /// Bytes retained before the rejected transition.
        current: RetainedBytes,
        /// Exact submitted immutable byte length.
        requested: ObjectLength,
        /// Immutable byte budget.
        maximum: ByteCapacity,
    },
    /// Exact retained-object slot budget would be exceeded before acceptance.
    #[error("store slot capacity {maximum:?} already holds {current:?} object slots")]
    SlotCapacityExceeded {
        /// Object slots occupied before the rejected transition.
        current: OccupiedSlots,
        /// Exact slots requested by the rejected admission.
        requested: SlotCapacity,
        /// Immutable slot budget.
        maximum: SlotCapacity,
    },
}

/// Rejected insertion returning the exact submitted owner unchanged.
#[derive(derive_more::Debug)]
pub struct RejectedInsert<DomainTag: Domain, PayloadOwner: AsRef<[u8]> = Box<[u8]>> {
    /// Original submitted descriptor, returned without duplication in the error.
    pub reference: ObjectRef<DomainTag>,
    /// Typed rejection reason.
    pub error: StoreError<DomainTag>,
    /// Original byte owner, returned without allocation or conversion.
    #[debug("{}", bytes.as_ref().len())]
    pub bytes: PayloadOwner,
}

pub(crate) struct StoredObject<DomainTag, PayloadOwner> {
    pub(crate) reference: ObjectRef<DomainTag>,
    pub(crate) bytes: PayloadOwner,
}

/// Bounded first-write-wins store with statically selected metadata and payload ownership.
pub struct Store<DomainTag, PayloadOwner, EntryBacking, BucketBacking>
where
    EntryBacking: MetadataBacking,
    BucketBacking: MetadataBacking,
{
    entries: EntryBacking::Table<StoredObject<DomainTag, PayloadOwner>>,
    index: ContentIndex<BucketBacking>,
    stats: StoreStats,
}

impl<DomainTag, PayloadOwner, EntryBacking, BucketBacking> Deref
    for Store<DomainTag, PayloadOwner, EntryBacking, BucketBacking>
where
    EntryBacking: MetadataBacking,
    BucketBacking: MetadataBacking,
{
    type Target = StoreStats;

    fn deref(&self) -> &Self::Target {
        &self.stats
    }
}

impl<DomainTag, PayloadOwner, EntryBacking, BucketBacking>
    Store<DomainTag, PayloadOwner, EntryBacking, BucketBacking>
where
    DomainTag: Domain,
    PayloadOwner: AsRef<[u8]>,
    EntryBacking: MetadataBacking,
    BucketBacking: MetadataBacking,
{
    /// Preallocates every object slot and index bucket before first admission.
    ///
    /// # Errors
    ///
    /// Returns the exact geometry, allocator, or inline-policy capacity cause.
    pub fn new(capacity: StoreCapacity) -> Result<Self, StoreInitError> {
        let layout = StoreLayout::new(capacity)?;
        let entries = EntryBacking::table(layout.entry_capacity).map_err(|error| match error {
            BackingError::Reservation(source) => StoreInitError::EntryReservation(source),
            BackingError::TooSmall {
                required,
                available,
            } => StoreInitError::InlineEntriesTooSmall {
                required,
                available,
            },
        })?;
        let index = ContentIndex::new(&layout)?;
        Ok(Self {
            entries,
            index,
            stats: StoreStats {
                capacity,
                index_buckets: layout.bucket_count,
                index_bytes: layout.index_bytes,
                retained_bytes: 0_u64.into(),
                occupied_slots: 0_usize.into(),
            },
        })
    }

    /// Transfers a verified owner into the store without copying payload bytes.
    ///
    /// # Errors
    ///
    /// Returns the exact descriptor, integrity, or capacity rejection with
    /// `bytes` unchanged.
    #[allow(
        clippy::result_large_err,
        reason = "rejection returns the caller's descriptor and exact byte owner without allocating"
    )]
    pub fn insert_owned(
        &mut self,
        reference: ObjectRef<DomainTag>,
        bytes: PayloadOwner,
    ) -> Result<InsertOutcome, RejectedInsert<DomainTag, PayloadOwner>> {
        let submission = VerifiedSubmission::new(reference, bytes)?;
        let vacant_bucket = match self.existing_outcome(&submission) {
            ExistingOutcome::Vacant { bucket } => bucket,
            ExistingOutcome::AlreadyPresent => return Ok(InsertOutcome::AlreadyPresent),
            ExistingOutcome::Conflict {
                existing,
                bytes_differ,
            } => {
                return Err(submission.reject(StoreError::IntegrityConflict {
                    existing,
                    bytes_differ,
                }));
            }
        };
        let admission = self.prepare_admission(submission, vacant_bucket)?;
        self.commit(admission)
    }

    /// Admits one owner and lazily emits one aggregate outcome event.
    ///
    /// # Errors
    ///
    /// Returns the exact rejection from [`Self::insert_owned`] with the
    /// submitted owner unchanged.
    #[allow(
        clippy::result_large_err,
        reason = "rejection returns the caller's descriptor and exact byte owner without allocating"
    )]
    pub fn insert_owned_with_probe<Observation>(
        &mut self,
        reference: ObjectRef<DomainTag>,
        bytes: PayloadOwner,
        probe: &mut Observation,
    ) -> Result<InsertOutcome, RejectedInsert<DomainTag, PayloadOwner>>
    where
        Observation: Probe<StoreProbeEvent>,
    {
        let result = self.insert_owned(reference, bytes);
        probe.record_with(|| StoreProbeEvent {
            admission: admission_of(&result),
        });
        result
    }

    /// Borrows an immutable stored object with no lock, refcount, or payload copy.
    #[must_use]
    pub fn get(&self, content: ContentId<DomainTag>) -> Option<StoredObjectView<'_, DomainTag>> {
        match self.index.probe(content, &self.entries) {
            IndexProbe::Occupied { entry, .. } => Some(StoredObjectView {
                reference: entry.reference,
                bytes: entry.bytes.as_ref(),
            }),
            IndexProbe::Vacant { .. } => None,
        }
    }

    /// Returns bounded lookup probe evidence.
    #[must_use]
    pub fn lookup(&self, content: ContentId<DomainTag>) -> Lookup {
        match self.index.probe(content, &self.entries) {
            IndexProbe::Occupied { probes, .. } => Lookup::Present { probes },
            IndexProbe::Vacant { probes, .. } => Lookup::Absent { probes },
        }
    }

    fn existing_outcome(
        &self,
        submission: &VerifiedSubmission<DomainTag, PayloadOwner>,
    ) -> ExistingOutcome<DomainTag> {
        match self
            .index
            .probe(submission.reference.content, &self.entries)
        {
            IndexProbe::Occupied { entry, .. }
                if entry.reference == submission.reference
                    && entry.bytes.as_ref() == submission.bytes.as_ref() =>
            {
                ExistingOutcome::AlreadyPresent
            }
            IndexProbe::Occupied { entry, .. } => ExistingOutcome::Conflict {
                existing: entry.reference,
                bytes_differ: entry.bytes.as_ref() != submission.bytes.as_ref(),
            },
            IndexProbe::Vacant { bucket, .. } => ExistingOutcome::Vacant { bucket },
        }
    }

    #[allow(
        clippy::result_large_err,
        reason = "reservation returns the submitted descriptor and exact byte owner on rejection"
    )]
    fn prepare_admission(
        &self,
        submission: VerifiedSubmission<DomainTag, PayloadOwner>,
        bucket: VacantBucket,
    ) -> Result<PreparedAdmission<DomainTag, PayloadOwner>, RejectedInsert<DomainTag, PayloadOwner>>
    {
        let requested_bytes = submission.reference.length;
        if *requested_bytes
            > self
                .stats
                .capacity
                .bytes
                .remaining_after(self.stats.retained_bytes)
        {
            return Err(submission.reject(StoreError::ByteCapacityExceeded {
                current: self.stats.retained_bytes,
                requested: requested_bytes,
                maximum: self.stats.capacity.bytes,
            }));
        }
        let entry = match EntryOrdinal::for_append(self.entries.len(), self.stats.capacity.slots) {
            Ok(entry) => entry,
            Err(current) => {
                return Err(submission.reject(StoreError::SlotCapacityExceeded {
                    current,
                    requested: SlotCapacity::ONE,
                    maximum: self.stats.capacity.slots,
                }));
            }
        };
        let next_stats = StoreStats {
            capacity: self.stats.capacity,
            index_buckets: self.stats.index_buckets,
            index_bytes: self.stats.index_bytes,
            retained_bytes: (*self.stats.retained_bytes + *requested_bytes).into(),
            occupied_slots: entry.occupancy(),
        };
        Ok(PreparedAdmission {
            bucket,
            entry,
            submission,
            next_stats,
        })
    }

    #[allow(
        clippy::result_large_err,
        reason = "the initialized-prefix append returns the exact submitted owner if its vacancy is exhausted"
    )]
    fn commit(
        &mut self,
        admission: PreparedAdmission<DomainTag, PayloadOwner>,
    ) -> Result<InsertOutcome, RejectedInsert<DomainTag, PayloadOwner>> {
        let stored = StoredObject {
            reference: admission.submission.reference,
            bytes: admission.submission.bytes,
        };
        // This is containment for a violated backing contract, not a second
        // admission decision. The prepared ordinal's zero-based position is
        // exactly the initialized occupancy before this failed append.
        if let Err(stored) = self.entries.try_push(stored) {
            return Err(RejectedInsert {
                reference: stored.reference,
                error: StoreError::SlotCapacityExceeded {
                    current: admission.entry.position(),
                    requested: SlotCapacity::ONE,
                    maximum: self.stats.capacity.slots,
                },
                bytes: stored.bytes,
            });
        }
        self.index.install(admission.bucket, admission.entry);
        self.stats = admission.next_stats;
        Ok(InsertOutcome::Inserted)
    }
}

enum ExistingOutcome<DomainTag> {
    AlreadyPresent,
    Conflict {
        existing: ObjectRef<DomainTag>,
        bytes_differ: bool,
    },
    Vacant {
        bucket: VacantBucket,
    },
}

struct VerifiedSubmission<DomainTag, PayloadOwner> {
    reference: ObjectRef<DomainTag>,
    bytes: PayloadOwner,
}

impl<DomainTag: Domain, PayloadOwner: AsRef<[u8]>> VerifiedSubmission<DomainTag, PayloadOwner> {
    #[allow(
        clippy::result_large_err,
        reason = "verification returns the exact submitted owner before it can be probed or copied"
    )]
    fn new(
        reference: ObjectRef<DomainTag>,
        bytes: PayloadOwner,
    ) -> Result<Self, RejectedInsert<DomainTag, PayloadOwner>> {
        let actual = canonical_length(bytes.as_ref().len());
        if reference.length != actual {
            return Err(RejectedInsert {
                reference,
                error: StoreError::LengthMismatch { actual },
                bytes,
            });
        }
        let computed = ContentId::<DomainTag>::from_canonical_bytes(bytes.as_ref());
        if computed != reference.content {
            return Err(RejectedInsert {
                reference,
                error: StoreError::ContentMismatch { computed },
                bytes,
            });
        }
        Ok(Self { reference, bytes })
    }

    fn reject(self, error: StoreError<DomainTag>) -> RejectedInsert<DomainTag, PayloadOwner> {
        RejectedInsert {
            reference: self.reference,
            error,
            bytes: self.bytes,
        }
    }
}

struct PreparedAdmission<DomainTag, PayloadOwner> {
    bucket: VacantBucket,
    entry: EntryOrdinal,
    submission: VerifiedSubmission<DomainTag, PayloadOwner>,
    next_stats: StoreStats,
}

const fn admission_of<DomainTag: Domain, PayloadOwner: AsRef<[u8]>>(
    result: &Result<InsertOutcome, RejectedInsert<DomainTag, PayloadOwner>>,
) -> StoreAdmission {
    match result {
        Ok(InsertOutcome::Inserted) => StoreAdmission::Inserted,
        Ok(InsertOutcome::AlreadyPresent) => StoreAdmission::AlreadyPresent,
        Err(rejected) => match &rejected.error {
            StoreError::LengthMismatch { .. } => StoreAdmission::LengthMismatch,
            StoreError::ContentMismatch { .. } => StoreAdmission::ContentMismatch,
            StoreError::IntegrityConflict { .. } => StoreAdmission::IntegrityConflict,
            StoreError::ByteCapacityExceeded { .. } => StoreAdmission::ByteCapacityExceeded,
            StoreError::SlotCapacityExceeded { .. } => StoreAdmission::SlotCapacityExceeded,
        },
    }
}

#[allow(
    clippy::as_conversions,
    reason = "usize is losslessly representable by the canonical u64 length on supported 32/64-bit targets"
)]
fn canonical_length(length: usize) -> ObjectLength {
    ObjectLength::from(length as u64)
}
