//! Reusable component layer for lindsey (GUI-PLAN §11).
//!
//! Every component in this module is a thin composition of gpui-component primitives,
//! design tokens from `theme/`, and motion helpers from `motion/`. Together they form
//! the single "component vocabulary" that every screen in the app speaks.
//!
//! # Design principles
//!
//! - **Zero inline values.** Every colour, dimension, and duration comes from a token
//!   (`cx.theme_ext()`, `cx.theme()`, or `motion::tokens`). This is the rule the
//!   whole design system exists to enforce.
//! - **Pre-computed inputs.** Components accept `SharedString` values that are
//!   already formatted; they never call `format!`, `to_string`, `sort`, or `filter`
//!   in render (GUI-PLAN §1.1.4 / §24.2).
//! - **Stable element IDs.** Every animated element uses a namespaced `ElementId`
//!   of the form `("ui.component_name", caller_id)` (LD-19).
//! - **LD-6 caller contract.** Components that are used in list contexts document
//!   the virtualization requirement; they do not enforce it internally.

pub mod badge;
pub mod count_label;
pub mod empty_state;
pub mod error_state;
pub mod key_hint;
pub mod progress_row;
pub mod provenance_dot;
pub mod section_header;
pub mod signature_line;
pub mod slot_view;
pub mod toolbar;

pub use badge::Badge;
pub use count_label::CountLabel;
pub use empty_state::EmptyState;
pub use error_state::ErrorState;
pub use key_hint::KeyHint;
pub use progress_row::ProgressRow;
pub use provenance_dot::ProvenanceDot;
pub use section_header::SectionHeader;
pub use signature_line::{SigToken, SignatureLine, SymbolKey};
pub use slot_view::{SlotContentState, slot_view};
pub use toolbar::Toolbar;
