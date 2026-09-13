//! Version-bound graph arrangement facade.
//!
//! The child modules separate maintenance budgets and plans, immutable rows,
//! replacement overlays, query-plan identity, and bounded execution.  They
//! all consume the checked graph delta and binding from the extension root.

mod base;
mod limits;
mod overlay;
mod plan;
mod query;
mod view;

pub use base::GraphBase;
pub use limits::{ArrangementLimits, RefreshKind};
pub use overlay::GraphOverlay;
pub use plan::ArrangementPlan;
pub use query::QueryPlan;
pub use view::{GraphArrangement, RefreshOutcome};
