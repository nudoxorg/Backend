//! Exact Cargo browse addresses. A saved binding is never a current read.

use crate::core::LocalProjectId;
use backend_library::browse::ProjectTreeRequestBindingV1;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

/// The exact submitted directory and both owner-returned root commitments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoBrowseContext {
    requested_project: LocalProjectId,
    request_binding: ProjectTreeRequestBindingV1,
}

impl CargoBrowseContext {
    /// Admits an address, including a saved address, without admitting reads.
    pub(crate) fn from_binding_address(
        requested_project: LocalProjectId,
        request_binding: ProjectTreeRequestBindingV1,
    ) -> Option<Self> {
        let coordinate = requested_project.service_coordinate().ok()?;
        let operand = backend_library::ProductText::new(coordinate).ok()?;
        if operand.as_str() != coordinate { return None; }
        let path = std::path::Path::new(coordinate);
        (request_binding.has_admissible_shape() && request_binding.matches_requested_root(path))
            .then_some(Self { requested_project, request_binding })
    }

    /// The exact address sent to ProjectTree, even for a member directory.
    #[must_use]
    pub fn requested_project(&self) -> &LocalProjectId { &self.requested_project }

    /// The full owner binding, never reconstructed from a displayed root.
    #[must_use]
    pub const fn request_binding(&self) -> ProjectTreeRequestBindingV1 { self.request_binding }

    fn binding_identity(&self) -> (u16, [u8; 32], [u8; 32]) {
        (self.request_binding.schema, self.request_binding.requested_root_digest,
            self.request_binding.effective_workspace_root_digest)
    }
}

impl Hash for CargoBrowseContext {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.requested_project.hash(state);
        self.binding_identity().hash(state);
    }
}
impl PartialOrd for CargoBrowseContext {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}
impl Ord for CargoBrowseContext {
    fn cmp(&self, other: &Self) -> Ordering {
        self.requested_project.cmp(&other.requested_project)
            .then_with(|| self.binding_identity().cmp(&other.binding_identity()))
    }
}

/// Legacy addresses must observe their exact Tree before any source read.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CargoBrowseAddress {
    /// Old persisted addresses have no owner binding and request only Tree.
    AwaitingTree {
        /// Exact submitted directory retained by the saved source address.
        requested_project: LocalProjectId,
    },
    /// A complete address still requires a fresh current resource for actions.
    Bound(CargoBrowseContext),
}

impl CargoBrowseAddress {
    #[must_use]
    pub fn requested_project(&self) -> &LocalProjectId {
        match self { Self::AwaitingTree { requested_project } => requested_project,
            Self::Bound(context) => context.requested_project() }
    }

    #[must_use]
    pub fn context(&self) -> Option<&CargoBrowseContext> {
        match self { Self::Bound(context) => Some(context), Self::AwaitingTree { .. } => None }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn fixture_context(project: LocalProjectId) -> CargoBrowseContext {
    let coordinate = project.service_coordinate().expect("fixture project");
    let binding = ProjectTreeRequestBindingV1::for_paths(std::path::Path::new(coordinate), coordinate)
        .expect("synthetic fixture binding, not live-owner acceptance");
    CargoBrowseContext::from_binding_address(project, binding).expect("fixture context")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn requested_member_and_effective_workspace_remain_distinct_address_identity() {
        let member = LocalProjectId::new("/fixture/workspace/member").expect("member");
        let binding = ProjectTreeRequestBindingV1::for_paths(
            std::path::Path::new("/fixture/workspace/member"), "/fixture/workspace",
        ).expect("owner fixture binding");
        let context = CargoBrowseContext::from_binding_address(member.clone(), binding).expect("member context");
        assert_eq!(context.requested_project(), &member);
        assert_eq!(context.request_binding(), binding);
        assert!(CargoBrowseContext::from_binding_address(
            LocalProjectId::new("/fixture/workspace").expect("workspace"), binding,
        ).is_none(), "an effective root cannot substitute for the submitted directory");
        let changed = ProjectTreeRequestBindingV1::for_paths(
            std::path::Path::new("/fixture/workspace/member"), "/fixture/replacement",
        ).expect("changed binding");
        assert_ne!(context, CargoBrowseContext::from_binding_address(member, changed).expect("other address"));
    }

    #[test]
    fn service_text_cannot_silently_trim_an_exact_native_project_address() {
        let requested = std::path::Path::new("/fixture/workspace/member folder ");
        let project = LocalProjectId::from_path(requested).expect("exact native spelling");
        assert_eq!(project.service_coordinate().expect("UTF-8"), "/fixture/workspace/member folder ");
        let binding = ProjectTreeRequestBindingV1::for_paths(requested, "/fixture/workspace").expect("address commitments");
        assert!(CargoBrowseContext::from_binding_address(project, binding).is_none(), "the current ProductText boundary would change this submitted directory");
        let requested = std::path::Path::new("/fixture/workspace/member folder");
        let project = LocalProjectId::from_path(requested).expect("interior space");
        let binding = ProjectTreeRequestBindingV1::for_paths(requested, "/fixture/workspace").expect("binding");
        assert_eq!(CargoBrowseContext::from_binding_address(project, binding).expect("exact service operand").requested_project().service_coordinate().expect("UTF-8"), "/fixture/workspace/member folder");
    }
}
