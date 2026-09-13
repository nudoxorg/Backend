use super::Node;
use crate::{NodeError, Relation};
use std::fmt;

/// Work performed while preparing one immutable tree update.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TreeWork {
    /// Leaf rows re-encoded for changed neighborhoods.
    pub rows: usize,
    /// Canonical leaves and branches rebuilt.
    pub nodes: usize,
    /// Existing canonical nodes visited while locating the update.
    pub visited_nodes: usize,
    /// Existing immutable child nodes retained by the new root.
    pub reused_nodes: usize,
    /// Alias for the number of newly copied canonical nodes.
    pub copied_nodes: usize,
    /// Total canonical bytes emitted by copied nodes.
    pub encoded_bytes: usize,
}
impl TreeWork {
    pub(super) fn visit(&mut self) -> Result<(), TreeError> {
        self.visited_nodes = self
            .visited_nodes
            .checked_add(1)
            .ok_or(TreeError::Overflow)?;
        Ok(())
    }
    pub(super) fn reuse(&mut self, n: usize) -> Result<(), TreeError> {
        self.reused_nodes = self
            .reused_nodes
            .checked_add(n)
            .ok_or(TreeError::Overflow)?;
        Ok(())
    }
    pub(super) fn rebuild_node<R: Relation>(&mut self, node: &Node<R>) -> Result<(), TreeError> {
        let n = 1;
        self.nodes = self.nodes.checked_add(n).ok_or(TreeError::Overflow)?;
        self.copied_nodes = self
            .copied_nodes
            .checked_add(n)
            .ok_or(TreeError::Overflow)?;
        self.encoded_bytes = self
            .encoded_bytes
            .checked_add(node.canonical.as_bytes().len())
            .ok_or(TreeError::Overflow)?;
        Ok(())
    }
    pub(super) fn rows(&mut self, n: usize) -> Result<(), TreeError> {
        self.rows = self.rows.checked_add(n).ok_or(TreeError::Overflow)?;
        Ok(())
    }
}
/// Failure while constructing or incrementally updating a persistent tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeError {
    /// Input changes were not strictly ordered or contained a duplicate key.
    UnsortedOrDuplicate,
    /// A tree root or update neighborhood was structurally invalid.
    InvalidRoot,
    /// Checked arithmetic exceeded its representable bound.
    Overflow,
    /// A canonical node constructor rejected the resulting bytes.
    Canonical(NodeError),
}
impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid persistent tree update: {self:?}")
    }
}
impl std::error::Error for TreeError {}
