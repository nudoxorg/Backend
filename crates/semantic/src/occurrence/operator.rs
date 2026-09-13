use crate::{Edge, EdgeKind, OccurrenceFact};
use backend_flow::{
    ArrangementRoot, Delta as FlowDelta, FlowError, Frontier, IncrementalOperator, Microbatch,
    PreparedOutput, RowRef,
};
use backend_version::CoverageWitness;

/// One incremental occurrence-to-edge operator using the backend-flow
/// lending/output contract.
pub struct OccurrenceOperator {
    coverage: CoverageWitness,
}

impl OccurrenceOperator {
    /// Creates an operator bound to an authority witness.
    #[must_use]
    pub const fn new(coverage: CoverageWitness) -> Self {
        Self { coverage }
    }

    /// Returns the witness bound at construction.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }
}

impl IncrementalOperator for OccurrenceOperator {
    type Input = OccurrenceFact;
    type Output = Edge;
    type Batch<'a> = std::iter::Map<
        std::slice::Iter<'a, FlowDelta<Edge>>,
        fn(&'a FlowDelta<Edge>) -> RowRef<'a, Edge>,
    >;

    fn prepare<'a>(
        &'a mut self,
        input: &'a Microbatch<Self::Input>,
        base: ArrangementRoot<Self::Output>,
        target: ArrangementRoot<Self::Output>,
        frontier: Frontier,
        coverage: CoverageWitness,
    ) -> Result<PreparedOutput<Self::Output>, FlowError> {
        if !matches!(coverage, CoverageWitness::Complete(_)) || coverage != self.coverage {
            return Err(FlowError::IncompleteFrontier);
        }
        let mut deltas = Vec::new();
        for row in input.cursor() {
            let support = i64::from(row.value.support);
            let diff = support
                .checked_mul(row.diff.value())
                .ok_or(FlowError::Overflow)?;
            deltas.push(FlowDelta::checked(
                row.key,
                Edge {
                    from: row.value.source,
                    to: row.value.target,
                    kind: EdgeKind::Reference,
                    multiplicity: row.value.multiplicity,
                    support: row.value.support,
                },
                row.time,
                diff,
            )?);
        }
        PreparedOutput::admit(base, target, frontier, coverage, deltas)
    }

    fn borrow<'a>(&'a self, output: &'a PreparedOutput<Self::Output>) -> Self::Batch<'a> {
        output.deltas().iter().map(|row| RowRef {
            key: row.key,
            value: &row.value,
            time: row.time,
            diff: row.diff,
        })
    }
}
