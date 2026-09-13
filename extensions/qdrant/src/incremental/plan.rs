//! Root-bound vector transition planning and binding fences.

use super::limits::{OverlayLimits, RefreshKind};
use crate::delta::CandidateDelta;
use crate::{CandidateId, Error, Root};
use backend_version::CoverageWitness;
use std::mem::size_of;

/// Exact root-bound refresh plan for vector maintenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshPlan {
    base: Root,
    target: Root,
    changed_points: usize,
    bytes: usize,
    kind: RefreshKind,
}

impl RefreshPlan {
    /// Creates a plan from a checked candidate transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition is stale, incomplete, malformed,
    /// or its bounded counters overflow.
    pub fn for_delta(
        base: Root,
        delta: &CandidateDelta,
        limits: OverlayLimits,
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
        let mut changed_points = 0usize;
        let mut bytes = 0usize;
        for change in delta.delta.changes() {
            changed_points = changed_points.checked_add(1).ok_or(Error::SizeLimit)?;
            bytes = bytes
                .checked_add(
                    size_of::<CandidateId>()
                        .checked_mul(2)
                        .ok_or(Error::SizeLimit)?,
                )
                .ok_or(Error::SizeLimit)?;
            if let Some(payload) = &change.after {
                bytes = bytes.checked_add(payload.len()).ok_or(Error::SizeLimit)?;
            }
        }
        let kind = if changed_points == 0 {
            RefreshKind::Reuse
        } else if changed_points <= limits.max_changed_points && bytes <= limits.max_bytes {
            RefreshKind::Overlay
        } else {
            RefreshKind::Rebuild
        };
        Ok(Self {
            base,
            target: delta.delta.target(),
            changed_points,
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

    /// Returns changed point count.
    #[must_use]
    pub const fn changed_points(self) -> usize {
        self.changed_points
    }

    /// Returns the encoded payload estimate.
    #[must_use]
    pub const fn bytes(self) -> usize {
        self.bytes
    }

    /// Returns the selected refresh kind.
    #[must_use]
    pub const fn kind(self) -> RefreshKind {
        self.kind
    }

    pub(crate) const fn requiring_rebuild(self) -> Self {
        Self {
            kind: RefreshKind::Rebuild,
            ..self
        }
    }
}

pub(crate) fn validate_delta(
    binding: crate::Binding,
    coverage: CoverageWitness,
    delta: &CandidateDelta,
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
