//! Exact shell replay of an observed connection; no activation proof.

/// Original workspace initialization boundary for a Unix connection replay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionReplayWorkspace {
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
pub struct ConnectionReplayContext {
    project: String,
    endpoint: String,
    authority_secret: String,
    workspace: ConnectionReplayWorkspace,
}

impl ConnectionReplayContext {
    /// Retains all selected operands and the original workspace boundary.
    #[must_use]
    pub fn new(
        project: String,
        endpoint: String,
        authority_secret: String,
        workspace: ConnectionReplayWorkspace,
    ) -> Self {
        Self {
            project,
            endpoint,
            authority_secret,
            workspace,
        }
    }

    /// Exact selected project used by a deliberate Add action.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Renders a POSIX command without changing the invoking shell's environment.
    /// Default roots clear conflicting selections in a subshell and restore
    /// only the captured anchor. Every operand uses the shared shell quoting.
    #[must_use]
    pub fn command_shell(&self, replay: &ConnectionReplayCommand) -> Option<String> {
        if replay
            .arguments
            .iter()
            .any(|argument| argument.contains('\0'))
        {
            return None;
        }
        let authority = format!(
            "BACKEND_LOCALD_AUTHORITY_SECRET_FILE={}",
            crate::shell::quote_argument(&self.authority_secret)
        );
        let mut command = format!(
            "{authority} nudox --project {}",
            crate::shell::quote_argument(&self.project)
        );
        if let ConnectionReplayWorkspace::Explicit(directory) = &self.workspace {
            command.push_str(" --workspace ");
            command.push_str(&crate::shell::quote_argument(directory));
        }
        command.push_str(" --endpoint ");
        command.push_str(&crate::shell::quote_argument(&self.endpoint));
        if replay.passive {
            command.push_str(" --passive");
        }
        command.push(' ');
        command.push_str(backend_library::command_spec(replay.command).name);
        for argument in &replay.arguments {
            command.push(' ');
            command.push_str(&crate::shell::quote_argument(argument));
        }
        let anchor = match &self.workspace {
            ConnectionReplayWorkspace::Explicit(_) => return Some(command),
            ConnectionReplayWorkspace::Home(anchor) => ("HOME", anchor),
            ConnectionReplayWorkspace::XdgState(anchor) => ("XDG_STATE_HOME", anchor),
            ConnectionReplayWorkspace::XdgData(anchor) => ("XDG_DATA_HOME", anchor),
        };
        Some(format!(
            "(unset BACKEND_LOCALD_WORKSPACE XDG_STATE_HOME XDG_DATA_HOME; {}={} {command})",
            anchor.0,
            crate::shell::quote_argument(anchor.1)
        ))
    }
}

/// A registered caller action on the captured connection. Rendering is not
/// execution permission or proof of admission, activation, or publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionReplayCommand {
    command: backend_library::CommandId,
    passive: bool,
    arguments: Vec<String>,
}

impl ConnectionReplayCommand {
    /// Retains the caller's registered command, observation mode and exact argv.
    #[must_use]
    pub fn new(command: backend_library::CommandId, passive: bool, arguments: Vec<String>) -> Self {
        Self {
            command,
            passive,
            arguments,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn replay_arguments_survive_posix_parsing_and_nul_has_no_copyable_action() {
        let context = ConnectionReplayContext::new(
            "/project".into(),
            "/socket".into(),
            "/authority".into(),
            ConnectionReplayWorkspace::Explicit("/state".into()),
        );
        let arguments = vec![
            String::new(),
            "' $(sentinel) `sentinel`; literal".into(),
            "line\nsecond".into(),
        ];
        let rendered = context
            .command_shell(&ConnectionReplayCommand::new(
                backend_library::CommandId::Add,
                false,
                arguments.clone(),
            ))
            .expect("exact UTF-8 argv");
        let output = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                &format!("nudox() {{ printf '%s\\000' \"$@\"; }}; {rendered}"),
            ])
            .output()
            .expect("POSIX argv capture only");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let words = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
        assert_eq!(
            &words[7..10],
            arguments
                .iter()
                .map(|arg| arg.as_bytes())
                .collect::<Vec<_>>()
        );
        assert!(
            context
                .command_shell(&ConnectionReplayCommand::new(
                    backend_library::CommandId::Add,
                    false,
                    vec!["embedded\0nul".into()]
                ))
                .is_none()
        );
    }
}
