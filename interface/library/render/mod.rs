//! Defines render behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the render invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Whole-reply text renderers shared by the MCP server and the CLI.
//!
//! `markdown` renders a [`crate::Reply`] for agents; `text` renders it for terminals. Both are
//! pure functions over the reply and the document model, built on
//! [`interface_documents::walk_page`], so the two surfaces cannot disagree about what a page is.
//! `common` owns the rules they share — how a target is spelled relative to a page, how a fault
//! reads, which slug names which failure — so a difference between them can only ever be typography.

pub mod common;
pub mod markdown;
pub mod text;
