//! One strict display projection of the connection frozen by discovery.

pub(crate) fn context(
    paths: &backend_runtime::WorkspacePaths,
) -> Option<backend_present::ConnectionReplayContext> {
    #[cfg(unix)]
    {
        // Relative operands lack a captured CWD; never guess or decode lossily.
        fn operand(path: &std::path::Path) -> Option<String> {
            let text = path.to_str()?;
            (path.is_absolute() && !text.contains('\0')).then(|| text.to_owned())
        }
        let selected = paths.connection_replay_selection()?;
        operand(selected.data)?;
        let workspace = match selected.workspace {
            backend_runtime::ConnectionWorkspaceSelection::Explicit(path) => {
                backend_present::ConnectionReplayWorkspace::Explicit(operand(path)?)
            }
            backend_runtime::ConnectionWorkspaceSelection::Home(anchor) => {
                backend_present::ConnectionReplayWorkspace::Home(operand(anchor)?)
            }
            backend_runtime::ConnectionWorkspaceSelection::XdgState(anchor) => {
                backend_present::ConnectionReplayWorkspace::XdgState(operand(anchor)?)
            }
            backend_runtime::ConnectionWorkspaceSelection::XdgData(anchor) => {
                backend_present::ConnectionReplayWorkspace::XdgData(operand(anchor)?)
            }
        };
        Some(backend_present::ConnectionReplayContext::new(
            operand(selected.project)?,
            operand(selected.endpoint)?,
            operand(selected.authority_secret)?,
            workspace,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        None
    }
}
