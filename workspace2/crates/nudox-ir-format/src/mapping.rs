//! Read-only file-backed ownership for a completely manifest-validated IR fragment.

use core::{num::TryFromIntError, ops::Deref};
use std::{fs::File, io, path::Path};

use memmap2::{Mmap, MmapOptions};
use thiserror::Error;

use crate::{FragmentRangeManifest, FragmentRangeVerifyError, FragmentView, wire::FragmentLayout};

/// Immutable facts of one complete manifest-validated mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappedFragmentView {
    /// Exact complete-fragment and section commitments checked during mapping.
    pub manifest: FragmentRangeManifest,
    /// Exact metadata length used to bound this operating-system mapping.
    pub mapped_bytes: usize,
}

/// Read-only mapping that owns bytes and their once-validated compact fragment layout.
pub struct MappedFragment {
    view: MappedFragmentView,
    mapping: Mmap,
    layout: FragmentLayout,
}

impl Deref for MappedFragment {
    type Target = MappedFragmentView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl AsRef<[u8]> for MappedFragment {
    fn as_ref(&self) -> &[u8] {
        &self.mapping
    }
}

impl MappedFragment {
    /// Borrows compact IR directly from the once-validated mapping without copying or reparsing.
    #[must_use]
    pub fn view(&self) -> FragmentView<'_> {
        FragmentView::from_validated_layout(&self.mapping, self.layout)
    }
}

/// Filesystem phase retaining its original I/O source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappedFragmentIoPhase {
    /// Opening the immutable content-addressed fragment descriptor.
    Open,
    /// Reading the descriptor metadata used to set one exact mapping length.
    Metadata,
    /// Creating the read-only operating-system mapping.
    Map,
}

/// Failure before a manifest-validated mapping can lend compact IR views.
#[derive(Debug, Error)]
pub enum MappedFragmentError {
    /// One named filesystem operation retained its exact I/O cause.
    #[error("mapped fragment {phase:?} failed")]
    Io {
        /// Exact filesystem phase.
        phase: MappedFragmentIoPhase,
        /// Original operating-system error.
        #[source]
        source: io::Error,
    },
    /// File metadata length cannot fit this process's address space.
    #[error("mapped fragment length {observed} exceeds this process address space")]
    FileLength {
        /// Exact descriptor byte length.
        observed: u64,
        /// Original checked conversion error.
        #[source]
        source: TryFromIntError,
    },
    /// Zero-length files never contain a compact fragment header and are never mapped.
    #[error("mapped fragment file is empty")]
    EmptyFile,
    /// Complete mapping bytes failed the typed manifest or compact-fragment grammar.
    #[error("mapped fragment validation failed")]
    Validation(#[source] FragmentRangeVerifyError),
}

/// Opens a read-only mapping and proves its full manifest and fragment grammar before lending it.
///
/// The exact descriptor metadata length bounds the mapping. The returned owner stores the mapping,
/// manifest, and once-validated layout, so [`MappedFragment::view`] only reconstructs borrows of
/// the same bytes; it never hashes, reparses, copies, or allocates semantic data.
///
/// # Safety
///
/// `path` must name an immutable inode whose contents and length will not change until the returned
/// [`MappedFragment`] is dropped. Opening a descriptor read-only cannot prevent a second descriptor
/// or process from modifying or replacing its file. Call this only for an immutable
/// content-addressed artifact published to a new path; never mutate or replace that inode while
/// the mapping is live.
#[allow(
    unsafe_code,
    reason = "the caller's immutable-inode contract, checked nonzero metadata length, read-only descriptor, and owner-held Mmap are verified at this single boundary"
)]
pub unsafe fn open_fragment_mmap(
    path: impl AsRef<Path>,
    manifest: FragmentRangeManifest,
) -> Result<MappedFragment, MappedFragmentError> {
    let file = File::open(path).map_err(|source| MappedFragmentError::Io {
        phase: MappedFragmentIoPhase::Open,
        source,
    })?;
    let file_length = file
        .metadata()
        .map_err(|source| MappedFragmentError::Io {
            phase: MappedFragmentIoPhase::Metadata,
            source,
        })?
        .len();
    let mapping_length =
        usize::try_from(file_length).map_err(|source| MappedFragmentError::FileLength {
            observed: file_length,
            source,
        })?;
    if mapping_length == 0 {
        return Err(MappedFragmentError::EmptyFile);
    }
    // SAFETY: the caller guarantees the documented immutable-inode contract; this maps exactly
    // the checked nonzero descriptor metadata length read above, retains a read-only descriptor
    // mapping in `MappedFragment`, and ties all fragment borrows to `&MappedFragment`.
    let mapping = unsafe { MmapOptions::new().len(mapping_length).map(&file) }.map_err(|source| {
        MappedFragmentError::Io {
            phase: MappedFragmentIoPhase::Map,
            source,
        }
    })?;
    let layout = manifest
        .verify_fragment_layout(&mapping)
        .map_err(MappedFragmentError::Validation)?;
    Ok(MappedFragment {
        view: MappedFragmentView {
            manifest,
            mapped_bytes: mapping_length,
        },
        mapping,
        layout,
    })
}
