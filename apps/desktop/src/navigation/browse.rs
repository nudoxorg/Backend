//! Routes of the browsing pages: your tree now; find and compare next.
//!
//! One `OrbitRoute::Browse` variant carries them all, so the shell learns a new
//! browsing page by a new [`BrowseRoute`] variant here, not by another arm in
//! every match over [`super::Route`].

use crate::core::LocalProjectId;

/// A browsing page.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BrowseRoute {
    /// A project's dependency tree (`nudox://backend/tree`).
    Tree(LocalProjectId),
}

impl BrowseRoute {
    /// The status bar address: path segments, then the place's own name.
    #[must_use]
    pub fn address(&self) -> (Vec<String>, String) {
        match self {
            Self::Tree(project) => (vec![project_name(project)], "tree".to_owned()),
        }
    }

    /// The titlebar's "here": the place's name, then where it is.
    #[must_use]
    pub fn here(&self) -> (String, &'static str) {
        match self {
            Self::Tree(project) => (project_name(project), "your tree"),
        }
    }
}

/// A project's name as people say it: its folder's name.
fn project_name(project: &LocalProjectId) -> String {
    project
        .path()
        .file_name()
        .map_or_else(|| project.display_lossy(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_is_addressed_by_its_project_folder() {
        let project = LocalProjectId::new("/workspace/backend").expect("project");
        let route = BrowseRoute::Tree(project);
        assert_eq!(route.address(), (vec!["backend".to_owned()], "tree".to_owned()));
        assert_eq!(route.here(), ("backend".to_owned(), "your tree"));
    }
}
