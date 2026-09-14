//! Defines locality artifact write behavior for `backend-store`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact write invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::size_of;

use backend_version::GenerationId;
use backend_version::object::RemoteBase;
use zerocopy::{
    IntoBytes,
    byteorder::{BigEndian, U32},
};

use crate::root::locality::{LocalityException, NonResident};
use crate::root::{GenerationRoot, MetadataBytes, RootEntryCount};

use super::{
    descriptor::{LOCALITY_DESCRIPTOR_BYTES, LocalityDescriptorWireRecord},
    errors::{LocalityError, LocalityRegion, LocalityWriteError},
    header::{HEADER_BYTES, HeaderWireRecord},
    layout::{ExceptionCount, LaneTable, LocalityLayout, PresentOverlayCount, PromiseCount},
    rank,
    view::ValidatedLocality,
};

/// Validated immutable sparse input and its exact canonical output layout.
///
/// `prepare` performs every input/root/coherence check exactly once. The
/// consuming `write` method does not allocate or search the root. It validates
/// the completed canonical bytes through the public grammar once so private
/// encoder drift cannot mint a stronger witness than untrusted input receives.
pub struct PreparedLocality<'facts, DomainTag> {
    /// Exact byte capacity required for one direct output artifact.
    pub required_bytes: MetadataBytes,
    facts: &'facts [LocalityException<DomainTag>],
    generation: GenerationId,
    root_count: RootEntryCount,
    layout: LocalityLayout,
    basis: Option<GenerationId>,
}

impl<'facts, DomainTag: backend_version::Domain> PreparedLocality<'facts, DomainTag> {
    /// Checks root-issued exception rows in strict canonical order and
    /// measures every direct-write lane.
    ///
    /// # Errors
    ///
    /// Returns the first root-binding, ordering, basis-coherence, or compact
    /// layout rejection.
    pub fn prepare(
        root: &GenerationRoot<DomainTag>,
        facts: &'facts [LocalityException<DomainTag>],
    ) -> Result<Self, LocalityError> {
        let counts = measure_input(root, facts)?;
        let layout =
            LocalityLayout::new(counts.exceptions, counts.promises, counts.present_overlays)?;
        Ok(Self {
            required_bytes: layout.bytes,
            facts,
            generation: root.id,
            root_count: root.entry_count,
            layout,
            basis: counts.basis,
        })
    }

    /// Preflights caller output before mutation, then writes every artifact
    /// lane directly and returns its already-proved borrowed view.
    ///
    /// # Errors
    ///
    /// Returns [`LocalityWriteError`] before changing any output byte when
    /// the supplied target is shorter than this exact measured layout.
    pub fn write(
        self,
        output: &mut [u8],
    ) -> Result<ValidatedLocality<'_, DomainTag>, LocalityWriteError> {
        let required = usize::from(self.required_bytes);
        if output.len() < required {
            return Err(LocalityWriteError::OutputTooSmall {
                required: self.required_bytes,
                available: output.len().into(),
            });
        }
        #[allow(
            clippy::indexing_slicing,
            reason = "the immediately preceding length preflight proved this exact measured prefix is present before any mutation"
        )]
        let output = &mut output[..required];
        let mut encoder = LocalityEncoder::new(
            output,
            self.generation,
            self.root_count,
            self.layout,
            self.basis,
        );
        for fact in self.facts {
            encoder.emit(fact.row.index, fact.placement);
        }
        encoder.finish().map_err(LocalityWriteError::from)
    }
}

/// Internal reusable direct encoder for prepared facts and propagated
/// overlays. It retains no allocation and accepts only root-issued rows.
pub(crate) struct LocalityEncoder<'output, DomainTag> {
    output: &'output mut [u8],
    lanes: LaneTable,
    layout: LocalityLayout,
    basis: Option<GenerationId>,
    cursor: WriteCursor,
    domain: core::marker::PhantomData<fn() -> DomainTag>,
}

impl<'output, DomainTag: backend_version::Domain> LocalityEncoder<'output, DomainTag> {
    pub(crate) fn new(
        output: &'output mut [u8],
        generation: GenerationId,
        root_count: RootEntryCount,
        layout: LocalityLayout,
        basis: Option<GenerationId>,
    ) -> Self {
        let lanes = layout.lanes();
        write_header::<DomainTag>(output, generation, root_count, &layout);
        clear_membership_and_rank_lanes(output, lanes);
        Self {
            output,
            lanes,
            layout,
            basis,
            cursor: WriteCursor::new(),
            domain: core::marker::PhantomData,
        }
    }

    pub(crate) fn emit(&mut self, row: crate::root::packed::RowIndex, placement: NonResident<DomainTag>) {
        write_row(
            self.output,
            self.lanes.rows,
            self.cursor.exception,
            row.compact,
        );
        self.cursor
            .write_placement(self.output, self.lanes, placement);
        self.cursor.exception += 1;
    }

    pub(crate) fn finish(self) -> Result<ValidatedLocality<'output, DomainTag>, LocalityError> {
        write_rank_directories(self.output, self.lanes, &self.layout);
        write_shared_basis(self.output, self.lanes, self.basis);
        super::validate::from_writer(self.output)
    }
}

#[derive(Clone, Copy)]
struct MeasuredInput {
    exceptions: ExceptionCount,
    promises: PromiseCount,
    present_overlays: PresentOverlayCount,
    basis: Option<GenerationId>,
}

fn measure_input<DomainTag>(
    root: &GenerationRoot<DomainTag>,
    facts: &[LocalityException<DomainTag>],
) -> Result<MeasuredInput, LocalityError> {
    let mut state = InputMeasure::new();
    for fact in facts {
        state.accept(root, fact)?;
    }
    Ok(state.finish())
}

struct InputMeasure {
    previous: Option<crate::root::EntryKey>,
    exceptions: ExceptionCount,
    promises: PromiseCount,
    present_overlays: PresentOverlayCount,
    basis: Option<GenerationId>,
}

impl InputMeasure {
    fn new() -> Self {
        Self {
            previous: None,
            exceptions: 0_u32.into(),
            promises: 0_u32.into(),
            present_overlays: 0_u32.into(),
            basis: None,
        }
    }

    fn accept<DomainTag>(
        &mut self,
        root: &GenerationRoot<DomainTag>,
        fact: &LocalityException<DomainTag>,
    ) -> Result<(), LocalityError> {
        validate_input_row(root, fact, self.previous)?;
        self.previous = Some(fact.row.key);
        self.exceptions = increment_exception(self.exceptions, root.entry_count)?;
        match fact.placement {
            NonResident::Promised(_) => {
                self.promises = increment_promise(self.promises)?;
            }
            NonResident::Overlaid(RemoteBase::Absent { generation }) => {
                normalize_basis(&mut self.basis, fact.row.key, generation)?;
            }
            NonResident::Overlaid(RemoteBase::Present {
                generation,
                object: _,
            }) => {
                normalize_basis(&mut self.basis, fact.row.key, generation)?;
                self.present_overlays = increment_present(self.present_overlays)?;
            }
        }
        Ok(())
    }

    const fn finish(self) -> MeasuredInput {
        MeasuredInput {
            exceptions: self.exceptions,
            promises: self.promises,
            present_overlays: self.present_overlays,
            basis: self.basis,
        }
    }
}

fn validate_input_row<DomainTag>(
    root: &GenerationRoot<DomainTag>,
    fact: &LocalityException<DomainTag>,
    previous: Option<crate::root::EntryKey>,
) -> Result<(), LocalityError> {
    if let Some(previous) = previous
        && fact.row.key <= previous
    {
        return Err(LocalityError::InputOrder {
            previous,
            actual: fact.row.key,
        });
    }
    if fact.row.generation != root.id {
        return Err(LocalityError::InputGenerationMismatch {
            key: fact.row.key,
            row_generation: fact.row.generation,
            root_generation: root.id,
        });
    }
    if fact.row.root_count != root.entry_count {
        return Err(LocalityError::InputRootCountMismatch {
            key: fact.row.key,
            row_count: fact.row.root_count,
            root_count: root.entry_count,
        });
    }
    let actual = root.entry_at(fact.row.index).key;
    if actual != fact.row.key {
        return Err(LocalityError::InputRowKeyMismatch {
            expected: fact.row.key,
            actual,
        });
    }
    Ok(())
}

fn increment_exception(
    count: ExceptionCount,
    root_count: RootEntryCount,
) -> Result<ExceptionCount, LocalityError> {
    count
        .checked_next()
        .filter(|next| u32::from(*next) <= u32::from(root_count))
        .ok_or(LocalityError::ExceptionCountExceedsRoot {
            exceptions: u32::from(count),
            root_count: u32::from(root_count),
        })
}

fn increment_promise(count: PromiseCount) -> Result<PromiseCount, LocalityError> {
    count.checked_next().ok_or(LocalityError::LayoutOverflow {
        region: LocalityRegion::Providers,
        count: u32::from(count),
    })
}

fn increment_present(count: PresentOverlayCount) -> Result<PresentOverlayCount, LocalityError> {
    count.checked_next().ok_or(LocalityError::LayoutOverflow {
        region: LocalityRegion::PresentOverlays,
        count: u32::from(count),
    })
}

fn normalize_basis(
    basis: &mut Option<GenerationId>,
    key: crate::root::EntryKey,
    generation: GenerationId,
) -> Result<(), LocalityError> {
    match *basis {
        Some(expected) if expected != generation => Err(LocalityError::MixedOverlayBasis {
            key,
            expected,
            actual: generation,
        }),
        Some(_) => Ok(()),
        None => {
            *basis = Some(generation);
            Ok(())
        }
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "the measured layout preflight proves every writer output contains the fixed header prefix"
)]
fn write_header<DomainTag: backend_version::Domain>(
    output: &mut [u8],
    generation: GenerationId,
    root_count: RootEntryCount,
    layout: &LocalityLayout,
) {
    let header = HeaderWireRecord {
        generation: *generation,
        content_domain: u8::from(DomainTag::CODE),
        root_count: U32::<BigEndian>::new(u32::from(root_count)),
        exception_count: U32::new(u32::from(layout.exceptions)),
        promise_count: U32::new(u32::from(layout.promises)),
        present_overlay_count: U32::new(u32::from(layout.present_overlays)),
    };
    output[..HEADER_BYTES].copy_from_slice(header.as_bytes());
}

#[allow(
    clippy::indexing_slicing,
    reason = "the output preflight accepted the exact measured layout, whose disjoint lane starts are derived once from the same counts"
)]
fn clear_membership_and_rank_lanes(output: &mut [u8], lanes: LaneTable) {
    output[lanes.promise_bits..lanes.promise_ranks].fill(0);
    output[lanes.promise_ranks..lanes.providers].fill(0);
    output[lanes.overlay_bits..lanes.overlay_ranks].fill(0);
    output[lanes.overlay_ranks..lanes.present].fill(0);
}

struct WriteCursor {
    exception: u32,
    promise: u32,
    overlay: u32,
    present: u32,
}

impl WriteCursor {
    const fn new() -> Self {
        Self {
            exception: 0,
            promise: 0,
            overlay: 0,
            present: 0,
        }
    }

    fn write_placement<DomainTag>(
        &mut self,
        output: &mut [u8],
        lanes: LaneTable,
        placement: NonResident<DomainTag>,
    ) {
        match placement {
            NonResident::Promised(providers) => {
                set_member(output, lanes.promise_bits, self.exception);
                write_u64(output, lanes.providers, self.promise, *providers);
                self.promise += 1;
            }
            NonResident::Overlaid(RemoteBase::Absent { generation: _ }) => {
                self.overlay += 1;
            }
            NonResident::Overlaid(RemoteBase::Present {
                generation: _,
                object,
            }) => {
                set_member(output, lanes.overlay_bits, self.overlay);
                write_descriptor(output, lanes.present, self.present, object);
                self.overlay += 1;
                self.present += 1;
            }
        }
    }
}

fn write_rank_directories(output: &mut [u8], lanes: LaneTable, layout: &LocalityLayout) {
    write_rank_directory(
        output,
        lanes.promise_bits,
        lanes.promise_ranks,
        lanes.providers,
        u32::from(layout.exceptions),
    );
    write_rank_directory(
        output,
        lanes.overlay_bits,
        lanes.overlay_ranks,
        lanes.present,
        u32::from(lanes.overlay_count),
    );
}

#[allow(
    clippy::indexing_slicing,
    reason = "the measured lane table proves membership bytes precede and are disjoint from their rank directory"
)]
fn write_rank_directory(
    output: &mut [u8],
    bits_start: usize,
    ranks_start: usize,
    ranks_end: usize,
    count: u32,
) {
    let (before_ranks, after_bits) = output.split_at_mut(ranks_start);
    let bits = &before_ranks[bits_start..];
    let ranks = &mut after_bits[..ranks_end - ranks_start];
    rank::visit_prefixes(bits, count, |ordinal, prefix| {
        write_u32_lane(ranks, ordinal, prefix);
    });
}

#[allow(
    clippy::indexing_slicing,
    reason = "a shared basis exists only when the measured overlay count reserved its fixed 32-byte lane"
)]
fn write_shared_basis(output: &mut [u8], lanes: LaneTable, basis: Option<GenerationId>) {
    if let Some(basis) = basis {
        output[lanes.basis..lanes.basis + size_of::<[u8; 32]>()].copy_from_slice(&*basis);
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "the output preflight accepted the exact measured layout and each ordinal is advanced once from the prepared count"
)]
fn write_row(output: &mut [u8], start: usize, ordinal: u32, row: u32) {
    let start = start + native(ordinal) * size_of::<u32>();
    output[start..start + size_of::<u32>()].copy_from_slice(&row.to_be_bytes());
}

#[allow(
    clippy::indexing_slicing,
    reason = "the output preflight accepted the exact measured layout and each promise rank is advanced once from prepared input"
)]
fn write_u64(output: &mut [u8], start: usize, ordinal: u32, value: u64) {
    let start = start + native(ordinal) * size_of::<u64>();
    output[start..start + size_of::<u64>()].copy_from_slice(&value.to_be_bytes());
}

#[allow(
    clippy::indexing_slicing,
    reason = "the output preflight accepted the exact measured layout and each descriptor rank is advanced once from prepared input"
)]
fn write_descriptor<DomainTag>(
    output: &mut [u8],
    start: usize,
    ordinal: u32,
    object: backend_version::object::ObjectRef<DomainTag>,
) {
    let record = LocalityDescriptorWireRecord::from(&object);
    let start = start + native(ordinal) * LOCALITY_DESCRIPTOR_BYTES;
    output[start..start + LOCALITY_DESCRIPTOR_BYTES].copy_from_slice(record.as_bytes());
}

#[allow(
    clippy::indexing_slicing,
    reason = "one-pass rank construction emits exactly the measured omitted-zero prefix count"
)]
fn write_u32_lane(output: &mut [u8], ordinal: u32, value: u32) {
    let start = native(ordinal) * size_of::<u32>();
    output[start..start + size_of::<u32>()].copy_from_slice(&value.to_be_bytes());
}

#[allow(
    clippy::indexing_slicing,
    reason = "the output preflight accepted the exact measured layout and each exception ordinal is within its membership lane"
)]
fn set_member(output: &mut [u8], start: usize, ordinal: u32) {
    let byte = start + native(ordinal / u8::BITS);
    output[byte] |= 1_u8 << (ordinal % u8::BITS);
}

#[allow(
    clippy::as_conversions,
    reason = "the root target gate admits compact u32 locality ordinals as native output coordinates"
)]
const fn native(ordinal: u32) -> usize {
    ordinal as usize
}
