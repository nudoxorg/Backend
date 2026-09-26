//! Coverage witness constructors and identity checks.

use super::{
    ClosedRelationScope, Coverage, CoverageWitness, ProducerObservationIdentity, ScopeRoot,
    UntrustedCompleteCoverage, UntrustedCoverageScope,
};
use crate::ids::ID_BYTES;

impl CoverageWitness {
    /// Creates a complete local relation witness without authority provenance.
    ///
    /// Closed witnesses may prepare and apply exact relation deltas, while
    /// checked workspace admission continues to require [`Self::Complete`].
    #[must_use]
    pub const fn closed_relation(scope: ClosedRelationScope) -> Self {
        Self::Closed(scope)
    }

    /// Returns the public coverage state represented by this witness.
    #[must_use]
    pub const fn state(self) -> Coverage {
        match self {
            Self::Complete(_) | Self::UntrustedComplete(_) => Coverage::Complete,
            Self::Closed(_) => Coverage::Closed,
            Self::Partial(_) => Coverage::Partial,
            Self::Unavailable(_) => Coverage::Unavailable,
            Self::Unsupported(_) => Coverage::Unsupported,
        }
    }

    /// Returns whether independently checked producer claims authorized this
    /// complete scope.
    ///
    /// This is deliberately stronger than [`Coverage::is_complete`], which
    /// also describes untrusted wire labels awaiting admission.
    #[must_use]
    pub const fn is_authorized_complete(self) -> bool {
        matches!(self, Self::Complete(_))
    }

    /// Returns the exact scope retained by this witness.
    #[must_use]
    pub const fn scope_root(self) -> ScopeRoot {
        match self {
            Self::Complete(complete) => complete.scope_root(),
            Self::UntrustedComplete(complete) => complete.scope_root(),
            Self::Closed(closed) => closed.scope_root(),
            Self::Partial(partial) | Self::Unavailable(partial) | Self::Unsupported(partial) => {
                partial.scope_root()
            }
        }
    }

    /// Returns whether the complete scope crossed the admitted producer
    /// boundary.
    #[must_use]
    pub(crate) const fn is_bound(self) -> bool {
        match self {
            Self::Complete(_) => true,
            Self::Closed(_)
            | Self::UntrustedComplete(_)
            | Self::Partial(_)
            | Self::Unavailable(_)
            | Self::Unsupported(_) => false,
        }
    }

    /// Returns whether local delta algebra may operate on this state.
    #[must_use]
    pub(crate) const fn is_delta_eligible(self) -> bool {
        matches!(self, Self::Complete(_) | Self::Closed(_))
    }

    /// Compares only the public coverage state and scope.  This is used when
    /// a checked manifest is reconstructed from an untrusted wire summary;
    /// the admission marker is supplied separately by the typed witness.
    #[must_use]
    pub(crate) fn same_identity(self, other: Self) -> bool {
        self.state() == other.state()
            && self.scope_root().0 == other.scope_root().0
            && self.complete_identity() == other.complete_identity()
    }

    pub(crate) fn complete_identity(self) -> Option<ProducerObservationIdentity> {
        match self {
            Self::Complete(complete) => Some(complete.identity),
            Self::UntrustedComplete(complete) => Some(complete.identity),
            Self::Closed(_) | Self::Partial(_) | Self::Unavailable(_) | Self::Unsupported(_) => {
                None
            }
        }
    }

    /// Reconstructs coverage labels from a bounded untrusted wire descriptor.
    #[must_use]
    pub(crate) const fn from_untrusted_parts(
        state: Coverage,
        scope: ScopeRoot,
        complete_identity: Option<ProducerObservationIdentity>,
    ) -> Self {
        match state {
            Coverage::Complete => match complete_identity {
                Some(identity) => {
                    Self::UntrustedComplete(UntrustedCompleteCoverage { scope, identity })
                }
                None => Self::UntrustedComplete(UntrustedCompleteCoverage {
                    scope,
                    identity: ProducerObservationIdentity::from_parts(
                        [0; ID_BYTES],
                        [0; ID_BYTES],
                        [0; ID_BYTES],
                    ),
                }),
            },
            Coverage::Closed => Self::Closed(ClosedRelationScope::from_scope_root(scope)),
            Coverage::Partial => Self::Partial(UntrustedCoverageScope::from_scope_root(scope)),
            Coverage::Unavailable => {
                Self::Unavailable(UntrustedCoverageScope::from_scope_root(scope))
            }
            Coverage::Unsupported => {
                Self::Unsupported(UntrustedCoverageScope::from_scope_root(scope))
            }
        }
    }
}
