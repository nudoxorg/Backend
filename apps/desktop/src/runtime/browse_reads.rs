//! Reads for the browsing pages: one `project-tree` round trip, lowered to
//! the words every surface shares.

use super::reads::failure;
use crate::model::browse::{BrowseKey, BrowseValue, TreeModel};
use crate::model::pages::{PageValue, ReadFailure};
use backend_library::{ProductText, SurfaceCommand, SurfaceReply};
use backend_present::Engine;
use std::sync::Arc;

/// Reads one browsing resource.
///
/// # Errors
/// The owner's failure to read the tree, as a typed fault.
pub fn compose(engine: &mut dyn Engine, key: &BrowseKey) -> Result<PageValue, ReadFailure> {
    match key {
        BrowseKey::Tree(project) => {
            let root = project.service_coordinate().map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    format!("the project path cannot be sent to the owner: {error:?}"),
                ))
            })?;
            let root = ProductText::new(root).map_err(|error| {
                ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Protocol, error.to_string()))
            })?;
            match engine.surface(SurfaceCommand::ProjectTree { root }).map_err(|error| failure(&error))? {
                SurfaceReply::ProjectTree(tree) => Ok(PageValue::Browse(BrowseValue::Tree(Arc::new(tree_model(&tree))))),
                _ => Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Protocol,
                    "the project-tree reply changed shape",
                ))),
            }
        }
    }
}

/// The Library page's model of one tree.
#[must_use]
pub fn tree_model(tree: &backend_library::browse::ProjectTree) -> TreeModel {
    TreeModel {
        root: Arc::from(tree.root.as_str()),
        reading: backend_present::read_tree(tree),
    }
}
