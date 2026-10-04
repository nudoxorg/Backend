//! Closed TypeScript authority projection vocabulary.

use core::mem::size_of;

use super::projection::{ProjectionForeignKeyFault, ProjectionPackageLineageFault};

/// Closed TypeScript authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeScriptProjectionFault {
    /// A foreign target key failed grammar validation at this exact spelling.
    ForeignKey {
        /// Half-open source span of the path/display spelling.
        start: u32,
        /// Exclusive source end.
        end: u32,
        /// Exact grammar cause.
        cause: ProjectionForeignKeyFault,
    },
    /// An import-module package lineage failed grammar validation at this
    /// exact module spelling.
    PackageLineage {
        /// Half-open source span of the module spelling.
        start: u32,
        /// Exclusive source end.
        end: u32,
        /// Exact grammar cause.
        cause: ProjectionPackageLineageFault,
    },
    /// A host-size coordinate could not fit the wire's `u32` coordinate.
    CoordinateOverflow {
        /// Exact host-sized source coordinate that could not fit the wire’s u32 coordinate.
        value: u64,
    },
    /// An import binding named no retained module row.
    MissingImportBinding {
        /// Zero-based projected import fact for which the authority image supplied no resolved binding.
        fact: u32,
    },
}
impl core::fmt::Display for TypeScriptProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(size_of::<TypeScriptProjectionFault>() <= 16);
