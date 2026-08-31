use nudox_id::{ContentId, ContentRoutingWord};

use crate::backing::{BackingError, MetadataBacking};
use crate::capacity::{OccupiedSlots, SlotCapacity, StoreInitError, StoreLayout};
use crate::store::StoredObject;

/// A one-based entry coordinate proven to lie inside the store geometry.
#[derive(Clone, Copy)]
pub(crate) struct EntryOrdinal {
    pub(crate) one_based: u32,
}

impl EntryOrdinal {
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "the initialized prefix cannot exceed its validated backing capacity, which StoreLayout caps below u32::MAX"
    )]
    pub(crate) fn for_append(
        position: usize,
        capacity: SlotCapacity,
    ) -> Result<Self, OccupiedSlots> {
        #[allow(
            clippy::as_conversions,
            reason = "u32 slot capacities are lossless on the crate's target-gated 32/64-bit usize architectures"
        )]
        if position >= *capacity as usize {
            return Err(position.into());
        }
        let position = position as u32;
        // The strict capacity comparison and StoreLayout upper bound prove
        // this one-based coordinate non-zero and non-overflowing.
        Ok(Self {
            one_based: position + 1,
        })
    }

    #[allow(
        clippy::as_conversions,
        reason = "the admitted u32 ordinal is lossless on the crate's target-gated 32/64-bit usize architectures"
    )]
    pub(crate) fn position(self) -> OccupiedSlots {
        ((self.one_based - 1) as usize).into()
    }

    #[allow(
        clippy::as_conversions,
        reason = "the admitted u32 ordinal is lossless on the crate's target-gated 32/64-bit usize architectures"
    )]
    pub(crate) fn occupancy(self) -> OccupiedSlots {
        (self.one_based as usize).into()
    }
}

/// One compact index bucket. Zero is the typed vacant state; every other
/// value came from an admitted [`EntryOrdinal`].
#[derive(Clone, Copy)]
pub(crate) struct BucketSlot {
    one_based: u32,
}

impl BucketSlot {
    const VACANT: Self = Self { one_based: 0 };

    const fn occupied(self) -> Option<EntryOrdinal> {
        if self.one_based == Self::VACANT.one_based {
            None
        } else {
            Some(EntryOrdinal {
                one_based: self.one_based,
            })
        }
    }
}

/// A linear proof that one currently probed bucket is vacant.
pub(crate) struct VacantBucket {
    index: usize,
}

/// Bounded result of an open-addressed content lookup.
pub(crate) enum IndexProbe<'entries, DomainTag, PayloadOwner> {
    /// The claimed identity was found and borrowed directly from retained storage.
    Occupied {
        /// Exact retained entry selected by the occupied bucket.
        entry: &'entries StoredObject<DomainTag, PayloadOwner>,
        /// Occupied buckets examined before the match.
        probes: usize,
    },
    /// A free bucket is available for a verified new identity.
    Vacant {
        /// Linear proof consumed by installation.
        bucket: VacantBucket,
        /// Occupied buckets examined before the vacancy.
        probes: usize,
    },
}

/// Preallocated half-load open-addressed index from content identities to
/// append-only entries. Each typed bucket stores a one-based entry ordinal in
/// one word, reserving zero for its vacant state.
pub(crate) struct ContentIndex<BucketBacking: MetadataBacking> {
    buckets: BucketBacking::Table<BucketSlot>,
    mask: usize,
}

impl<BucketBacking: MetadataBacking> ContentIndex<BucketBacking> {
    pub(crate) fn new(layout: &StoreLayout) -> Result<Self, StoreInitError> {
        let buckets =
            BucketBacking::filled(*layout.bucket_count, BucketSlot::VACANT).map_err(|error| {
                match error {
                    BackingError::Reservation(source) => StoreInitError::BucketReservation(source),
                    BackingError::TooSmall {
                        required,
                        available,
                    } => StoreInitError::InlineBucketsTooSmall {
                        required,
                        available,
                    },
                }
            })?;
        Ok(Self {
            buckets,
            mask: layout.bucket_mask(),
        })
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "the power-of-two mask bounds every bucket; installed one-based ordinals are emitted only after appending to this exact immutable prefix"
    )]
    pub(crate) fn probe<'entries, DomainTag, PayloadOwner>(
        &self,
        content: ContentId<DomainTag>,
        entries: &'entries [StoredObject<DomainTag, PayloadOwner>],
    ) -> IndexProbe<'entries, DomainTag, PayloadOwner> {
        let mut bucket = routing_word(content) & self.mask;
        let mut probes = 0;
        loop {
            let Some(entry) = self.buckets[bucket].occupied() else {
                return IndexProbe::Vacant {
                    bucket: VacantBucket { index: bucket },
                    probes,
                };
            };
            let entry = &entries[entry_offset(entry)];
            if entry.reference.content == content {
                return IndexProbe::Occupied { entry, probes };
            }
            probes += 1;
            bucket = (bucket + 1) & self.mask;
        }
    }

    /// Consumes a fresh vacancy proof in the owner-threaded transition that
    /// appended the corresponding entry.
    #[allow(
        clippy::indexing_slicing,
        clippy::needless_pass_by_value,
        reason = "consuming the linear vacancy prevents reuse; it was produced by a masked probe over this exact bucket table"
    )]
    pub(crate) fn install(&mut self, bucket: VacantBucket, entry: EntryOrdinal) {
        self.buckets[bucket.index] = BucketSlot {
            one_based: entry.one_based,
        };
    }
}

#[allow(
    clippy::as_conversions,
    reason = "store geometry caps entry ordinals below the addressable range on every supported 32/64-bit target"
)]
const fn entry_offset(entry: EntryOrdinal) -> usize {
    (entry.one_based - 1) as usize
}

/// Uses the foundation-owned low little-endian routing word directly.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "the fixed 64-bit routing word is consumed under explicit 32/64-bit target branches; the 32-bit branch deliberately selects its low word"
)]
fn routing_word<DomainTag>(content: ContentId<DomainTag>) -> usize {
    #[cfg(target_pointer_width = "64")]
    {
        u64::from(ContentRoutingWord::from(content)) as usize
    }
    #[cfg(target_pointer_width = "32")]
    {
        let word = u64::from(ContentRoutingWord::from(content));
        word as u32 as usize
    }
}
