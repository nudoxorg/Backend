use core::num::NonZeroU64;

use crate::packed::RowIndex;
use crate::{Locality, MetadataBytes, RootEntryCount};
use fearless_simd::Level;
use nudox_id::{
    ContentAuthority, Domain, Encoding, EncodingTag, GenerationId, LocalitySortedEncoding,
};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef, ProviderSet, RemoteBase};
use zerocopy::{
    Immutable, KnownLayout, TryFromBytes, Unalign, Unaligned,
    byteorder::{BigEndian, U32},
};

use super::{descriptor::LocalityDescriptorWireRecord, errors::LocalityError, rank, validate};

pub(in crate::locality) struct BorrowedLanes<'bytes> {
    pub(in crate::locality) rows: &'bytes [U32<BigEndian>],
    pub(in crate::locality) promise_bits: &'bytes [u8],
    pub(in crate::locality) promise_ranks: &'bytes [u8],
    pub(in crate::locality) providers: &'bytes [ProviderWire],
    pub(in crate::locality) placement: PlacementLanes<'bytes>,
}

/// Big-endian provider bits whose non-zero validity is carried by the wire
/// type itself. Byte order never changes whether an integer is zero.
#[repr(transparent)]
#[derive(Immutable, KnownLayout, TryFromBytes, Unaligned)]
pub(in crate::locality) struct ProviderWire(Unalign<NonZeroU64>);

impl ProviderWire {
    pub(in crate::locality) fn provider_set(&self) -> ProviderSet {
        ProviderSet::from_be(self.0.get())
    }
}

pub(in crate::locality) enum PlacementLanes<'bytes> {
    PromisesOnly,
    Overlays(OverlayLanes<'bytes>),
}

pub(in crate::locality) struct OverlayLanes<'bytes> {
    pub(in crate::locality) basis: GenerationId,
    pub(in crate::locality) presence_bits: &'bytes [u8],
    pub(in crate::locality) presence_ranks: &'bytes [u8],
    pub(in crate::locality) descriptors: &'bytes [LocalityDescriptorWireRecord],
}

/// Borrowed validation witness for one complete locality artifact.
///
/// The canonical bytes remain borrowed. Header facts are decoded once and the
/// validated lane table is retained, so random reads and sequential cursors
/// never rebuild the complete grammar geometry. The direct writer traverses
/// the same validator before returning this witness.
pub struct ValidatedLocality<'bytes, DomainTag> {
    /// Underlying complete canonical artifact bytes.
    pub bytes: &'bytes [u8],
    /// Generation identity bound into the fixed header.
    pub generation: GenerationId,
    /// Checked artifact-wide authority shared by every descriptor payload.
    pub content_authority: ContentAuthority<DomainTag>,
    /// Root cardinality bound into the fixed header.
    pub root_count: RootEntryCount,
    exception_count: u32,
    lanes: BorrowedLanes<'bytes>,
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
    pub fn validate<'bytes, DomainTag: Domain>(
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

impl<'bytes, DomainTag: Domain> ValidatedLocality<'bytes, DomainTag> {
    pub(super) const fn from_validated(
        bytes: &'bytes [u8],
        generation: GenerationId,
        content_authority: ContentAuthority<DomainTag>,
        root_count: RootEntryCount,
        exception_count: u32,
        lanes: BorrowedLanes<'bytes>,
    ) -> Self {
        Self {
            bytes,
            generation,
            content_authority,
            root_count,
            exception_count,
            lanes,
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

    pub(crate) fn locality_for(&self, row: RowIndex) -> Locality<DomainTag> {
        self.locality_for_with(row, &mut ())
    }

    pub(crate) fn measured_locality_for(
        &self,
        row: RowIndex,
        work: &mut crate::LocalityLookupWork,
    ) -> Locality<DomainTag> {
        self.locality_for_with(row, work)
    }

    pub(crate) const fn scan(&self) -> super::super::cursor::LocalityCursor<'_, DomainTag> {
        super::super::cursor::LocalityCursor::new(self)
    }

    fn locality_for_with<WorkPolicy: LookupWorkPolicy>(
        &self,
        row: RowIndex,
        work: &mut WorkPolicy,
    ) -> Locality<DomainTag> {
        let Some(exception) =
            binary_search_row(self.lanes.rows, self.exception_count, row.compact(), work)
        else {
            return Locality::Resident;
        };
        match &self.lanes.placement {
            PlacementLanes::PromisesOnly => Locality::Promised(self.provider_at(exception)),
            PlacementLanes::Overlays(overlays) => {
                if rank::member(self.lanes.promise_bits, exception) {
                    let promise =
                        work.rank(self.lanes.promise_bits, self.lanes.promise_ranks, exception);
                    return Locality::Promised(self.provider_at(promise));
                }
                let promise_before =
                    work.rank(self.lanes.promise_bits, self.lanes.promise_ranks, exception);
                overlay_at::<DomainTag, _>(
                    overlays,
                    self.content_authority,
                    exception - promise_before,
                    work,
                )
            }
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "validated rank populations prove every projected promise ordinal is in the typed provider lane"
    )]
    fn provider_at(&self, promise: u32) -> ProviderSet {
        self.lanes.providers[native(promise)].provider_set()
    }

    pub(crate) const fn exception_count(&self) -> u32 {
        self.exception_count
    }

    pub(in crate::locality) const fn cursor_lanes(&self) -> &BorrowedLanes<'bytes> {
        &self.lanes
    }
}

impl<'bytes, DomainTag: Domain> TryFrom<&'bytes [u8]> for ValidatedLocality<'bytes, DomainTag> {
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
pub fn with_validated_locality<ByteOwner, DomainTag: Domain, Output>(
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

#[allow(
    clippy::indexing_slicing,
    reason = "the binary-search interval is initialized from and remains bounded by the exact typed row count"
)]
fn binary_search_row<WorkPolicy: LookupWorkPolicy>(
    rows: &[U32<BigEndian>],
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
        match rows[native(middle)].get().cmp(&target) {
            core::cmp::Ordering::Less => start = middle + 1,
            core::cmp::Ordering::Greater => end = middle,
            core::cmp::Ordering::Equal => return Some(middle),
        }
    }
    None
}

#[allow(
    clippy::indexing_slicing,
    reason = "validated descriptor ranks prove every projection ordinal is in the typed lane"
)]
fn overlay_at<DomainTag: Domain, WorkPolicy: LookupWorkPolicy>(
    lanes: &OverlayLanes<'_>,
    authority: ContentAuthority<DomainTag>,
    overlay: u32,
    work: &mut WorkPolicy,
) -> Locality<DomainTag> {
    if !rank::member(lanes.presence_bits, overlay) {
        return Locality::Overlaid(RemoteBase::Absent {
            generation: lanes.basis,
        });
    }
    let present = work.rank(lanes.presence_bits, lanes.presence_ranks, overlay);
    Locality::Overlaid(RemoteBase::Present {
        generation: lanes.basis,
        object: project_descriptor(&lanes.descriptors[native(present)], authority),
    })
}

pub(in crate::locality) fn project_descriptor<DomainTag: Domain>(
    record: &LocalityDescriptorWireRecord,
    authority: ContentAuthority<DomainTag>,
) -> ObjectRef<DomainTag> {
    ObjectRef {
        content: authority.bind(record.content),
        length: ObjectLength::from(record.length.get()),
        schema: record.schema.get(),
        kind: ObjectKind::from(record.kind.get()),
    }
}

#[allow(
    clippy::as_conversions,
    reason = "the root target gate admits compact u32 locality ordinals as native lane coordinates"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
