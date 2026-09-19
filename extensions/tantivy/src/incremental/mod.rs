//! Version-bound lexical maintenance split by ownership boundary.
//!
//! Planning, immutable postings, exact overlays, and query execution are
//! separate modules.  They all consume the canonical [`crate::Binding`] and
//! typed [`crate::delta::DocumentDelta`] contracts; the extension never owns a
//! semantic head or invents coverage for a provider result.

mod base;
mod overlay;
mod plan;
mod view;

pub use base::LexicalBase;
pub use overlay::LexicalOverlay;
pub use plan::{OverlayLimits, RefreshKind, RefreshPlan};
pub use view::{LexicalView, RefreshOutcome};
