//! One-pass structural validation for the selected sorted-locality grammar.

use core::mem::size_of;

use fearless_simd::Level;
use zerocopy::{
    FromBytes, TryFromBytes,
    byteorder::{BigEndian, U32, U64},
};

use super::{
    descriptor::{LOCALITY_DESCRIPTOR_BYTES, LocalityDescriptorWireRecord, SCHEMA_OFFSET},
    errors::{LocalityError, LocalityRegion},
    header::{HEADER_BYTES, HeaderWireRecord},
    layout::{LaneTable, LocalityLayout},
    rank, rows,
    view::{BorrowedLanes, OverlayLanes, PlacementLanes, ProviderWire, ValidatedLocality},
};

pub(super) fn parse<DomainTag: nudox_id::Domain>(
    bytes: &[u8],
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    parse_with_level(bytes, None)
}

pub(super) fn parse_accelerated<DomainTag: nudox_id::Domain>(
    bytes: &[u8],
    level: Level,
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    parse_with_level(bytes, Some(level))
}

/// Validates bytes emitted by the private direct writer through the same full
/// grammar as untrusted input. This is intentionally not a weaker shortcut:
/// a writer implementation drift must return the exact public invariant
/// failure before it can mint a borrowed witness.
pub(super) fn from_writer<DomainTag: nudox_id::Domain>(
    bytes: &[u8],
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    parse(bytes)
}

fn descriptor_validity_error(bytes: &[u8]) -> LocalityError {
    for (ordinal, descriptor) in (0_u32..).zip(bytes.chunks_exact(LOCALITY_DESCRIPTOR_BYTES)) {
        let schema = descriptor
            .get(SCHEMA_OFFSET..SCHEMA_OFFSET + size_of::<u32>())
            .and_then(|bytes| <&[u8; 4]>::try_from(bytes).ok())
            .map(|bytes| u32::from_be_bytes(*bytes));
        if let Some(Err(source)) = schema.map(nudox_schema::SchemaId::try_from) {
            return LocalityError::PresentOverlaySchema { ordinal, source };
        }
    }
    LocalityError::TypedLaneInvariant {
        region: LocalityRegion::PresentOverlays,
    }
}

fn parse_with_level<DomainTag: nudox_id::Domain>(
    bytes: &[u8],
    level: Option<Level>,
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    let header_bytes = bytes.get(..HEADER_BYTES).ok_or(LocalityError::Truncated {
        region: LocalityRegion::Header,
        required: HEADER_BYTES.into(),
        available: bytes.len().into(),
    })?;
    let header = HeaderWireRecord::ref_from_bytes(header_bytes).map_err(|source| {
        LocalityError::Truncated {
            region: LocalityRegion::Header,
            required: HEADER_BYTES.into(),
            available: source.into_src().len().into(),
        }
    })?;
    let content_authority =
        nudox_id::ContentAuthority::<DomainTag>::try_from(header.content_domain)?;
    validate_declared_cardinality(header)?;
    let layout = LocalityLayout::new(
        header.exception_count.get().into(),
        header.promise_count.get().into(),
        header.present_overlay_count.get().into(),
    )?;
    let lanes = layout.lanes();
    require_complete(bytes, lanes.complete)?;
    if bytes.len() != lanes.complete {
        return Err(LocalityError::TrailingBytes {
            expected: lanes.complete.into(),
            actual: bytes.len().into(),
        });
    }
    let rows = lane(bytes, lanes.rows, lanes.promise_bits);
    let row_result = match level {
        Some(level) => rows::validate_accelerated(
            rows,
            header.exception_count.get(),
            header.root_count.get(),
            level,
        ),
        None => rows::validate_scalar(rows, header.exception_count.get(), header.root_count.get()),
    };
    row_result.map_err(|error| row_error(error, header.root_count.get()))?;
    validate_membership_lanes(bytes, lanes, header)?;
    let providers = validate_provider_lane(bytes, lanes, header.promise_count.get())?;
    let descriptors =
        validate_descriptor_schemas(bytes, lanes, header.present_overlay_count.get())?;
    let rows = typed_rows(bytes, lanes, header.exception_count.get())?;
    let placement = placement_lanes(bytes, lanes, descriptors)?;
    Ok(ValidatedLocality::from_validated(
        bytes,
        nudox_id::GenerationId::try_from(header.generation)?,
        content_authority,
        header.root_count.get().into(),
        header.exception_count.get(),
        BorrowedLanes {
            rows,
            promise_bits: lane(bytes, lanes.promise_bits, lanes.promise_ranks),
            promise_ranks: lane(bytes, lanes.promise_ranks, lanes.providers),
            providers,
            placement,
        },
    ))
}

fn typed_rows(
    bytes: &[u8],
    lanes: LaneTable,
    count: u32,
) -> Result<&[U32<BigEndian>], LocalityError> {
    <[U32<BigEndian>]>::ref_from_bytes_with_elems(
        lane(bytes, lanes.rows, lanes.promise_bits),
        native(count),
    )
    .map_err(|source| LocalityError::Truncated {
        region: LocalityRegion::ExceptionRows,
        required: lanes.promise_bits.into(),
        available: source.into_src().len().into(),
    })
}

fn placement_lanes<'bytes>(
    bytes: &'bytes [u8],
    lanes: LaneTable,
    descriptors: &'bytes [LocalityDescriptorWireRecord],
) -> Result<PlacementLanes<'bytes>, LocalityError> {
    if u32::from(lanes.overlay_count) == 0 {
        return Ok(PlacementLanes::PromisesOnly);
    }
    let basis = nudox_id::GenerationId::try_from(lane(bytes, lanes.basis, lanes.complete))?;
    Ok(PlacementLanes::Overlays(OverlayLanes {
        basis,
        presence_bits: lane(bytes, lanes.overlay_bits, lanes.overlay_ranks),
        presence_ranks: lane(bytes, lanes.overlay_ranks, lanes.present),
        descriptors,
    }))
}

fn validate_membership_lanes(
    bytes: &[u8],
    lanes: LaneTable,
    header: &HeaderWireRecord,
) -> Result<(), LocalityError> {
    let promise_bits = lane(bytes, lanes.promise_bits, lanes.promise_ranks);
    let promise_ranks = lane(bytes, lanes.promise_ranks, lanes.providers);
    let overlay_bits = lane(bytes, lanes.overlay_bits, lanes.overlay_ranks);
    let overlay_ranks = lane(bytes, lanes.overlay_ranks, lanes.present);
    validate_bits(
        promise_bits,
        header.exception_count.get(),
        LocalityRegion::PromiseBits,
    )?;
    validate_bits(
        overlay_bits,
        u32::from(lanes.overlay_count),
        LocalityRegion::OverlayPresenceBits,
    )?;
    let promises = validate_rank_lane(
        promise_bits,
        promise_ranks,
        header.exception_count.get(),
        LocalityRegion::PromiseRanks,
    )?;
    validate_population(
        promises,
        header.promise_count.get(),
        |declared, observed| LocalityError::PromiseCountMismatch { declared, observed },
    )?;
    let present = validate_rank_lane(
        overlay_bits,
        overlay_ranks,
        u32::from(lanes.overlay_count),
        LocalityRegion::OverlayPresenceRanks,
    )?;
    validate_population(
        present,
        header.present_overlay_count.get(),
        |declared, observed| LocalityError::PresentOverlayCountMismatch { declared, observed },
    )
}

fn validate_population(
    observed: u32,
    declared: u32,
    error: impl FnOnce(u32, u32) -> LocalityError,
) -> Result<(), LocalityError> {
    if observed == declared {
        Ok(())
    } else {
        Err(error(declared, observed))
    }
}

const fn validate_declared_cardinality(header: &HeaderWireRecord) -> Result<(), LocalityError> {
    if header.exception_count.get() > header.root_count.get() {
        return Err(LocalityError::ExceptionCountExceedsRoot {
            exceptions: header.exception_count.get(),
            root_count: header.root_count.get(),
        });
    }
    Ok(())
}

fn require_complete(bytes: &[u8], complete: usize) -> Result<(), LocalityError> {
    if bytes.len() < complete {
        return Err(LocalityError::Truncated {
            region: LocalityRegion::OverlayBasis,
            required: complete.into(),
            available: bytes.len().into(),
        });
    }
    Ok(())
}

const fn row_error(error: rows::RowError, root_count: u32) -> LocalityError {
    match error {
        rows::RowError::OutOfRange { ordinal, row } => LocalityError::ExceptionRowOutOfRange {
            ordinal,
            row,
            root_count,
        },
        rows::RowError::NotStrict {
            ordinal,
            previous,
            current,
        } => LocalityError::ExceptionRowsNotStrict {
            ordinal,
            previous,
            current,
        },
    }
}

const fn validate_bits(
    bits: &[u8],
    count: u32,
    region: LocalityRegion,
) -> Result<(), LocalityError> {
    let mask = rank::unused_tail_mask(count);
    if let Some(&last) = bits.last()
        && mask != 0
        && last & mask != 0
    {
        return Err(LocalityError::NonzeroUnusedBits {
            region,
            byte: bits.len() - 1,
            observed: last,
        });
    }
    Ok(())
}

fn validate_rank_lane(
    bits: &[u8],
    prefixes: &[u8],
    count: u32,
    region: LocalityRegion,
) -> Result<u32, LocalityError> {
    rank::validate_prefixes(bits, prefixes, count).map_err(|(block, observed, expected)| {
        LocalityError::RankPrefixMismatch {
            region,
            block,
            observed,
            expected,
        }
    })
}

fn validate_provider_lane(
    bytes: &[u8],
    lanes: LaneTable,
    promise_count: u32,
) -> Result<&[ProviderWire], LocalityError> {
    let provider_bytes = lane(bytes, lanes.providers, lanes.overlay_bits);
    <[ProviderWire]>::try_ref_from_bytes_with_elems(provider_bytes, native(promise_count))
        .map_err(|_| provider_validity_error(provider_bytes, promise_count))
}

fn provider_validity_error(bytes: &[u8], count: u32) -> LocalityError {
    let Ok(providers) = <[U64<BigEndian>]>::ref_from_bytes_with_elems(bytes, native(count)) else {
        return LocalityError::TypedLaneInvariant {
            region: LocalityRegion::Providers,
        };
    };
    for (ordinal, provider) in (0_u32..).zip(providers) {
        if provider.get() == 0 {
            return LocalityError::EmptyProvider {
                ordinal,
                observed: 0,
                source: nudox_object::ProviderSetError::Empty,
            };
        }
    }
    LocalityError::TypedLaneInvariant {
        region: LocalityRegion::Providers,
    }
}

fn validate_descriptor_schemas(
    bytes: &[u8],
    lanes: LaneTable,
    present_count: u32,
) -> Result<&[LocalityDescriptorWireRecord], LocalityError> {
    let descriptor_bytes = lane(bytes, lanes.present, lanes.basis);
    <[LocalityDescriptorWireRecord]>::try_ref_from_bytes_with_elems(
        descriptor_bytes,
        native(present_count),
    )
    .map_err(|_| descriptor_validity_error(descriptor_bytes))
}

#[allow(
    clippy::indexing_slicing,
    reason = "the exact complete artifact was checked before every temporary lane borrow is derived from its measured offsets"
)]
fn lane(bytes: &[u8], start: usize, end: usize) -> &[u8] {
    &bytes[start..end]
}

#[allow(
    clippy::indexing_slicing,
    reason = "a validated fixed-width provider lane has exactly eight bytes for every promise rank"
)]
#[allow(
    clippy::as_conversions,
    reason = "the root target gate admits compact u32 locality ordinals as native lane coordinates"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
