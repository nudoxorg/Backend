//! Occurrence evidence, derived support, and flow operators.
//!
//! The facade exposes the three independent contracts of occurrence data:
//! persistent edge derivation, weighted ledger transitions, and the flow
//! adapter.  Each implementation lives behind a private module so storage
//! invariants cannot be confused with execution plumbing.

mod edge;
mod ledger;
mod operator;
mod work;

pub use edge::EdgeSet;
pub use ledger::OccurrenceLedger;
pub use operator::OccurrenceOperator;
pub use work::OccurrenceWorkCounters;
