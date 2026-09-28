//! Read models of the browsing pages.
//!
//! The words come from `backend-present` ([`backend_present::TreeReading`]),
//! so the Library page says exactly what `backend project-tree` prints.

use crate::core::LocalProjectId;
use crate::navigation::BrowseRoute;
use std::fmt;
use std::sync::Arc;

/// Identity of one browsing resource.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BrowseKey {
    /// A project's dependency tree.
    Tree(LocalProjectId),
}

impl From<&BrowseRoute> for BrowseKey {
    fn from(route: &BrowseRoute) -> Self {
        match route {
            BrowseRoute::Tree(project) => Self::Tree(project.clone()),
        }
    }
}

impl fmt::Display for BrowseKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tree(project) => write!(formatter, "tree {}", project.display_lossy()),
        }
    }
}

/// One browsing resource's value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowseValue {
    /// A project's tree, read.
    Tree(Arc<TreeModel>),
}

impl BrowseValue {
    /// The tree, when this value is one.
    #[must_use]
    pub fn tree(&self) -> Option<&TreeModel> {
        match self {
            Self::Tree(tree) => Some(tree),
        }
    }
}

/// A project's tree as the Library page shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeModel {
    /// The workspace root the tree was read at.
    pub root: Arc<str>,
    /// Every sentence of the page.
    pub reading: backend_present::TreeReading,
}
