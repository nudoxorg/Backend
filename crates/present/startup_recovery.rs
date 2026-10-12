//! Exact shell replay of an observed startup selection; no activation proof.

/// Original workspace initialization boundary for a Unix startup retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupRecoveryWorkspace {
    /// Retain an explicit directory, including a workspace environment override.
    Explicit(String),
    /// Restore the original HOME anchor and use the platform's default layout.
    Home(String),
    /// Restore the original Linux XDG state anchor.
    XdgState(String),
    /// Restore the original Linux XDG data anchor and state suffix.
    XdgData(String),
}

/// Complete shell-renderable observation of one selected Unix attachment.
/// This value grants no owner activation, directory admission, or readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupRecoveryContext {
    project: String,
    endpoint: String,
    authority_secret: String,
    workspace: StartupRecoveryWorkspace,
}

impl StartupRecoveryContext {
    /// Retains all selected operands and the original workspace boundary.
    #[must_use]
    pub fn new(
        project: String,
        endpoint: String,
        authority_secret: String,
        workspace: StartupRecoveryWorkspace,
    ) -> Self {
        Self {
            project,
            endpoint,
            authority_secret,
            workspace,
        }
    }

    /// Renders a POSIX command without changing the invoking shell's environment.
    /// Default roots clear conflicting selections in a subshell and restore
    /// only the captured anchor. Every operand uses the shared shell quoting.
    #[must_use]
    pub fn shell(&self) -> String {
        let authority = format!(
            "BACKEND_LOCALD_AUTHORITY_SECRET_FILE={}",
            crate::shell::quote_argument(&self.authority_secret)
        );
        let mut command = format!(
            "{authority} nudox --project {}",
            crate::shell::quote_argument(&self.project)
        );
        if let StartupRecoveryWorkspace::Explicit(directory) = &self.workspace {
            command.push_str(" --workspace ");
            command.push_str(&crate::shell::quote_argument(directory));
        }
        command.push_str(" --endpoint ");
        command.push_str(&crate::shell::quote_argument(&self.endpoint));
        command.push_str(" health");
        let anchor = match &self.workspace {
            StartupRecoveryWorkspace::Explicit(_) => return command,
            StartupRecoveryWorkspace::Home(anchor) => ("HOME", anchor),
            StartupRecoveryWorkspace::XdgState(anchor) => ("XDG_STATE_HOME", anchor),
            StartupRecoveryWorkspace::XdgData(anchor) => ("XDG_DATA_HOME", anchor),
        };
        format!(
            "(unset BACKEND_LOCALD_WORKSPACE XDG_STATE_HOME XDG_DATA_HOME; {}={} {command})",
            anchor.0,
            crate::shell::quote_argument(anchor.1)
        )
    }
}
