//! Cursor signing authority is acquired only when a request uses a cursor.
//!
//! Protocol metadata has no continuation state. A workspace authority remains
//! durable and shared across processes, but constructing a server must not
//! create it or wait for workspace filesystem synchronization.

use super::RpcError;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone)]
pub(crate) struct CursorAuthority(Arc<Authority>);

enum Authority {
    Admitted([u8; 32]),
    Workspace {
        paths: backend_runtime::WorkspacePaths,
        key: OnceLock<[u8; 32]>,
        admission: Mutex<()>,
    },
}

impl CursorAuthority {
    /// Retains the selected workspace without reading or creating its state.
    pub(crate) fn workspace(paths: &backend_runtime::WorkspacePaths) -> Self {
        Self(Arc::new(Authority::Workspace {
            paths: paths.clone(),
            key: OnceLock::new(),
            admission: Mutex::new(()),
        }))
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
        Self(Arc::new(Authority::Admitted(key)))
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
