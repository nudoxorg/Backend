use core::{marker::PhantomData, mem::size_of, ops::Deref};

use crate::packed::RowIndex;
use crate::{Locality, MetadataBytes, RootEntryCount};
use fearless_simd::Level;
use nudox_id::{Encoding, EncodingTag, GenerationId, LocalitySortedEncoding};
use nudox_object::{ObjectRef, ProviderSet, RemoteBase};

use super::{
    errors::{LocalityError, LocalityReadError},
    layout::LaneTable,
    rank, validate,
};

/// Borrowed validation witness for one complete locality artifact.
///
/// The canonical bytes remain borrowed. Header facts are decoded once and the
/// validated lane table is retained, so random reads and sequential cursors
/// never rebuild the complete grammar geometry. The direct writer supplies
/// the same already-known facts without recasting its output.
pub struct ValidatedLocality<'bytes, DomainTag> {
    facts: ValidatedLocalityFacts<'bytes>,
    exception_count: u32,
    lanes: LaneTable,
    domain: PhantomData<fn() -> DomainTag>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedLocalityFacts<'bytes> {
    pub bytes: &'bytes [u8],
    pub generation: GenerationId,
    pub root_count: RootEntryCount,
}

impl<'bytes, DomainTag> Deref for ValidatedLocality<'bytes, DomainTag> {
    type Target = ValidatedLocalityFacts<'bytes>;
    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Reusable accelerated locality-validation engine.
///
/// Backend detection occurs once when this owner is constructed. The chosen
/// SIMD implementation remains private to `nudox-root`, so callers depend on
/// the validation contract rather than a replaceable acceleration crate.
#[derive(Clone, Copy)]
pub struct LocalityValidator {
    level: Level,
}

impl LocalityValidator {
    /// Detects and caches the strongest supported validation backend once.
    #[must_use]
    pub fn new() -> Self {
        Self {
            level: match Level::try_detect() {
                Some(level) => level,
                None => Level::baseline(),
            },
        }
    }

    /// Validates one complete artifact with the cached backend.
    ///
    /// Short row lanes remain scalar at the measured crossover; feature
    /// detection never occurs inside validation.
    ///
    /// # Errors
    ///
    /// Returns the exact structural, rank, provider, or schema violation.
    pub fn validate<'bytes, DomainTag>(
        &self,
        bytes: &'bytes [u8],
    ) -> Result<ValidatedLocality<'bytes, DomainTag>, LocalityError> {
        validate::parse_accelerated(bytes, self.level)
    }
}

impl Default for LocalityValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl<'bytes, DomainTag> ValidatedLocality<'bytes, DomainTag> {
    pub(super) const fn from_validated(
        bytes: &'bytes [u8],
        generation: GenerationId,
        root_count: RootEntryCount,
        exception_count: u32,
        lanes: LaneTable,
    ) -> Self {
        Self {
            facts: ValidatedLocalityFacts { bytes, generation, root_count },
            exception_count,
            lanes,
            domain: PhantomData,
        }
    }

    /// Returns exact retained artifact bytes excluding external owner headers.
    #[must_use]
    pub fn metadata_bytes(&self) -> MetadataBytes {
        self.bytes.len().into()
    }

    /// Returns the one authenticated encoding marker selected for this view.
    #[must_use]
    pub const fn encoding() -> EncodingTag {
        LocalitySortedEncoding::TAG
    }

    pub(crate) fn locality_for(
        &self,
        row: RowIndex,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        self.locality_for_with(row, &mut ())
    }

    pub(crate) fn measured_locality_for(
        &self,
        row: RowIndex,
        work: &mut crate::LocalityLookupWork,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        self.locality_for_with(row, work)
    }

    pub(crate) const fn scan(&self) -> super::super::cursor::LocalityCursor<'_, DomainTag> {
        super::super::cursor::LocalityCursor::new(self)
    }

    fn locality_for_with<WorkPolicy: LookupWorkPolicy>(
        &self,
        row: RowIndex,
        work: &mut WorkPolicy,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        let rows = lane(self.bytes, self.lanes.rows, self.lanes.promise_bits);
        let Some(exception) = binary_search_row(rows, self.exception_count, row.compact(), work)
        else {
            return Ok(Locality::Resident);
        };
        let promise_bits = lane(
            self.bytes,
            self.lanes.promise_bits,
            self.lanes.promise_ranks,
        );
        let promise_ranks = lane(self.bytes, self.lanes.promise_ranks, self.lanes.providers);
        if rank::member(promise_bits, exception) {
            let promise = work.rank(promise_bits, promise_ranks, exception);
            return self
                .provider_at(self.lanes, promise)
                .map(Locality::Promised);
        }
        let promise_before = work.rank(promise_bits, promise_ranks, exception);
        self.overlay_from_rank(self.lanes, exception - promise_before, work)
    }

    fn overlay_from_rank<WorkPolicy: LookupWorkPolicy>(
        &self,
        lanes: LaneTable,
        overlay: u32,
        work: &mut WorkPolicy,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        let overlay_bits = lane(self.bytes, lanes.overlay_bits, lanes.overlay_ranks);
        if !rank::member(overlay_bits, overlay) {
            return Ok(Locality::Overlaid(RemoteBase::Absent {
                generation: self.overlay_basis(lanes),
            }));
        }
        let overlay_ranks = lane(self.bytes, lanes.overlay_ranks, lanes.present);
        let present = work.rank(overlay_bits, overlay_ranks, overlay);
        self.descriptor_at(lanes, present).map(|object| {
            Locality::Overlaid(RemoteBase::Present {
                generation: self.overlay_basis(lanes),
                object,
            })
        })
    }

    fn provider_at(
        &self,
        lanes: LaneTable,
        promise: u32,
    ) -> Result<ProviderSet, LocalityReadError> {
        let providers = lane(self.bytes, lanes.providers, lanes.overlay_bits);
        let raw = read_u64(providers, promise);
        super::trusted_decode::provider(raw, promise)
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "a non-promise validated exception proves the measured shared overlay-basis lane is present"
    )]
    fn overlay_basis(&self, lanes: LaneTable) -> GenerationId {
        let mut bytes = [0_u8; 32];
        bytes.copy_from_slice(&self.bytes[lanes.basis..lanes.complete]);
        GenerationId::from(bytes)
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "a validated present-overlay rank is within the exact fixed-width descriptor lane"
    )]
    fn descriptor_at(
        &self,
        lanes: LaneTable,
        present: u32,
    ) -> Result<ObjectRef<DomainTag>, LocalityReadError> {
        let descriptors = lane(self.bytes, lanes.present, lanes.basis);
        let start = native(present) * nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES;
        super::trusted_decode::descriptor(
            &descriptors[start..start + nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES],
            present,
        )
    }

    pub(in crate::locality) fn exception_row(&self, lanes: LaneTable, exception: u32) -> u32 {
        read_u32(lane(self.bytes, lanes.rows, lanes.promise_bits), exception)
    }

    pub(crate) const fn exception_count(&self) -> u32 {
        self.exception_count
    }

    pub(in crate::locality) fn cursor_is_promise(&self, lanes: LaneTable, exception: u32) -> bool {
        rank::member(
            lane(self.bytes, lanes.promise_bits, lanes.promise_ranks),
            exception,
        )
    }

    pub(in crate::locality) fn cursor_overlay_is_present(
        &self,
        lanes: LaneTable,
        overlay: u32,
    ) -> bool {
        rank::member(
            lane(self.bytes, lanes.overlay_bits, lanes.overlay_ranks),
            overlay,
        )
    }

    pub(in crate::locality) fn cursor_promise(
        &self,
        lanes: LaneTable,
        promise: u32,
    ) -> Result<ProviderSet, LocalityReadError> {
        self.provider_at(lanes, promise)
    }

    pub(in crate::locality) fn cursor_overlay_absent(
        &self,
        lanes: LaneTable,
    ) -> Locality<DomainTag> {
        Locality::Overlaid(RemoteBase::Absent {
            generation: self.overlay_basis(lanes),
        })
    }

    pub(in crate::locality) fn cursor_overlay_present(
        &self,
        lanes: LaneTable,
        present: u32,
    ) -> Result<Locality<DomainTag>, LocalityReadError> {
        self.descriptor_at(lanes, present).map(|object| {
            Locality::Overlaid(RemoteBase::Present {
                generation: self.overlay_basis(lanes),
                object,
            })
        })
    }

    pub(in crate::locality) const fn lane_table(&self) -> LaneTable {
        self.lanes
    }
}

impl<'bytes, DomainTag> TryFrom<&'bytes [u8]> for ValidatedLocality<'bytes, DomainTag> {
    type Error = LocalityError;

    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        validate::parse(bytes)
    }
}

/// Validates the current bytes through one HRTB callback. No generic owner
/// caches a validation proof across a potentially interior-mutable `AsRef`.
///
/// # Errors
///
/// Returns the exact validation failure for the bytes borrowed for this call.
pub fn with_validated_locality<ByteOwner, DomainTag, Output>(
    owner: &ByteOwner,
    visit: impl for<'bytes> FnOnce(ValidatedLocality<'bytes, DomainTag>) -> Output,
) -> Result<Output, LocalityError>
where
    ByteOwner: AsRef<[u8]>,
{
    ValidatedLocality::try_from(owner.as_ref()).map(visit)
}

trait LookupWorkPolicy {
    fn search_started(&mut self);
    fn compared(&mut self);
    fn rank(&mut self, bits: &[u8], prefixes: &[u8], ordinal: u32) -> u32;
}

impl LookupWorkPolicy for () {
    fn search_started(&mut self) {}

    fn compared(&mut self) {}

    fn rank(&mut self, bits: &[u8], prefixes: &[u8], ordinal: u32) -> u32 {
        rank::rank(bits, prefixes, ordinal)
    }
}

impl LookupWorkPolicy for crate::LocalityLookupWork {
    fn search_started(&mut self) {
        self.route_binary_searches += 1;
    }

    fn compared(&mut self) {
        self.route_comparisons += 1;
    }

    fn rank(&mut self, bits: &[u8], prefixes: &[u8], ordinal: u32) -> u32 {
        let (rank, words) = rank::measured_rank(bits, prefixes, ordinal);
        self.rank_word_popcounts += words;
        rank
    }
}

fn binary_search_row<WorkPolicy: LookupWorkPolicy>(
    rows: &[u8],
    count: u32,
    target: u32,
    work: &mut WorkPolicy,
) -> Option<u32> {
    work.search_started();
    let mut start = 0;
    let mut end = count;
    while start < end {
        let middle = start + (end - start) / 2;
        work.compared();
        match read_u32(rows, middle).cmp(&target) {
            core::cmp::Ordering::Less => start = middle + 1,
            core::cmp::Ordering::Greater => end = middle,
            core::cmp::Ordering::Equal => return Some(middle),
        }
    }
    None
}

#[allow(
    clippy::indexing_slicing,
    reason = "the validated artifact proves every temporary lane start/end and fixed record window"
)]
fn lane(bytes: &[u8], start: usize, end: usize) -> &[u8] {
    &bytes[start..end]
}

#[allow(
    clippy::indexing_slicing,
    reason = "a validated fixed-width u32 lane has exactly four bytes for every semantic ordinal"
)]
fn read_u32(bytes: &[u8], ordinal: u32) -> u32 {
    let start = native(ordinal) * size_of::<u32>();
    u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ])
}

#[allow(
    clippy::indexing_slicing,
    reason = "a validated fixed-width provider lane has exactly eight bytes for every promise rank"
)]
fn read_u64(bytes: &[u8], ordinal: u32) -> u64 {
    let start = native(ordinal) * size_of::<u64>();
    u64::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
        bytes[start + 4],
        bytes[start + 5],
        bytes[start + 6],
        bytes[start + 7],
    ])
}

#[allow(
    clippy::as_conversions,
    reason = "the root target gate admits compact u32 locality ordinals as native lane coordinates"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
