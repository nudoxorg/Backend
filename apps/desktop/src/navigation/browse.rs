//! Routes of the browsing pages: your tree now; find and compare next.
//!
//! One `OrbitRoute::Browse` variant carries them all, so the shell learns a new
//! browsing page by a new [`BrowseRoute`] variant here, not by another arm in
//! every match over [`super::Route`].

use crate::core::LocalProjectId;
use crate::model::pages::{PackageRef, SearchQuery};
use std::sync::Arc;

/// A bounded, ordered set of distinct package identities to compare.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CompareSet(Arc<[PackageRef]>);

impl CompareSet {
    /// Admit two to four distinct packages without silently changing their order.
    pub fn new(packages: impl IntoIterator<Item = PackageRef>) -> Result<Self, CompareError> {
        let mut unique = Vec::with_capacity(4);
        for package in packages {
            if unique.contains(&package) {
                return Err(CompareError::Duplicate);
            }
            if unique.len() == 4 {
                return Err(CompareError::TooMany);
            }
            unique.push(package);
        }
        if unique.len() < 2 {
            return Err(CompareError::TooFew);
        }
        Ok(Self(unique.into()))
    }

    /// Packages in the reader's selected order.
    #[must_use]
    pub fn packages(&self) -> &[PackageRef] { &self.0 }
}

/// Invalid comparison selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareError {
    /// Select at least two packages.
    TooFew,
    /// Four packages is the comparison's reading bound.
    TooMany,
    /// A package cannot be compared with itself.
    Duplicate,
}

/// A browsing page.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BrowseRoute {
    /// A project's dependency tree (`nudox://backend/tree`).
    Tree(LocalProjectId),
    /// Find before a query is entered.
    FindHome,
    /// Indexed answers, with a stable query identity.
    Find(SearchQuery),
    /// Side-by-side evidence for two to four packages.
    Compare(CompareSet),
}

impl BrowseRoute {
    /// The status bar address: path segments, then the place's own name.
    #[must_use]
    pub fn address(&self) -> (Vec<String>, String) {
        match self {
            Self::Tree(project) => (vec![project_name(project)], "tree".to_owned()),
            Self::FindHome => (vec![], "find".to_owned()),
            Self::Find(query) => (vec![], format!("find?q={}", query_component(&query.text))),
            Self::Compare(selection) => (vec![], format!("compare?packages={}", selection.packages().iter().map(|package| query_component(package.as_str())).collect::<Vec<_>>().join(","))),
        }
    }

    /// The titlebar's "here": the place's name, then where it is.
    #[must_use]
    pub fn here(&self) -> (String, &'static str) {
        match self {
            Self::Tree(project) => (project_name(project), "your tree"),
            Self::FindHome => ("Find".to_owned(), "packages and possibilities"),
            Self::Find(query) => (query.text.to_string(), "find"),
            Self::Compare(_) => ("Packages".to_owned(), "compare"),
        }
    }
}

fn query_component(value: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
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

    #[test]
    fn find_addresses_escape_query_delimiters_and_unicode() {
        let route = BrowseRoute::Find(SearchQuery::new("a & b/λ?", 50).unwrap());
        assert_eq!(route.address(), (vec![], "find?q=a%20%26%20b%2F%CE%BB%3F".into()));
    }

    #[test]
    fn comparisons_preserve_order_and_reject_ambiguous_or_unbounded_selections() {
        let packages = (0..5).map(|at| PackageRef::parse(&format!("pkg:cargo/example{at}@1.0.0")).unwrap()).collect::<Vec<_>>();
        assert_eq!(CompareSet::new([]), Err(CompareError::TooFew));
        assert_eq!(CompareSet::new(packages[..1].iter().cloned()), Err(CompareError::TooFew));
        assert_eq!(CompareSet::new([packages[0].clone(), packages[0].clone()]), Err(CompareError::Duplicate));
        assert_eq!(CompareSet::new(packages.iter().cloned()), Err(CompareError::TooMany));
        let reversed = [packages[2].clone(), packages[0].clone()];
        assert_eq!(CompareSet::new(reversed.clone()).unwrap().packages(), &reversed);
    }
}
