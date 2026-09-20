//! Bounded lexical transition planning and binding fences.

use crate::delta::DocumentDelta;
use crate::{Error, Root};
use backend_version::CoverageWitness;
use std::collections::BTreeSet;
use std::mem::size_of;

/// Limits for incremental lexical maintenance.
///
/// These limits are a maintenance budget, rather than a semantic shortcut.
/// Exceeding one returns [`Error::RebuildRequired`], allowing the owner to
/// schedule a bounded rebuild from a complete canonical state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayLimits {
    /// Maximum changed document identities retained by one overlay.
    pub max_changed_documents: usize,
    /// Maximum distinct normalized terms retained by one overlay.
    pub max_terms: usize,
    /// Maximum estimated posting bytes retained by one overlay.
    pub max_bytes: usize,
    /// Maximum documents admitted by one rebuild operation.
    pub max_rebuild_documents: usize,
}

impl Default for OverlayLimits {
    fn default() -> Self {
        Self {
            max_changed_documents: 512,
            max_terms: 16_384,
            max_bytes: 4 * 1024 * 1024,
            max_rebuild_documents: 65_536,
        }
    }
}

impl OverlayLimits {
    /// Validates a maintenance budget before it can drive allocation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLimits`] when any bound is zero.
    pub const fn validate(self) -> Result<Self, Error> {
        if self.max_changed_documents == 0
            || self.max_terms == 0
            || self.max_bytes == 0
            || self.max_rebuild_documents == 0
        {
            Err(Error::InvalidLimits)
        } else {
            Ok(self)
        }
    }
}

/// Whether a typed transition can be maintained by the bounded overlay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshKind {
    /// The transition contains no effective changes.
    Reuse,
    /// Apply the transition to the existing exact overlay.
    Overlay,
    /// Rebuild the immutable base from the target relation.
    Rebuild,
}

/// A deterministic refresh decision tied to one exact typed delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshPlan {
    base: Root,
    target: Root,
    changed_documents: usize,
    terms: usize,
    estimated_bytes: usize,
    kind: RefreshKind,
}

impl RefreshPlan {
    /// Plans maintenance without allocating a second relation or cache.
    ///
    /// The plan inspects only the checked delta.  The caller must still pass
    /// the plan to [`crate::LexicalView::advance`] so the exact base fence is
    /// checked immediately before publication.
    ///
    /// # Errors
    ///
    /// Returns an error when the delta is stale, incomplete, malformed, or
    /// cannot be represented by the bounded counters.
    pub fn for_delta(
        base: Root,
        delta: &DocumentDelta,
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
        let mut changed_documents = 0usize;
        let mut terms = BTreeSet::new();
        let mut estimated_bytes = 0usize;
        for change in delta.delta.changes() {
            changed_documents = changed_documents.checked_add(1).ok_or(Error::SizeLimit)?;
            estimated_bytes = estimated_bytes
                .checked_add(size_of::<u64>())
                .ok_or(Error::SizeLimit)?;
            if let Some(fields) = &change.after {
                for term in terms_for(fields) {
                    estimated_bytes = estimated_bytes
                        .checked_add(term.len().checked_mul(2).ok_or(Error::SizeLimit)?)
                        .and_then(|size| size.checked_add(size_of::<u64>()))
                        .ok_or(Error::SizeLimit)?;
                    terms.insert(term);
                }
            }
        }
        let kind = if changed_documents == 0 {
            RefreshKind::Reuse
        } else if changed_documents <= limits.max_changed_documents
            && terms.len() <= limits.max_terms
            && estimated_bytes <= limits.max_bytes
        {
            RefreshKind::Overlay
        } else {
            RefreshKind::Rebuild
        };
        Ok(Self {
            base,
            target: delta.delta.target(),
            changed_documents,
            terms: terms.len(),
            estimated_bytes,
            kind,
        })
    }

    /// Returns the exact source root.
    #[must_use]
    pub const fn base(self) -> Root {
        self.base
    }

    /// Returns the exact target root.
    #[must_use]
    pub const fn target(self) -> Root {
        self.target
    }

    /// Returns the number of changed logical documents.
    #[must_use]
    pub const fn changed_documents(self) -> usize {
        self.changed_documents
    }

    /// Returns the distinct term count in the transition.
    #[must_use]
    pub const fn terms(self) -> usize {
        self.terms
    }

    /// Returns the bounded posting-byte estimate.
    #[must_use]
    pub const fn estimated_bytes(self) -> usize {
        self.estimated_bytes
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

pub(crate) fn validate_delta_binding(
    binding: crate::Binding,
    coverage: CoverageWitness,
    delta: &DocumentDelta,
) -> Result<(), Error> {
    if !matches!(
        delta.coverage,
        CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
    ) || delta.coverage != coverage
        || delta.binding.workspace != binding.workspace
        || delta.binding.recipe != binding.recipe
        || delta.binding.authority != binding.authority
        || delta.binding.read_manifest != binding.read_manifest
        || delta.binding.frontier != binding.frontier
        || delta.delta.base() != binding.root
        || delta.binding.root != delta.delta.target()
        || !delta.delta.is_canonical()
    {
        return Err(
            if matches!(
                delta.coverage,
                CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
            ) {
                Error::StaleRoot
            } else {
                Error::IncompleteCoverage
            },
        );
    }
    Ok(())
}

pub(crate) fn terms_for(fields: &[(String, String)]) -> impl Iterator<Item = String> + '_ {
    fields.iter().flat_map(|(_, value)| {
        value
            .split_ascii_whitespace()
            .filter(|term| !term.is_empty())
            .map(str::to_ascii_lowercase)
    })
}
