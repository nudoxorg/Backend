//! Selection of the installed native SQL workers.
//!
//! The SQLite and PostgreSQL parsers run out of process. The owner admits a
//! worker only when the release build embedded the digest of that exact worker
//! (`NUDOX_BUILD_SQLITE_WORKER_ID` / `NUDOX_BUILD_POSTGRES_WORKER_ID`) and the
//! sibling file beside this executable still has that identity. A development
//! checkout carries no such binding, so selection finds nothing and every SQL
//! host is then recorded as unavailable (`EngineNotAdmitted`), never as empty.
//! Environment variables and project paths cannot supply the anchor. Tests
//! supply admitted workers through the adapter instead.
//!
//! Linux release images that load the worker through a private libgcc lease are
//! admitted by the packaging path; this module covers workers without runtime
//! library dependencies.
use super::{
    sql_capture::{PostgresWorkerPermit, SqliteWorkerPermit},
    sql_index::SqlWorkers,
};
use backend_compile::ToolchainArtifact;
use backend_sql_contract::{Dialect, Engine, Request, Settings};
use std::{path::Path, sync::Arc};

const fn valid_compiled_anchor(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return false;
    }
    let mut i = 0;
    let mut nonzero = false;
    while i < bytes.len() {
        let b = bytes[i];
        if !((b >= b'0' && b <= b'9') || (b >= b'a' && b <= b'f')) {
            return false;
        }
        nonzero |= b != b'0';
        i += 1;
    }
    nonzero
}

const _: () = {
    if let Some(value) = option_env!("NUDOX_BUILD_SQLITE_WORKER_ID") {
        assert!(valid_compiled_anchor(value), "invalid compiled SQLite worker identity");
    }
    if let Some(value) = option_env!("NUDOX_BUILD_POSTGRES_WORKER_ID") {
        assert!(valid_compiled_anchor(value), "invalid compiled PostgreSQL worker identity");
    }
};

fn decode_anchor(value: &str) -> Option<[u8; 32]> {
    if !valid_compiled_anchor(value) {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *slot = digit(pair[0]) * 16 + digit(pair[1]);
    }
    (out != [0; 32]).then_some(out)
}

/// Workers shipped beside this executable and bound to it at build time.
pub(super) fn selected_installed_sql_workers() -> SqlWorkers {
    let Ok(owner) = std::env::current_exe() else {
        return SqlWorkers::default();
    };
    SqlWorkers {
        sqlite: option_env!("NUDOX_BUILD_SQLITE_WORKER_ID")
            .and_then(decode_anchor)
            .and_then(|anchor| admit_sibling(&owner, "backend-sqlite-worker", anchor, Dialect::Sqlite, Engine::Syntaqlite4e4de76))
            .and_then(|(path, artifact, basis)| SqliteWorkerPermit::from_admitted(path, artifact, basis).ok())
            .map(Arc::new),
        postgres: option_env!("NUDOX_BUILD_POSTGRES_WORKER_ID")
            .and_then(decode_anchor)
            .and_then(|anchor| admit_sibling(&owner, "backend-postgres-worker", anchor, Dialect::Postgres, Engine::Pgrust38634b4))
            .and_then(|(path, artifact, basis)| PostgresWorkerPermit::from_admitted(path, artifact, basis).ok())
            .map(Arc::new),
    }
}

fn admit_sibling(
    owner: &Path,
    name: &str,
    expected: [u8; 32],
    dialect: Dialect,
    engine: Engine,
) -> Option<(std::path::PathBuf, ToolchainArtifact, backend_sql_contract::ProducerBasis)> {
    let owner = owner.canonicalize().ok()?;
    let candidate = owner.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let metadata = std::fs::symlink_metadata(&candidate).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > backend_compile::MAX_EXECUTABLE_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    let artifact = ToolchainArtifact::from_path(&candidate, Vec::new()).ok()?;
    if artifact.identity().to_bytes() != expected {
        return None;
    }
    // The worker's own report names the producer basis it was built with; the
    // anchor above is what makes that report trustworthy.
    let response = backend_sql_contract::call_worker(
        &candidate,
        &Request {
            version: backend_sql_contract::PROTOCOL_VERSION,
            expected_basis: None,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        },
        dialect,
        engine,
    );
    Some((candidate, artifact, response.basis?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_compiled_anchor_means_no_worker_and_bad_anchors_are_rejected() {
        assert!(decode_anchor("").is_none());
        assert!(decode_anchor(&"00".repeat(32)).is_none());
        assert!(decode_anchor(&"zz".repeat(32)).is_none());
        assert_eq!(decode_anchor(&"ab".repeat(32)), Some([0xab; 32]));
        // A development build embeds nothing, so nothing is admitted.
        if option_env!("NUDOX_BUILD_SQLITE_WORKER_ID").is_none() {
            assert!(selected_installed_sql_workers().sqlite.is_none());
        }
    }

    #[test]
    fn a_sibling_with_a_different_identity_is_not_admitted() {
        let directory = tempfile::tempdir().expect("private test directory");
        let owner = directory.path().join("backend-locald");
        std::fs::write(&owner, b"owner").expect("owner fixture");
        assert!(admit_sibling(&owner, "backend-sqlite-worker", [1; 32], Dialect::Sqlite, Engine::Syntaqlite4e4de76).is_none());
        let worker = directory.path().join("backend-sqlite-worker");
        std::fs::write(&worker, b"not a worker").expect("sibling fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).expect("executable");
        }
        assert!(admit_sibling(&owner, "backend-sqlite-worker", [1; 32], Dialect::Sqlite, Engine::Syntaqlite4e4de76).is_none());
    }
}
