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
//! Linux bundles also bind the private libgcc dependency and native producer
//! basis. Each host launch leases and revalidates that exact dependency.
use super::{
    sql_capture::{PostgresWorkerPermit, SqliteWorkerPermit},
    sql_index::SqlWorkers,
};
use backend_compile::ToolchainArtifact;
use backend_sql_contract::{CheckedLaunch, Dialect, Engine};
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
        assert!(
            valid_compiled_anchor(value),
            "invalid compiled SQLite worker identity"
        );
    }
    if let Some(value) = option_env!("NUDOX_BUILD_POSTGRES_WORKER_ID") {
        assert!(
            valid_compiled_anchor(value),
            "invalid compiled PostgreSQL worker identity"
        );
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
            .and_then(|anchor| {
                admit_sqlite_sibling(&owner, anchor, option_env!("NUDOX_BUILD_SQLITE_BASIS_JSON"))
            })
            .map(Arc::new),
        postgres: option_env!("NUDOX_BUILD_POSTGRES_WORKER_ID")
            .and_then(decode_anchor)
            .and_then(|anchor| {
                admit_sibling(
                    &owner,
                    "backend-postgres-worker",
                    anchor,
                    Dialect::Postgres,
                    Engine::Pgrust38634b4,
                )
            })
            .and_then(|(path, artifact, basis)| {
                PostgresWorkerPermit::from_admitted(path, artifact, basis).ok()
            })
            .map(Arc::new),
    }
}

fn admit_sqlite_sibling(
    owner: &Path,
    expected: [u8; 32],
    basis_json: Option<&str>,
) -> Option<SqliteWorkerPermit> {
    #[cfg(target_os = "linux")]
    if let Some(basis_json) = basis_json {
        // This basis was observed from the independently built native observer,
        // then embedded with the final worker+library identity. Runtime paths
        // and a worker's unauthenticated self-report cannot supply it.
        if basis_json.len() > 8192 {
            return None;
        }
        let basis: backend_sql_contract::ProducerBasis = serde_json::from_str(basis_json).ok()?;
        let owner = owner.canonicalize().ok()?;
        let bin = owner.parent()?;
        if bin.file_name()?.to_str()? != "bin" {
            return None;
        }
        let candidate = bin.join("backend-sqlite-worker");
        let lib = bin.parent()?.join("lib");
        if !std::fs::symlink_metadata(&lib).ok()?.file_type().is_dir() {
            return None;
        }
        let library = lib.join("libgcc_s.so.1");
        for path in [&candidate, &library] {
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if !metadata.file_type().is_file() {
                return None;
            }
        }
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::symlink_metadata(&candidate)
            .ok()?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return None;
        }
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let deadline = std::time::Instant::now().checked_add(std::time::Duration::from_secs(30))?;
        let identity = backend_compile::ExecutableIdentity::from_path_until_with_limit(
            &library,
            &cancellation,
            deadline,
            16 * 1024 * 1024,
        )
        .ok()?;
        let executable = backend_compile::ExecutableIdentity::from_path_until_with_limit(
            &candidate,
            &cancellation,
            deadline,
            backend_compile::MAX_EXECUTABLE_BYTES,
        )
        .ok()?;
        let artifact = ToolchainArtifact::new(executable, vec![identity.digest()]);
        if artifact.identity().to_bytes() != expected {
            return None;
        }
        return SqliteWorkerPermit::from_admitted_with_linux_library(
            candidate, artifact, basis, library, identity,
        )
        .ok();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = basis_json;
    admit_sibling(
        owner,
        "backend-sqlite-worker",
        expected,
        Dialect::Sqlite,
        Engine::Syntaqlite4e4de76,
    )
    .and_then(|(path, artifact, basis)| {
        SqliteWorkerPermit::from_admitted(path, artifact, basis).ok()
    })
}

fn admit_sibling(
    owner: &Path,
    name: &str,
    expected: [u8; 32],
    dialect: Dialect,
    engine: Engine,
) -> Option<(
    std::path::PathBuf,
    ToolchainArtifact,
    backend_sql_contract::ProducerBasis,
)> {
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
    // The exact admitted worker can report its basis through the same bounded
    // supervisor and process-group retirement used for actual host analysis.
    let limits = backend_compile::ProcessLimits::new(
        8 * 1024 * 1024,
        128 * 1024,
        std::time::Duration::from_secs(10),
        9 * 1024 * 1024,
    )
    .and_then(|limits| limits.with_input_bytes_limit(4 * 1024 * 1024))
    .ok()?;
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let basis = backend_sql_contract::probe_basis_checked(
        CheckedLaunch {
            program: &candidate,
            workspace: candidate.parent()?,
            artifact: artifact.clone(),
            environment: backend_compile::ProcessEnvironment::new(vec![]).ok()?,
            limits,
            cancellation: &cancellation,
        },
        dialect,
        engine,
    )
    .ok()?;
    Some((candidate, artifact, basis))
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
        assert!(
            admit_sibling(
                &owner,
                "backend-sqlite-worker",
                [1; 32],
                Dialect::Sqlite,
                Engine::Syntaqlite4e4de76
            )
            .is_none()
        );
        let worker = directory.path().join("backend-sqlite-worker");
        std::fs::write(&worker, b"not a worker").expect("sibling fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
        }
        assert!(
            admit_sibling(
                &owner,
                "backend-sqlite-worker",
                [1; 32],
                Dialect::Sqlite,
                Engine::Syntaqlite4e4de76
            )
            .is_none()
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn installed_sqlite_admits_exact_worker_and_private_library_without_running_it() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let directory = tempfile::tempdir().expect("bundle fixture");
        let bin = directory.path().join("bin");
        let lib = directory.path().join("lib");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&lib).unwrap();
        let owner = bin.join("backend-locald");
        let worker = bin.join("backend-sqlite-worker");
        let library = lib.join("libgcc_s.so.1");
        std::fs::write(&owner, b"owner").unwrap();
        // Deliberately not a process: installed admission uses the independently
        // compiled basis and does not execute a worker before host supervision.
        std::fs::write(&worker, b"exact worker bytes").unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&library, b"exact private dependency").unwrap();
        let dependency = backend_compile::ExecutableIdentity::from_path(&library).unwrap();
        let artifact = ToolchainArtifact::from_path(&worker, vec![dependency.digest()]).unwrap();
        let anchor = artifact.identity().to_bytes();
        let basis = backend_sql_contract::ProducerBasis {
            engine: Engine::Syntaqlite4e4de76,
            native_revision: "4e4de76312429fb2235ae093ab2125914320d5bd".into(),
            recipe: "syntaqlite-sqlite-fmt-analysis-dqs-default-v1".into(),
            lock_sha256: [2; 32],
            support_sha256: Some([3; 32]),
            settings: backend_sql_contract::EffectiveSettings::Sqlite {
                version: i32::MAX,
                cflags: 0,
                analyzer_dqs_fallback: true,
            },
        };
        let basis = serde_json::to_string(&basis).unwrap();
        assert!(admit_sqlite_sibling(&owner, anchor, Some(&basis)).is_some());
        let empty_closure = ToolchainArtifact::from_path(&worker, vec![])
            .unwrap()
            .identity()
            .to_bytes();
        assert!(admit_sqlite_sibling(&owner, empty_closure, Some(&basis)).is_none());
        assert!(admit_sqlite_sibling(&owner, anchor, Some("{}")).is_none());
        std::fs::write(&library, b"changed dependency").unwrap();
        assert!(admit_sqlite_sibling(&owner, anchor, Some(&basis)).is_none());
        std::fs::write(&library, b"exact private dependency").unwrap();
        std::fs::write(&worker, b"changed worker").unwrap();
        assert!(admit_sqlite_sibling(&owner, anchor, Some(&basis)).is_none());
        std::fs::write(&worker, b"exact worker bytes").unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(admit_sqlite_sibling(&owner, anchor, Some(&basis)).is_none());
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(&library, lib.join("other")).unwrap();
        symlink(lib.join("other"), &library).unwrap();
        assert!(admit_sqlite_sibling(&owner, anchor, Some(&basis)).is_none());
    }
}
