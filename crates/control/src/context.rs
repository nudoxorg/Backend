//! Audience-scoped context deltas for delegated work.
//!
//! Context is carried as a typed append-only delta.  The delta binds an exact
//! context root, and its target is derived from the ordered evidence entries;
//! an old worker cannot accidentally apply a packet to a newer context.  An
//! audience tag is part of the canonical entry, so redaction is a projection
//! of the same versioned value rather than a second mutable context store.

use crate::ControlError;
use crate::ids::{
    ContextRoot, ContextSchema, EvaluationRoot, EvaluationSchema, EvidenceRoot, Identity,
    append_version,
};

/// Maximum number of context entries admitted in one delta.
pub const MAX_CONTEXT_ITEMS: usize = 1024;

/// Recipient class encoded into each context entry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ContextAudience {
    /// Scheduler and orchestration facts.
    Controller,
    /// Facts safe to expose to the implementing agent.
    Implementer,
    /// Facts reserved for an independent reviewer.
    Reviewer,
    /// Holdout/evaluation facts that must stay outside implementer context.
    Evaluator,
}

impl ContextAudience {
    const fn tag(self) -> u8 {
        match self {
            Self::Controller => 0,
            Self::Implementer => 1,
            Self::Reviewer => 2,
            Self::Evaluator => 3,
        }
    }

    /// Returns whether this entry may be included in a projection for the
    /// requested recipient.  Holdout evaluator entries are intentionally
    /// never visible to implementers or reviewers.
    #[must_use]
    pub const fn visible_to(self, recipient: Self) -> bool {
        match recipient {
            Self::Controller => true,
            Self::Implementer => matches!(self, Self::Controller | Self::Implementer),
            Self::Reviewer => matches!(self, Self::Controller | Self::Implementer | Self::Reviewer),
            Self::Evaluator => {
                matches!(self, Self::Controller | Self::Implementer | Self::Evaluator)
            }
        }
    }
}

/// One immutable evidence reference carried in a context delta.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ContextItem {
    audience: ContextAudience,
    evidence: EvidenceRoot,
    evaluation: Option<EvaluationRoot>,
}

impl ContextItem {
    /// Creates a context item backed by an evidence root.
    #[must_use]
    pub const fn new(audience: ContextAudience, evidence: EvidenceRoot) -> Self {
        Self {
            audience,
            evidence,
            evaluation: None,
        }
    }

    /// Attaches an optional evaluator root to an evidence item.  The root is
    /// retained in the same canonical item, so benchmark provenance cannot be
    /// smuggled through an untyped side channel.
    #[must_use]
    pub const fn with_evaluation(mut self, evaluation: EvaluationRoot) -> Self {
        self.evaluation = Some(evaluation);
        self
    }

    /// Returns the entry audience.
    #[must_use]
    pub const fn audience(self) -> ContextAudience {
        self.audience
    }

    /// Returns the evidence root.
    #[must_use]
    pub const fn evidence(self) -> EvidenceRoot {
        self.evidence
    }

    /// Returns the optional evaluator/holdout root.
    #[must_use]
    pub const fn evaluation(self) -> Option<EvaluationRoot> {
        self.evaluation
    }
}

/// Exact-base context/evidence transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextDelta {
    base: ContextRoot,
    target: ContextRoot,
    items: Box<[ContextItem]>,
}

impl ContextDelta {
    /// Builds a canonical delta from an exact base and a finite set of new
    /// evidence entries.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] for an oversized entry set and
    /// [`ControlError::NonCanonicalContext`] for duplicate entries.
    pub fn new(
        base: ContextRoot,
        items: impl IntoIterator<Item = ContextItem>,
    ) -> Result<Self, ControlError> {
        let mut items = items.into_iter().collect::<Vec<_>>();
        if items.len() > MAX_CONTEXT_ITEMS {
            return Err(ControlError::Bounds);
        }
        items.sort_unstable();
        if items.windows(2).any(|window| window[0] == window[1]) {
            return Err(ControlError::NonCanonicalContext);
        }
        let target = derive_target(base, &items)?;
        Ok(Self {
            base,
            target,
            items: items.into_boxed_slice(),
        })
    }

    /// Returns the exact context root required by this delta.
    #[must_use]
    pub const fn base(&self) -> ContextRoot {
        self.base
    }

    /// Returns the context root produced by this delta.
    #[must_use]
    pub const fn target(&self) -> ContextRoot {
        self.target
    }

    /// Returns the complete canonical entry list.
    #[must_use]
    pub fn items(&self) -> &[ContextItem] {
        &self.items
    }

    /// Returns the entries admitted for one audience projection.  The
    /// iterator borrows the canonical delta and performs no allocation.
    pub fn visible_to(&self, recipient: ContextAudience) -> impl Iterator<Item = ContextItem> + '_ {
        self.items
            .iter()
            .copied()
            .filter(move |item| item.audience.visible_to(recipient))
            // An evaluator root is itself holdout metadata.  Keep the public
            // evidence reference available to an implementer/reviewer while
            // removing the optional evaluation binding from the copied
            // projection.  Filtering the entry alone is insufficient here:
            // a controller entry can carry an evaluation root as provenance.
            .map(move |item| match recipient {
                ContextAudience::Controller | ContextAudience::Evaluator => item,
                ContextAudience::Implementer | ContextAudience::Reviewer => ContextItem {
                    evaluation: None,
                    ..item
                },
            })
    }

    /// Applies the transition only to its exact base root.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::StaleRoot`] when `base` is not the root bound
    /// by this delta.
    pub fn apply_to(&self, base: ContextRoot) -> Result<ContextRoot, ControlError> {
        if base != self.base {
            return Err(ControlError::StaleRoot);
        }
        Ok(self.target)
    }

    /// Returns the canonical transition bytes used to derive the target.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] if the entry count cannot fit its
    /// canonical length field.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        canonical_bytes(self.base, &self.items)
    }
}

fn derive_target(base: ContextRoot, items: &[ContextItem]) -> Result<ContextRoot, ControlError> {
    let bytes = canonical_bytes(base, items)?;
    Identity::<ContextSchema>::from_bytes(bytes).map(|identity| identity.id())
}

fn canonical_bytes(base: ContextRoot, items: &[ContextItem]) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::with_capacity(1 + backend_version::ID_BYTES + items.len() * 42);
    bytes.push(1);
    append_version::<ContextSchema>(&mut bytes, base);
    let count = u32::try_from(items.len()).map_err(|_| ControlError::Bounds)?;
    bytes.extend_from_slice(&count.to_be_bytes());
    for item in items {
        bytes.push(item.audience.tag());
        append_version::<crate::ids::EvidenceSchema>(&mut bytes, item.evidence);
        match item.evaluation {
            Some(evaluation) => {
                bytes.push(1);
                append_version::<EvaluationSchema>(&mut bytes, evaluation);
            }
            None => bytes.push(0),
        }
    }
    Ok(bytes)
}
