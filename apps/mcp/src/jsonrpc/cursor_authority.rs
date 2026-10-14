//! Cursor signing authority is acquired only when a request uses a cursor.
//!
//! Protocol metadata has no continuation state. A workspace authority remains
//! durable and shared across processes, but constructing a server must not
//! create it or wait for workspace filesystem synchronization.

use super::RpcError;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

/// Owner cursors longer than this many bytes are issued to the agent as a
/// short handle. A portable query proof is two kilobytes of compressed
/// base64: copied into the agent's context on every page it would cost more
/// than the page's rows.
pub(super) const SHORT_CURSOR_THRESHOLD: usize = 120;

/// Handles remembered at once; the oldest is forgotten first.
const MAX_HANDLES: usize = 256;

/// How long a handle is honoured: exactly as long as its signed wrapper.
const HANDLE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(900);

#[derive(Clone)]
pub(crate) struct CursorAuthority(Arc<Authority>, Arc<Mutex<CursorHandles>>);

/// Handles issued by this process, newest last.
///
/// A handle is content-addressed: its id is the head of the hash of the owner
/// cursor it stands for, so the cursor behind a handle can be checked against
/// the handle itself, and the same cursor always has the same handle. The
/// table is not an authority. A handle is looked up only after its wrapper's
/// MAC verified against the request context, and the owner still admits the
/// cursor it resolves to.
///
/// A workspace authority also keeps each handle as a small private file beside
/// the cursor secret, so a handle outlives a restart of this process exactly as
/// the signed long form always has. A file that cannot be written only costs
/// that survival; the handle still resolves in this process.
#[derive(Default)]
struct CursorHandles {
    entries: VecDeque<(String, String)>,
}

enum Authority {
    Admitted([u8; 32]),
    Workspace {
        paths: backend_runtime::WorkspacePaths,
        key: OnceLock<[u8; 32]>,
        admission: Mutex<()>,
    },
}

fn handle_id(owner_token: &str) -> String {
    blake3::hash(owner_token.as_bytes()).as_bytes()[..6]
        .iter()
        .fold(String::with_capacity(12), |mut id, byte| {
            use std::fmt::Write as _;
            let _ = write!(id, "{byte:02x}");
            id
        })
}

impl CursorAuthority {
    /// Retains the selected workspace without reading or creating its state.
    pub(crate) fn workspace(paths: &backend_runtime::WorkspacePaths) -> Self {
        Self(
            Arc::new(Authority::Workspace {
                paths: paths.clone(),
                key: OnceLock::new(),
                admission: Mutex::new(()),
            }),
            Arc::default(),
        )
    }

    /// Remembers an owner cursor and returns the short handle that names it.
    pub(super) fn issue_handle(&self, owner_token: &str) -> Result<String, RpcError> {
        let id = handle_id(owner_token);
        {
            let mut table = self
                .1
                .lock()
                .map_err(|_| RpcError::tool("cursor handle table unavailable"))?;
            if !table.entries.iter().any(|(known, _)| *known == id) {
                if table.entries.len() >= MAX_HANDLES {
                    table.entries.pop_front();
                }
                table.entries.push_back((id.clone(), owner_token.to_owned()));
            }
        }
        if let Some(directory) = self.handle_directory() {
            // Best effort: only restart survival depends on it.
            let _ = persist_handle(&directory, &id, owner_token);
        }
        Ok(id)
    }

    /// Returns the owner cursor a handle stands for, when this process or its
    /// workspace still remembers it. Resolution never creates state.
    pub(super) fn resolve_handle(&self, id: &str) -> Result<Option<String>, RpcError> {
        {
            let table = self
                .1
                .lock()
                .map_err(|_| RpcError::tool("cursor handle table unavailable"))?;
            if let Some((_, owner)) = table.entries.iter().find(|(known, _)| known == id) {
                return Ok(Some(owner.clone()));
            }
        }
        Ok(self
            .handle_directory()
            .and_then(|directory| read_handle(&directory, id)))
    }

    fn handle_directory(&self) -> Option<std::path::PathBuf> {
        match self.0.as_ref() {
            Authority::Admitted(_) => None,
            Authority::Workspace { paths, .. } => Some(
                paths
                    .authority_secret()
                    .parent()?
                    .join("mcp-cursor-handles"),
            ),
        }
    }

    pub(super) fn key(&self) -> Result<[u8; 32], RpcError> {
        match self.0.as_ref() {
            Authority::Admitted(key) => Ok(*key),
            Authority::Workspace {
                paths,
                key,
                admission,
            } => {
                if let Some(key) = key.get() {
                    return Ok(*key);
                }
                let _admission = admission
                    .lock()
                    .map_err(|_| RpcError::tool("cursor authority admission unavailable"))?;
                if let Some(key) = key.get() {
                    return Ok(*key);
                }
                // A refusal admits no key and may be retried after workspace
                // recovery. Once admitted, every session keeps the same key.
                let admitted = admit_workspace_key(paths).map_err(RpcError::tool)?;
                Ok(*key.get_or_init(|| admitted))
            }
        }
    }

    /// Verification may read an existing authority, but untrusted input must
    /// never create one. A missing key remains retryable after owner recovery
    /// or the first successful signing request.
    pub(super) fn verification_key(&self) -> Result<Option<[u8; 32]>, RpcError> {
        match self.0.as_ref() {
            Authority::Admitted(key) => Ok(Some(*key)),
            Authority::Workspace {
                paths,
                key,
                admission,
            } => {
                if let Some(key) = key.get() {
                    return Ok(Some(*key));
                }
                let _admission = admission
                    .lock()
                    .map_err(|_| RpcError::tool("cursor authority admission unavailable"))?;
                if let Some(key) = key.get() {
                    return Ok(Some(*key));
                }
                match backend_engine::read_authority_secret(paths.authority_secret()) {
                    Ok(admitted) => Ok(Some(*key.get_or_init(|| admitted))),
                    Err(backend_engine::AuthoritySecretError::Io(std::io::ErrorKind::NotFound)) => {
                        Ok(None)
                    }
                    Err(error) => Err(RpcError::tool(format!(
                        "cannot admit MCP authority secret {}: {error}",
                        paths.authority_secret().display()
                    ))),
                }
            }
        }
    }
}

impl From<[u8; 32]> for CursorAuthority {
    fn from(key: [u8; 32]) -> Self {
        Self(Arc::new(Authority::Admitted(key)), Arc::default())
    }
}

fn admit_workspace_key(paths: &backend_runtime::WorkspacePaths) -> Result<[u8; 32], String> {
    match backend_engine::read_authority_secret(paths.authority_secret()) {
        Ok(key) => Ok(key),
        Err(backend_engine::AuthoritySecretError::Io(std::io::ErrorKind::NotFound)) => {
            paths.initialize().map_err(|error| error.to_string())?;
            super::read_authority_secret(paths.authority_secret())
        }
        Err(error) => Err(format!(
            "cannot admit MCP authority secret {}: {error}",
            paths.authority_secret().display()
        )),
    }
}

fn persist_handle(
    directory: &std::path::Path,
    id: &str,
    owner_token: &str,
) -> std::io::Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let path = directory.join(id);
    if path.exists() {
        // Refresh the lifetime: the same cursor issued again is a new page.
        let file = std::fs::OpenOptions::new().write(true).open(&path)?;
        file.set_modified(std::time::SystemTime::now())?;
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(directory)?;
    let temporary = directory.join(format!("{id}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(owner_token.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    prune_handles(directory);
    Ok(())
}

/// Forgets expired handles, then the oldest beyond the bound.
fn prune_handles(directory: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() > HANDLE_LIFETIME {
            let _ = std::fs::remove_file(entry.path());
        } else {
            live.push((modified, entry.path()));
        }
    }
    live.sort();
    let excess = live.len().saturating_sub(MAX_HANDLES);
    for (_, path) in live.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

fn read_handle(directory: &std::path::Path, id: &str) -> Option<String> {
    use std::io::Read as _;
    if id.len() != 12 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let file = std::fs::File::open(directory.join(id)).ok()?;
    let modified = file.metadata().ok()?.modified().ok()?;
    if std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        > HANDLE_LIFETIME
    {
        return None;
    }
    let mut owner = String::new();
    file.take(backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES as u64 + 1)
        .read_to_string(&mut owner)
        .ok()?;
    // The handle is the head of the cursor's hash: a file whose content does
    // not hash to its name is not a handle this module wrote.
    (owner.len() <= backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES && handle_id(&owner) == id)
        .then_some(owner)
}
