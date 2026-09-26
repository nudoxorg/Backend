//! Closed Python authority projection vocabulary.

use super::projection::{ProjectionForeignKeyFault, ProjectionPackageLineageFault};

/// Closed Python authority projection terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PythonProjectionFault {
    /// An authority spelling that must become a foreign key was not UTF-8.
    ForeignSpellingUtf8 {
        /// Inclusive source start.
        start: u32,
        /// Exclusive source end.
        end: u32,
    },
    /// A foreign target key failed grammar validation at this exact spelling.
    ForeignKey {
        /// Inclusive source start.
        start: u32,
        /// Exclusive source end.
        end: u32,
        /// Exact grammar cause.
        cause: ProjectionForeignKeyFault,
    },
    /// A foreign package lineage failed grammar validation at this exact
    /// module spelling.
    PackageLineage {
        /// Inclusive source start.
        start: u32,
        /// Exclusive source end.
        end: u32,
        /// Exact grammar cause.
        cause: ProjectionPackageLineageFault,
    },
    /// A declaration row could not be bound to its computed lexical owner.
    Containment {
        /// Inclusive source start of the child row.
        start: u32,
        /// Exclusive source end of the child row.
        end: u32,
    },
}
impl core::fmt::Display for PythonProjectionFault {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
const _: () = assert!(core::mem::size_of::<PythonProjectionFault>() <= 16);
