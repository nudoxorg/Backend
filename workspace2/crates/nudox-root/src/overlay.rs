//! Direct sparse overlay propagation with caller-owned ancestry marks.

use core::{iter::Peekable, mem::size_of_val};

use nudox_id::GenerationId;
use nudox_object::{ObjectRef, RemoteBase};
use thiserror::Error;

use crate::locality::LocalityEncoder;
use crate::packed::RowIndex;
use crate::{
    EntryKey, GenerationEntry, GenerationRoot, GenerationView, Locality, LocalityError,
    LocalityLayout, LocalityReadError, LocalityWriteError, MetadataBytes, NonResident, RootEntry,
    ValidatedLocality,
};

/// Overlay-propagation rejection without semantic-root mutation.
#[derive(Debug, Error)]
pub enum OverlayError<DomainTag> {
    /// Existing locality cannot be coherently composed with the supplied root.
    #[error("could not compose root and locality")]
    Locality(#[source] LocalityError),
    /// Validated locality lanes could not reconstruct one exact root entry.
    #[error("could not read validated locality")]
    LocalityRead(#[from] LocalityReadError),
    /// Caller mark scratch cannot cover every root coordinate before mutation.
    #[error("overlay marks have {available:?} bytes but require {required:?}")]
    MarkScratchTooSmall {
        /// Required root-sized mark backing.
        required: MetadataBytes,
        /// Supplied mark backing.
        available: MetadataBytes,
    },
    /// An existing overlay named a remote generation other than `base`.
    #[error("overlay {key:?} named base {actual:?}, expected {expected:?}")]
    BaseGenerationMismatch {
        /// Local semantic key.
        key: EntryKey,
        /// Named remote generation.
        actual: GenerationId,
        /// Supplied base generation.
        expected: GenerationId,
    },
    /// Existing remote present evidence disagrees with the supplied base root.
    #[error("overlay {key:?} expected {expected:?}, base held {actual:?}")]
    BaseObjectMismatch {
        /// Local semantic key.
        key: EntryKey,
        /// Descriptor named by existing overlay evidence.
        expected: ObjectRef<DomainTag>,
        /// Descriptor observed in the supplied base, if any.
        actual: Option<ObjectRef<DomainTag>>,
    },
    /// Existing overlay claims remote absence where the base has an object.
    #[error("overlay {key:?} claimed absent base that held {actual:?}")]
    ExpectedAbsentBase {
        /// Local semantic key.
        key: EntryKey,
        /// Descriptor observed in the supplied base.
        actual: ObjectRef<DomainTag>,
    },
    /// Final direct locality output is shorter than its measured exact layout.
    #[error("could not write propagated locality")]
    Output(#[source] LocalityWriteError),
    /// Caller mark plus final artifact accounting overflowed host metadata bytes.
    #[error("overlay propagation metadata byte accounting overflowed")]
    MetadataByteOverflow,
}

/// Exact caller scratch and final locality artifact bytes used by propagation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayBuildWork {
    /// Root-sized caller mark backing bytes.
    pub mark_bytes: MetadataBytes,
    /// Exact final canonical locality artifact bytes.
    pub final_metadata_bytes: MetadataBytes,
    /// Mark backing plus final output simultaneous byte peak.
    pub peak_live_bytes: MetadataBytes,
}

/// Validates existing overlays, propagates their ancestry, and writes the
/// final ordered sparse artifact directly into `output`.
///
/// # Errors
///
/// Returns scratch-capacity, source-overlay evidence, layout, or preflight
/// output rejection. A short `output` is rejected before it changes any byte.
#[allow(
    clippy::result_large_err,
    reason = "base mismatch intentionally retains exact inline object evidence without allocating an error path"
)]
pub fn propagate_overlays<'output, DomainTag>(
    view: &GenerationView<'_, '_, DomainTag>,
    base: &GenerationRoot<DomainTag>,
    marks: &mut [bool],
    output: &'output mut [u8],
) -> Result<(ValidatedLocality<'output, DomainTag>, OverlayBuildWork), OverlayError<DomainTag>> {
    let mut marks = OverlayMarks::new(view, marks)?;
    validate_and_mark(view, base, &mut marks)?;
    let counts = count_output(view, base, &marks)?;
    let layout = LocalityLayout::from_counts(counts.exceptions, counts.promises, counts.present)
        .map_err(OverlayError::Locality)?;
    let final_metadata_bytes = layout.bytes();
    let required = usize::from(final_metadata_bytes);
    if output.len() < required {
        return Err(OverlayError::Output(LocalityWriteError {
            required: final_metadata_bytes,
            available: output.len().into(),
        }));
    }
    #[allow(
        clippy::indexing_slicing,
        reason = "the immediately preceding exact preflight proves this direct-output prefix exists before the encoder mutates it"
    )]
    let output = &mut output[..required];
    let basis = (counts.exceptions != counts.promises).then_some(base.id);
    let mut encoder = LocalityEncoder::new(output, view.id, view.entry_count, layout, basis);
    write_output(view, base, &marks, &mut encoder)?;
    let locality = encoder.finish();
    let mark_bytes = marks.bytes();
    let peak_live_bytes = mark_bytes
        .checked_combined(final_metadata_bytes)
        .ok_or(OverlayError::MetadataByteOverflow)?;
    Ok((
        locality,
        OverlayBuildWork {
            mark_bytes,
            final_metadata_bytes,
            peak_live_bytes,
        },
    ))
}

struct OverlayMarks<'marks> {
    values: &'marks mut [bool],
}

impl<'marks> OverlayMarks<'marks> {
    #[allow(
        clippy::result_large_err,
        reason = "the exact inline overlay mismatch evidence is carried by the shared rejection type without error-path allocation"
    )]
    fn new<DomainTag>(
        view: &GenerationView<'_, '_, DomainTag>,
        values: &'marks mut [bool],
    ) -> Result<Self, OverlayError<DomainTag>> {
        if values.len() < view.len() {
            return Err(OverlayError::MarkScratchTooSmall {
                required: view.len().into(),
                available: size_of_val(values).into(),
            });
        }
        let (values, _) = values.split_at_mut(view.len());
        values.fill(false);
        Ok(Self { values })
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "only private RowIndex values from this exact root index caller scratch preflighted to root length"
    )]
    fn insert(&mut self, index: RowIndex) -> bool {
        let mark = &mut self.values[index.array_index()];
        let already_marked = *mark;
        *mark = true;
        !already_marked
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "only private RowIndex values from this exact root index caller scratch preflighted to root length"
    )]
    fn contains(&self, index: RowIndex) -> bool {
        self.values[index.array_index()]
    }

    fn bytes(&self) -> MetadataBytes {
        size_of_val(self.values).into()
    }
}

#[allow(
    clippy::result_large_err,
    reason = "base comparison deliberately preserves exact inline descriptor mismatch evidence"
)]
fn validate_and_mark<DomainTag>(
    view: &GenerationView<'_, '_, DomainTag>,
    base: &GenerationRoot<DomainTag>,
    marks: &mut OverlayMarks<'_>,
) -> Result<(), OverlayError<DomainTag>> {
    let mut base_rows = base.closure().peekable();
    for item in view.root_rows() {
        let (index, entry) = item?;
        if let Locality::Overlaid(remote) = entry.locality {
            validate_overlay(&entry, remote, base.id, &mut base_rows)?;
            mark_ancestors(view, marks, index);
        }
    }
    Ok(())
}

fn mark_ancestors<DomainTag>(
    root: &GenerationRoot<DomainTag>,
    marks: &mut OverlayMarks<'_>,
    start: RowIndex,
) {
    let mut current = Some(start);
    while let Some(index) = current {
        if !marks.insert(index) {
            return;
        }
        current = root.parent_index(index);
    }
}

#[derive(Clone, Copy)]
struct OutputCounts {
    exceptions: u32,
    promises: u32,
    present: u32,
}

#[allow(
    clippy::result_large_err,
    reason = "validated locality reconstruction preserves exact source and ordinal through the shared overlay rejection"
)]
fn count_output<DomainTag>(
    view: &GenerationView<'_, '_, DomainTag>,
    base: &GenerationRoot<DomainTag>,
    marks: &OverlayMarks<'_>,
) -> Result<OutputCounts, OverlayError<DomainTag>> {
    let mut counts = OutputCounts {
        exceptions: 0,
        promises: 0,
        present: 0,
    };
    let mut base_rows = base.closure().peekable();
    for item in view.root_rows() {
        let (index, entry) = item?;
        if marks.contains(index) {
            counts.exceptions += 1;
            if base_object(entry.key, &mut base_rows).is_some() {
                counts.present += 1;
            }
        } else if matches!(entry.locality, Locality::Promised(_)) {
            counts.exceptions += 1;
            counts.promises += 1;
        }
    }
    Ok(counts)
}

#[allow(
    clippy::result_large_err,
    reason = "validated locality reconstruction preserves exact source and ordinal through the shared overlay rejection"
)]
fn write_output<DomainTag>(
    view: &GenerationView<'_, '_, DomainTag>,
    base: &GenerationRoot<DomainTag>,
    marks: &OverlayMarks<'_>,
    encoder: &mut LocalityEncoder<'_, DomainTag>,
) -> Result<(), OverlayError<DomainTag>> {
    let mut base_rows = base.closure().peekable();
    for item in view.root_rows() {
        let (index, entry) = item?;
        if marks.contains(index) {
            encoder.emit(
                index,
                NonResident::Overlaid(remote_base(entry.key, base.id, &mut base_rows)),
            );
        } else if let Locality::Promised(providers) = entry.locality {
            encoder.emit(index, NonResident::Promised(providers));
        }
    }
    Ok(())
}

fn remote_base<DomainTag, Rows>(
    key: EntryKey,
    generation: GenerationId,
    rows: &mut Peekable<Rows>,
) -> RemoteBase<DomainTag>
where
    Rows: Iterator<Item = RootEntry<DomainTag>>,
{
    match base_object(key, rows) {
        Some(object) => RemoteBase::Present { generation, object },
        None => RemoteBase::Absent { generation },
    }
}

fn base_object<DomainTag, Rows>(
    key: EntryKey,
    rows: &mut Peekable<Rows>,
) -> Option<ObjectRef<DomainTag>>
where
    Rows: Iterator<Item = RootEntry<DomainTag>>,
{
    loop {
        let relation = key.cmp(&rows.peek()?.key);
        match relation {
            core::cmp::Ordering::Less => return None,
            core::cmp::Ordering::Equal => return rows.next().map(|entry| entry.object),
            core::cmp::Ordering::Greater => {
                rows.next();
            }
        }
    }
}

#[allow(
    clippy::result_large_err,
    reason = "base comparison deliberately preserves exact inline descriptor mismatch evidence"
)]
fn validate_overlay<DomainTag, Rows>(
    entry: &GenerationEntry<DomainTag>,
    remote: RemoteBase<DomainTag>,
    base_generation: GenerationId,
    base_rows: &mut Peekable<Rows>,
) -> Result<(), OverlayError<DomainTag>>
where
    Rows: Iterator<Item = RootEntry<DomainTag>>,
{
    let (actual_generation, expected_object) = match remote {
        RemoteBase::Present { generation, object } => (generation, Some(object)),
        RemoteBase::Absent { generation } => (generation, None),
    };
    if actual_generation != base_generation {
        return Err(OverlayError::BaseGenerationMismatch {
            key: entry.key,
            actual: actual_generation,
            expected: base_generation,
        });
    }
    match (expected_object, base_object(entry.key, base_rows)) {
        (Some(expected), Some(actual)) if expected == actual => Ok(()),
        (Some(expected), actual) => Err(OverlayError::BaseObjectMismatch {
            key: entry.key,
            expected,
            actual,
        }),
        (None, None) => Ok(()),
        (None, Some(actual)) => Err(OverlayError::ExpectedAbsentBase {
            key: entry.key,
            actual,
        }),
    }
}
