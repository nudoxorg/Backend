//! Defines outline behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the outline invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The package outline: the containment tree every navigator, tree view, and table of contents shares.

use interface_identity::PackageCoordinate;

use crate::{Census, Symbol, Text};

/// One outline node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineNode {
    /// Identity header.
    pub symbol: Symbol,
    /// First documentation line.
    pub summary: Option<Text>,
    /// Children in retained member order.
    pub children: Box<[OutlineNode]>,
}

impl OutlineNode {
    /// Counts this node and every descendant.
    #[must_use]
    pub fn size(&self) -> usize {
        1 + self.children.iter().map(Self::size).sum::<usize>()
    }
}

/// One package's complete containment tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outline {
    /// Package the tree belongs to.
    pub package: PackageCoordinate,
    /// Root declarations in canonical order.
    pub roots: Box<[OutlineNode]>,
    /// Declaration counts.
    pub census: Census,
}

impl Outline {
    /// Depth-first traversal in display order.
    pub fn walk(&self) -> impl Iterator<Item = (&OutlineNode, usize)> {
        let mut stack: Vec<(&OutlineNode, usize)> =
            self.roots.iter().rev().map(|node| (node, 0)).collect();
        core::iter::from_fn(move || {
            let (node, depth) = stack.pop()?;
            stack.extend(node.children.iter().rev().map(|child| (child, depth + 1)));
            Some((node, depth))
        })
    }
}
