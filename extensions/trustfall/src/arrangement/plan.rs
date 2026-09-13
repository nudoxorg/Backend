//! Checked graph delta planning and binding validation.

use super::limits::{ArrangementLimits, RefreshKind};
use crate::delta::GraphDelta;
use crate::{Error, Root};
use backend_version::CoverageWitness;
use std::mem::size_of;

/// Exact root-bound graph refresh plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArrangementPlan {
    base: Root,
    target: Root,
    changed_rows: usize,
    bytes: usize,
    kind: RefreshKind,
}

impl ArrangementPlan {
    /// Plans one checked transition against a known arrangement root.
    ///
    /// # Errors
    ///
    /// Returns an error when the delta is stale, incomplete, malformed, or
    /// its byte counters overflow.
    pub fn for_delta(
        base: Root,
        delta: &GraphDelta,
        limits: ArrangementLimits,
    ) -> Result<Self, Error> {
        let limits = limits.validate()?;
        if delta.delta.base() != base
            || delta.binding.root != delta.delta.target()
            || !delta.delta.is_canonical()
        {
            return Err(Error::StaleRoot);
        }
        if !matches!(
            delta.coverage,
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        let mut changed_rows = 0usize;
        let mut bytes = 0usize;
        for change in delta.delta.changes() {
            changed_rows = changed_rows.checked_add(1).ok_or(Error::SizeLimit)?;
            bytes = bytes
                .checked_add(size_of::<u64>().checked_mul(2).ok_or(Error::SizeLimit)?)
                .ok_or(Error::SizeLimit)?;
            if let Some(values) = &change.after {
                for value in values {
                    bytes = bytes.checked_add(value.len()).ok_or(Error::SizeLimit)?;
                }
            }
        }
        let kind = if changed_rows == 0 {
            RefreshKind::Reuse
        } else if changed_rows <= limits.max_changed_rows && bytes <= limits.max_bytes {
            RefreshKind::Overlay
        } else {
            RefreshKind::Rebuild
        };
        Ok(Self {
            base,
            target: delta.delta.target(),
            changed_rows,
            bytes,
            kind,
        })
    }

    /// Returns the exact base root.
    #[must_use]
    pub const fn base(self) -> Root {
        self.base
    }

    /// Returns the exact target root.
    #[must_use]
    pub const fn target(self) -> Root {
        self.target
    }

    /// Returns changed logical-row count.
    #[must_use]
    pub const fn changed_rows(self) -> usize {
        self.changed_rows
    }

    /// Returns the replacement-byte estimate.
    #[must_use]
    pub const fn bytes(self) -> usize {
        self.bytes
    }

    /// Returns the selected strategy.
    #[must_use]
    pub const fn kind(self) -> RefreshKind {
        self.kind
    }

    pub(super) const fn requiring_rebuild(self) -> Self {
        Self {
            kind: RefreshKind::Rebuild,
            ..self
        }
    }
}

pub(super) fn validate_delta(
    binding: crate::Binding,
    coverage: CoverageWitness,
    delta: &GraphDelta,
) -> Result<(), Error> {
    if !matches!(
        delta.coverage,
        CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
    ) {
        return Err(Error::IncompleteCoverage);
    }
    if delta.coverage != coverage
        || delta.binding.workspace != binding.workspace
        || delta.binding.recipe != binding.recipe
        || delta.binding.authority != binding.authority
        || delta.binding.read_manifest != binding.read_manifest
        || delta.binding.frontier != binding.frontier
        || delta.delta.base() != binding.root
        || delta.binding.root != delta.delta.target()
        || !delta.delta.is_canonical()
    {
        return Err(Error::StaleRoot);
    }
    Ok(())
}
