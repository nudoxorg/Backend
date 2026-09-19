//! Read-manifest-bound graph query plans.

use super::view::GraphArrangement;
use crate::contracts::Query;
use crate::{Binding, Error, Limits};
use backend_version::CoverageWitness;

/// Query descriptor bound to an exact graph relation, coverage, and read
/// manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlan {
    pub(super) binding: Binding,
    pub(super) coverage: CoverageWitness,
    pub(super) query: Query,
}

impl QueryPlan {
    /// Creates a plan only when the query declares exactly the binding's read
    /// manifest and complete coverage witness. Query text and input roots
    /// therefore travel together.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ReadManifestDrift`] when declared reads differ from
    /// the exact binding or when query limits reject the plan.
    pub fn new(binding: Binding, coverage: CoverageWitness, query: Query) -> Result<Self, Error> {
        query.validate(Limits::default())?;
        if !matches!(
            coverage,
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        if query.read_manifest != binding.read_manifest {
            return Err(Error::ReadManifestDrift);
        }
        Ok(Self {
            binding,
            coverage,
            query,
        })
    }

    /// Returns the exact binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the exact coverage witness selected by the query.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns the canonical query descriptor.
    #[must_use]
    pub const fn query(&self) -> &Query {
        &self.query
    }

    pub(super) fn validate_against(&self, arrangement: &GraphArrangement) -> Result<(), Error> {
        self.query.validate(Limits::default())?;
        if self.binding != arrangement.binding()
            || self.coverage != arrangement.base().coverage()
            || self.query.read_manifest != self.binding.read_manifest
        {
            Err(Error::StaleRoot)
        } else {
            Ok(())
        }
    }
}
