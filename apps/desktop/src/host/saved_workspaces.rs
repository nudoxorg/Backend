//! Opens an explicitly chosen existing index in a separate immutable binding.
//! The native picker does not enter the Add/index mutation route.
use crate::runtime::offload::{Answer, Asker, Memo};
use backend_runtime::{AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV, WorkspacePaths};
use gpui::{App, Context, Global, PathPromptOptions};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

#[derive(Clone, Eq, Hash, PartialEq)]
struct OpenKey {
    project: PathBuf,
    attempt: u64,
}
struct Saved {
    opening: Memo<OpenKey, Result<u32, String>>,
    selected: Option<OpenKey>,
    next: u64,
    picking: bool,
    picker_error: Option<String>,
}
impl Global for Saved {}
impl Default for Saved {
    fn default() -> Self {
        Self {
            opening: Memo::new(NonZeroUsize::MIN, |key: &OpenKey| open_window(&key.project)),
            selected: None,
            next: 0,
            picking: false,
            picker_error: None,
        }
    }
}

pub(crate) fn explicitly_configured() -> bool {
    [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some())
}

pub(crate) fn opening<T: 'static>(cx: &mut Context<T>) -> Option<Answer<Result<u32, String>>> {
    let state = cx.default_global::<Saved>();
    let (key, memo) = (state.selected.clone()?, state.opening.clone());
    Some(memo.get(&key, cx))
}

pub(crate) fn picker_status(cx: &mut App) -> Option<String> {
    let state = cx.default_global::<Saved>();
    if state.picking {
        Some("Choose the project folder whose existing index you want to open.".into())
    } else {
        state.picker_error.clone()
    }
}

pub(crate) fn choose(cx: &mut App) {
    if explicitly_configured() {
        return;
    }
    let state = cx.default_global::<Saved>();
    if state.picking
        || state
            .selected
            .as_ref()
            .is_some_and(|key| state.opening.is_reading(key))
    {
        return;
    }
    state.picking = true;
    state.picker_error = None;
    state.selected = None;
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open indexed project".into()),
    });
    cx.spawn(async move |cx| {
        let chosen = match receiver.await {
            Ok(Ok(Some(mut paths))) if paths.len() == 1 => Ok(paths.pop()),
            Ok(Ok(None)) => Ok(None),
            Ok(Ok(Some(_))) => Err("Choose one project folder.".to_owned()),
            Ok(Err(error)) => Err(format!("The project folder picker could not open: {error}")),
            Err(_) => Err("The project folder picker closed without a result.".to_owned()),
        };
        let _ = cx.update(|cx| {
            cx.default_global::<Saved>().picking = false;
            match chosen {
                Ok(Some(project)) => start_open(project, cx),
                Ok(None) => {}
                Err(error) => {
                    cx.default_global::<Saved>().picker_error =
                        Some(error.chars().take(480).collect())
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
    cx.refresh_windows();
}

fn start_open(project: PathBuf, cx: &mut App) {
    if explicitly_configured() {
        return;
    }
    let state = cx.default_global::<Saved>();
    let key = OpenKey {
        project,
        attempt: state.next,
    };
    state.next = state.next.saturating_add(1);
    state.selected = Some(key.clone());
    let memo = state.opening.clone();
    let _ = memo.ask(&key, Asker::Everyone, cx);
}

fn command(executable: &Path, paths: &WorkspacePaths) -> Command {
    let mut child = Command::new(executable);
    child
        .env(PROJECT_ENV, paths.project())
        .env(DATA_ENV, paths.data())
        .env(ENDPOINT_ENV, paths.endpoint())
        .env(AUTHORITY_SECRET_ENV, paths.authority_secret())
        .current_dir(paths.data())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    child
}

fn open_window(project: &std::path::Path) -> Result<u32, String> {
    // Resolves only the path authenticated by this store and matching its
    // default-directory hash. This is a read, not an index request or migration.
    let workspace = backend_client::find_existing_indexed_project(project)
        .map_err(|error| error.to_string())?;
    let paths = workspace.open().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| error.to_string())?;
    let mut child_command = command(&executable, &paths);
    // The monitor owns Child before reporting success and reaps only that child.
    // No signal, restart or state wipe is used to make another owner attach.
    let (sent, received) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("nudox-saved-window".to_owned())
        .spawn(move || {
            if let Err(error) = workspace.revalidate(&paths) {
                let _ = sent.send(Err(error.to_string()));
                return;
            }
            match child_command.spawn() {
                Ok(mut child) => {
                    let _ = sent.send(Ok(child.id()));
                    let _ = child.wait();
                }
                Err(error) => {
                    let _ = sent.send(Err(error.to_string()));
                }
            }
        })
        .map_err(|error| error.to_string())?;
    received.recv().map_err(|error| error.to_string())?
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn saved_window_uses_its_exact_binding_without_reindex_or_parent_environment_changes() {
        let root = std::env::temp_dir().join("nudox-saved-command-only");
        let project = root.join("original-project");
        let data = root.join("original-data");
        let endpoint = backend_runtime::derive_endpoint(&data);
        let paths = WorkspacePaths::discover(
            Some(project.clone()),
            Some(data.clone()),
            Some(endpoint.clone()),
        )
        .expect("paths only");
        let before =
            [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV].map(std::env::var_os);
        let child = command(Path::new("/not-executed/backend-desktop"), &paths);
        let env = child
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(std::ffi::OsStr::to_owned)))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            env.get(std::ffi::OsStr::new(PROJECT_ENV)),
            Some(&Some(paths.project().as_os_str().to_owned()))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new(DATA_ENV)),
            Some(&Some(paths.data().as_os_str().to_owned()))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new(ENDPOINT_ENV)),
            Some(&Some(paths.endpoint().as_os_str().to_owned()))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new(AUTHORITY_SECRET_ENV)),
            Some(&Some(paths.authority_secret().as_os_str().to_owned()))
        );
        assert_eq!(
            child.get_args().count(),
            0,
            "opening a saved index sends no add/reindex command"
        );
        assert_eq!(
            child.get_current_dir(),
            Some(paths.data()),
            "an absent source folder does not prevent reading its retained index"
        );
        assert_eq!(
            [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV].map(std::env::var_os),
            before
        );
    }
}
