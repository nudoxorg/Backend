//! Defines render behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the render invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One page traversal shared by every text renderer, so Markdown and terminal output cannot drift.
//!
//! A renderer implements [`PageVisitor`] and lets [`walk_page`] drive it. The walk order is the
//! reading order: header, trail, signature, prose, members by kind, relations by role, source.
//! Renderers never place an address or a signature inside a Markdown table cell, because cell
//! escaping corrupts the very spellings a reader copies back.

use crate::{Block, MemberGroup, Page, RelationGroup, Signature, SourceLocation, Symbol};

/// Callbacks in reading order for one page.
pub trait PageVisitor {
    /// The page's own identity header.
    fn header(&mut self, symbol: &Symbol);
    /// Ancestors from root to parent; called once with the whole trail.
    fn trail(&mut self, crumbs: &[Symbol]);
    /// The declaration signature.
    fn signature(&mut self, signature: &Signature);
    /// One documentation block, in order.
    fn prose_block(&mut self, block: &Block);
    /// One member group, in canonical kind order.
    fn members(&mut self, group: &MemberGroup);
    /// One relation group, in role order.
    fn relations(&mut self, group: &RelationGroup);
    /// The source location when retained.
    fn source(&mut self, location: &SourceLocation);
}

/// Drives a visitor over one page in reading order.
pub fn walk_page<V: PageVisitor + ?Sized>(page: &Page, visitor: &mut V) {
    visitor.header(&page.symbol);
    if !page.crumbs.is_empty() {
        visitor.trail(&page.crumbs);
    }
    if !page.signature.is_empty() {
        visitor.signature(&page.signature);
    }
    for block in page.prose.blocks() {
        visitor.prose_block(block);
    }
    for group in &page.members {
        visitor.members(group);
    }
    for group in &page.relations {
        visitor.relations(group);
    }
    if let Some(location) = &page.source {
        visitor.source(location);
    }
}
