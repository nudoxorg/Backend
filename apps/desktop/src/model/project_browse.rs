//! Project browsing is independent of indexing readiness. A dependency
//! destination needs current ecosystem evidence for the exact local root.

use crate::core::LocalProjectId;
use crate::model::pages::{PackageDossier, PackageRef};
use crate::navigation::{BrowseRoute, OrbitRoute, Route};

/// Which dependency-tree producer the current source evidence can address.
/// This is a route affordance only: opening it still requires the Tree's
/// exact current owner receipt before any source controls are admitted.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProjectTreeCapability {
    /// No supported root-manifest evidence has been read. This includes
    /// non-Cargo projects; declaration language never supplies this evidence.
    #[default]
    Unestablished,
    /// A bounded worker read parsed this exact root's Cargo manifest.
    Cargo,
}

impl ProjectTreeCapability {
    /// The only project-tree producer currently provided is Cargo.
    #[must_use]
    pub fn destination(self, project: &LocalProjectId) -> Option<Route> {
        match self {
            Self::Unestablished => None,
            Self::Cargo => Some(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project.clone())))),
        }
    }
}

/// Callers supply current admitted resources only. Retained pages, failed
/// reads, a different folder and a same-name registry release grant nothing.
#[must_use]
pub fn project_tree_capability(
    project: &LocalProjectId,
    dossier: Option<&PackageDossier>,
) -> ProjectTreeCapability {
    let Ok(package) = PackageRef::parse(project.as_str()) else { return ProjectTreeCapability::Unestablished; };
    if let Some(dossier) = dossier.filter(|dossier| dossier.package == package)
        && dossier.project_tree == ProjectTreeCapability::Cargo {
        return ProjectTreeCapability::Cargo;
    }
    ProjectTreeCapability::Unestablished
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_exact_current_local_root_can_supply_a_cargo_destination() {
        let project = LocalProjectId::new("/fixture/fastapi-full-stack").expect("project");
        assert_eq!(project_tree_capability(&project, None), ProjectTreeCapability::Unestablished);
        let mut dossier = crate::shell::tests::dossier();
        dossier.package = PackageRef::parse("/fixture/cargo-project").expect("another project");
        dossier.project_tree = ProjectTreeCapability::Cargo;
        assert_eq!(project_tree_capability(&project, Some(&dossier)), ProjectTreeCapability::Unestablished);
        dossier.package = PackageRef::parse(project.as_str()).expect("exact root");
        assert_eq!(project_tree_capability(&project, Some(&dossier)), ProjectTreeCapability::Cargo);
        let saved = serde_json::to_string(&dossier).expect("saved dossier");
        let restored: PackageDossier = serde_json::from_str(&saved).expect("restored dossier");
        assert_eq!(project_tree_capability(&project, Some(&restored)), ProjectTreeCapability::Unestablished);
        assert!(ProjectTreeCapability::Unestablished.destination(&project).is_none());
        assert_eq!(ProjectTreeCapability::Cargo.destination(&project), Some(Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project)))));
    }


}
