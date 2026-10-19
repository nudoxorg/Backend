//! Owner-isolated opening of an explicitly selected existing default index.
//! Directory presence never proves publication. Project identity comes only from
//! the authenticated owner's Packages reply and the original default-path hash.

use crate::ClientError;
#[cfg(any(unix, windows))]
use crate::Session;
#[cfg(any(unix, windows))]
use backend_library::{CommandReply, RowId};
use backend_platform::{DirectoryCapability, FileIdentity};
use backend_runtime::{AUTHORITY_SECRET_ENV, DATA_ENV, ENDPOINT_ENV, PROJECT_ENV, WorkspacePaths};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn refused(reason: impl std::fmt::Display) -> ClientError {
    ClientError::Io(format!("the saved workspace could not be opened: {reason}"))
}

/// Whether the caller selected a surface-specific workspace configuration.
#[must_use]
pub fn explicit_configuration() -> bool {
    [PROJECT_ENV, DATA_ENV, ENDPOINT_ENV, AUTHORITY_SECRET_ENV]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some())
}

/// An existing private default store, with no claim of publication or a live owner.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SavedIndexWorkspace {
    project: PathBuf,
    projects: PathBuf,
    data: PathBuf,
    authority_identity: [u8; 32],
    directory_identity: FileIdentity,
    credential_identity: FileIdentity,
}

impl SavedIndexWorkspace {
    /// Existing default store directory; it is never moved, merged, or indexed here.
    #[must_use]
    pub fn data(&self) -> &Path {
        &self.data
    }

    fn verify(&self) -> Result<(), ClientError> {
        let parent = DirectoryCapability::open(&self.projects).map_err(refused)?;
        parent.validate_private().map_err(refused)?;
        let name = self
            .data
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| refused("invalid default workspace name"))?;
        if FileIdentity::of_path_nofollow(&self.data).map_err(refused)? != self.directory_identity {
            return Err(refused(
                "the saved workspace directory was replaced; refresh the list",
            ));
        }
        let root = parent.open_dir(name).map_err(refused)?;
        root.validate_private().map_err(refused)?;
        if authority_identity(&root)? != (self.authority_identity, self.credential_identity) {
            return Err(refused(
                "the existing authority changed; refresh the saved workspaces",
            ));
        }
        root.verify_path(&self.data).map_err(refused)?;
        parent.verify_path(&self.projects).map_err(refused)
    }

    /// Opens this exact owner and identifies its project from an authenticated read.
    /// A cold owner may start through the normal bounded runtime startup path.
    /// No index/add/remove command is sent. An ambiguous or absent hash match
    /// refuses rather than selecting an arbitrary package or working directory.
    ///
    /// # Errors
    /// Returns ordinary startup/transport errors or a changed/ambiguous binding.
    #[cfg(any(unix, windows))]
    pub fn open(&self) -> Result<WorkspacePaths, ClientError> {
        if explicit_configuration() {
            return Err(refused("explicit workspace configuration is active"));
        }
        self.verify()?;
        // The builtin owner receives no project operand: it recovers the exact
        // workspace selected by data/endpoint/authority. Use the existing private
        // data directory only as the startup cwd, never as an indexed project.
        let endpoint = backend_runtime::derive_endpoint(&self.data);
        let bootstrap = WorkspacePaths::discover(
            Some(self.data.clone()),
            Some(self.data.clone()),
            Some(endpoint),
        )
        .map_err(refused)?;
        let endpoint = backend_runtime::ensure_locald(&bootstrap).map_err(refused)?;
        let mut session = Session::connect_with_timeouts(
            &endpoint,
            Duration::from_secs(5),
            Duration::from_secs(30),
        )?;
        let reply = session.packages()?;
        let CommandReply::Packages(view) = reply.reply else {
            return Err(refused(
                "the owner did not return its indexed package shelf",
            ));
        };
        let candidates = view
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Package(_) => Some(PathBuf::from(row.label.as_str())),
                RowId::Symbol(_) | RowId::Object(_) => None,
            })
            .collect::<Vec<_>>();
        self.identify(&candidates)
    }

    /// Rechecks a resolved window binding immediately before its process starts.
    ///
    /// # Errors
    /// Refuses explicit overrides, changed credentials/store identity, or a
    /// caller-supplied project, endpoint, or authority outside this candidate.
    pub fn revalidate(&self, paths: &WorkspacePaths) -> Result<(), ClientError> {
        if explicit_configuration() {
            return Err(refused("explicit workspace configuration is active"));
        }
        if paths.project() != self.project
            || paths.data() != self.data
            || paths.endpoint() != backend_runtime::derive_endpoint(&self.data)
            || paths.authority_secret() != self.data.join("authority.secret")
            || !paths.project().is_absolute()
            || backend_runtime::normalize_surface_path(
                backend_runtime::default_project_workspace_in(paths.project(), &self.projects),
            ) != self.data
        {
            return Err(refused(
                "the resolved window does not match this saved workspace",
            ));
        }
        self.verify()
    }

    fn identify(&self, candidates: &[PathBuf]) -> Result<WorkspacePaths, ClientError> {
        let mut matching = candidates
            .iter()
            .filter(|project| {
                **project == self.project
                    && project.is_absolute()
                    && backend_runtime::normalize_surface_path(project) == **project
                    && backend_runtime::normalize_surface_path(
                        backend_runtime::default_project_workspace_in(project, &self.projects),
                    ) == self.data
            })
            .cloned()
            .collect::<Vec<_>>();
        matching.sort();
        matching.dedup();
        let [project] = matching.as_slice() else {
            return Err(refused(
                "this saved index has no unique original project identity; open its original folder explicitly",
            ));
        };
        self.verify()?;
        WorkspacePaths::discover(
            Some(project.clone()),
            Some(self.data.clone()),
            Some(backend_runtime::derive_endpoint(&self.data)),
        )
        .map_err(refused)
    }
}

fn authority_identity(root: &DirectoryCapability) -> Result<([u8; 32], FileIdentity), ClientError> {
    let mut file = root
        .open_private_file("authority.secret")
        .map_err(refused)?;
    if file.metadata().map_err(refused)?.len() != 32 {
        return Err(refused("existing authority is not exactly 32 bytes"));
    }
    let mut secret = [0; 32];
    file.read_exact(&mut secret).map_err(refused)?;
    Ok((
        blake3::derive_key("Nudox saved workspace authority v1", &secret),
        FileIdentity::of_file(&file).map_err(refused)?,
    ))
}

/// Selects an already-existing default index for one explicitly chosen folder.
/// This is read-only: missing stores and unstarted credential-only directories
/// refuse without initialization, owner startup, or an index command.
///
/// # Errors
/// Refuses explicit workspace overrides, an invalid folder, or an absent,
/// aliased, replaced or non-private existing default store/credential.
pub fn find_existing_indexed_project(project: &Path) -> Result<SavedIndexWorkspace, ClientError> {
    if explicit_configuration() {
        return Err(refused("explicit workspace configuration is active"));
    }
    let projects = backend_runtime::default_project_workspaces_directory().map_err(refused)?;
    find_in(project, &projects)
}

fn find_in(project: &Path, projects: &Path) -> Result<SavedIndexWorkspace, ClientError> {
    if !project.is_absolute() {
        return Err(refused("choose an absolute project folder"));
    }
    let project =
        backend_runtime::normalize_surface_path(std::fs::canonicalize(project).map_err(refused)?);
    if !project.is_dir() {
        return Err(refused("the selected project is not a directory"));
    }
    // Keep the raw platform parent for no-follow validation. Only normalize
    // the admitted child's surface spelling after both capabilities are held.
    let parent = DirectoryCapability::open(projects).map_err(|error| {
        refused(format!("no existing default index could be opened for this folder: {error}; nothing was created or indexed"))
    })?;
    parent.validate_private().map_err(refused)?;
    let selected = backend_runtime::default_project_workspace_in(&project, projects);
    let name = selected
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refused("invalid default workspace name"))?;
    let root = parent.open_dir(name).map_err(|error| {
        refused(format!("no existing default index could be opened for this folder: {error}; nothing was created or indexed"))
    })?;
    root.validate_private().map_err(refused)?;
    root.open_private_file("OWNER.state").map_err(|error| {
        refused(format!("this folder has no previously initialized owner: {error}; nothing was created or indexed"))
    })?;
    let (authority_identity, credential_identity) = authority_identity(&root)?;
    let data = backend_runtime::normalize_surface_path(&selected);
    let directory_identity = FileIdentity::of_path_nofollow(&data).map_err(refused)?;
    root.verify_path(&selected).map_err(refused)?;
    root.verify_path(&data).map_err(refused)?;
    parent.verify_path(projects).map_err(refused)?;
    let candidate = SavedIndexWorkspace {
        project,
        projects: projects.to_path_buf(),
        data,
        authority_identity,
        directory_identity,
        credential_identity,
    };
    candidate.verify()?;
    Ok(candidate)
}

#[cfg(test)]
mod tests;
