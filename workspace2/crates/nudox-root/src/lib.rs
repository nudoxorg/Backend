#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
//! Canonical packed generation roots, closure selection, and structural diffs.

extern crate alloc;

mod builder;
mod closure;
mod diff;
mod encode;
mod entry;
mod locality;
mod overlay;
mod packed;
mod root_view;

pub use builder::{
    GenerationRootBuilder, RejectedRootEntry, RootBuildError, RootBuildStage, RootPushError,
    RootWriteError,
};
pub use closure::{
    ClosureError, ClosureScratch, ClosureScratchFacts, RootProbeEvent, SelectedClosure,
    SelectionWork,
};
pub use diff::{ChangedRootDiff, RootChange, RootDiff, RootDiffMetrics};
pub use entry::{EntryKey, EntryRange, EntryRangeBounds, EntryRangeError, RootEntry};
pub use locality::{
    GenerationEntry, GenerationScan, GenerationView, Locality, LocalityError, LocalityException,
    LocalityLayout, LocalityLayoutView, LocalityLookupWork, LocalityRow, LocalityScanWork,
    LocalityValidator, LocalityWriteError, MeasuredGenerationLookup, MeasuredGenerationScan,
    NonResident, PreparedLocality, SelectedCount, SelectedGeneration, SelectedOrdinalBuffer,
    SelectedOrdinalBufferError, SelectedOrdinals, ValidatedLocality, ValidatedLocalityFacts,
    with_validated_locality,
};
pub use overlay::{OverlayBuildWork, OverlayError, propagate_overlays};
pub use packed::{
    GenerationRoot, GenerationRootFacts, HierarchyDepth, MetadataBytes, RootEntryCount,
    RootRowPhase,
};
pub use root_view::{
    BorrowedGenerationScan, BorrowedGenerationView, BorrowedRootFacts, BorrowedSelectedGeneration,
    BorrowedSelectedScan, RootReadError, ValidatedRoot,
};

#[cfg(test)]
mod tests;
