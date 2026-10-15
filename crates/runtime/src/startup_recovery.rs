//! Read-only recovery projection of the attachment frozen by discovery.

use super::WorkspacePaths;
use std::path::Path;

/// Original durable-directory boundary; this is not directory admission.
#[derive(Clone, Copy, Debug)]
pub enum StartupWorkspaceSelection<'a> {
    /// Explicit path or workspace environment override.
    Explicit(&'a Path),
    /// Platform HOME layout, retaining its original anchor.
    Home(&'a Path),
    /// Linux XDG state root with no suffix.
    XdgState(&'a Path),
    /// Linux XDG data root followed by the conventional state suffix.
    XdgData(&'a Path),
}

/// Complete selected path operands and the original initialization boundary.
/// Borrowing this observation cannot initialize state or grant owner authority.
#[derive(Clone, Copy, Debug)]
pub struct StartupRecoverySelection<'a> {
    /// Selected indexed project identity.
    pub project: &'a Path,
    /// Selected durable directory identity, also retained for default roots.
    pub data: &'a Path,
    /// Selected local endpoint spelling.
    pub endpoint: &'a Path,
    /// Selected authority-file operand, including an environment override.
    pub authority_secret: &'a Path,
    /// Whether the selected credential is derived from this workspace's default.
    /// A shell hint can reproduce this selection without disclosing a file path.
    pub workspace_authority: bool,
    /// Explicit versus platform-default directory selection.
    pub workspace: StartupWorkspaceSelection<'a>,
}

impl StartupRecoverySelection<'_> {
    /// Returns whether ordinary discovery can retain this credential selection
    /// without printing its private path in a recovery command.
    #[must_use]
    pub fn uses_default_authority(&self) -> bool {
        self.authority_secret == self.data.join(super::AUTHORITY_FILE)
    }
}

impl WorkspacePaths {
    /// Projects the already-captured Unix recovery selection without reading
    /// the environment again or changing the workspace's admission boundary.
    #[must_use]
    pub fn startup_recovery_selection(&self) -> Option<StartupRecoverySelection<'_>> {
        let workspace = match &self.private_application_root {
            None => StartupWorkspaceSelection::Explicit(&self.data),
            Some(root) => match root.suffix {
                #[cfg(target_os = "macos")]
                ["Library", "Application Support"] => StartupWorkspaceSelection::Home(&root.anchor),
                #[cfg(not(target_os = "macos"))]
                [".local", "state"] => StartupWorkspaceSelection::Home(&root.anchor),
                #[cfg(not(target_os = "macos"))]
                [] => StartupWorkspaceSelection::XdgState(&root.anchor),
                #[cfg(not(target_os = "macos"))]
                ["state"] => StartupWorkspaceSelection::XdgData(&root.anchor),
                _ => return None,
            },
        };
        Some(StartupRecoverySelection {
            project: &self.project,
            data: &self.data,
            endpoint: &self.endpoint,
            authority_secret: &self.authority_secret,
            workspace_authority: self.authority_secret == self.data.join(super::AUTHORITY_FILE),
            workspace,
        })
    }
}
