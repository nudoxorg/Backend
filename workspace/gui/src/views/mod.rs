//! View catalog — GUI-PLAN §26.
//!
//! Each sub-module is one screen or overlay in lindsey. Files are added here as
//! each milestone delivers them; the module structure mirrors §26's target tree.
//!
//! Views are pure functions of store state (LD-1): they subscribe, they render,
//! and they report intent upward as events. They never construct clients, touch
//! channels, or spawn.

pub mod omni_search;

pub use omni_search::{
    Cursor, OmniSearch, OmniSearchEvent, OpenDisposition, PreparedRow, ScopeChip, SearchAccess,
    SearchMode, SearchSnapshot, Section, SectionData, SectionStatus,
};
