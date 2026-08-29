//! One-pass structural validation for the selected sorted-locality grammar.

use core::mem::size_of;

use fearless_simd::Level;
use nudox_id::ObjectDomain;
use nudox_object::{ObjectDescriptorDecodeError, ObjectRef, ProviderSet};
use zerocopy::FromBytes;

use super::{
    errors::{LocalityError, LocalityRegion},
    header::{HEADER_BYTES, HeaderWireRecord},
    layout::{LaneTable, LocalityLayout},
    rank, rows,
    view::ValidatedLocality,
};

pub(super) fn parse<DomainTag>(
    bytes: &[u8],
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    parse_with_level(bytes, None)
}

pub(super) fn parse_accelerated<DomainTag>(
    bytes: &[u8],
    level: Level,
) -> Result<ValidatedLocality<'_, DomainTag>, LocalityError> {
    parse_with_level(bytes, Some(level))
}

fn parse_with_level<DomainTag>(
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
    validate_provider_lane(bytes, lanes, header.promise_count.get())?;
    validate_descriptor_schemas(bytes, lanes, header.present_overlay_count.get())?;
    Ok(ValidatedLocality::from_validated(
        bytes,
        header.generation.into(),
        header.root_count.get().into(),
        header.exception_count.get(),
        lanes,
    ))
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
) -> Result<(), LocalityError> {
    let providers = lane(bytes, lanes.providers, lanes.overlay_bits);
    let mut ordinal = 0;
    while ordinal < promise_count {
        let observed = read_u64(providers, ordinal);
        if let Err(source) = ProviderSet::try_from(observed) {
            return Err(LocalityError::EmptyProvider {
                ordinal,
                observed,
                source,
            });
        }
        ordinal += 1;
    }
    Ok(())
}

fn validate_descriptor_schemas(
    bytes: &[u8],
    lanes: LaneTable,
    present_count: u32,
) -> Result<(), LocalityError> {
    let descriptors = lane(bytes, lanes.present, lanes.basis);
    let mut ordinal = 0;
    while ordinal < present_count {
        let bytes = descriptor_bytes(descriptors, ordinal);
        match ObjectRef::<ObjectDomain>::try_from(bytes) {
            Ok(_) => {}
            Err(ObjectDescriptorDecodeError::Schema(source)) => {
                return Err(LocalityError::PresentOverlaySchema { ordinal, source });
            }
            Err(ObjectDescriptorDecodeError::Width { actual }) => {
                return Err(LocalityError::Truncated {
                    region: LocalityRegion::PresentOverlays,
                    required: size_of::<nudox_object::ObjectDescriptorWireRecord>().into(),
                    available: actual.into(),
                });
            }
        }
        ordinal += 1;
    }
    Ok(())
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
    clippy::indexing_slicing,
    reason = "the validated present-overlay count proves every requested fixed descriptor-record window exists"
)]
fn descriptor_bytes(bytes: &[u8], ordinal: u32) -> &[u8] {
    let start = native(ordinal) * nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES;
    &bytes[start..start + nudox_object::OBJECT_DESCRIPTOR_RECORD_BYTES]
}

#[allow(
    clippy::as_conversions,
    reason = "the root target gate admits compact u32 locality ordinals as native lane coordinates"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
