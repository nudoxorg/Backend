#![allow(clippy::expect_used, clippy::panic)]

use backend_client::{ClientError, Session};
use backend_library::{
    CommandFailure, CommandReply, CompileExecutionIntent, IndexOperationKey,
    IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState,
    PackageReference,
};
use backend_local_service::{EmbeddedLocalService, LocalHostVariable, ProcessConfig};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const PUBLIC_MARKER: &str = "operation_lifecycle_public_marker";

#[test]
fn public_index_operation_replays_and_conflicts_across_restart() -> Result<(), Box<dyn Error>> {
    let fixture = tempfile::Builder::new()
        .prefix("backend-index-op-")
        .tempdir_in("/tmp")?;
    let service_workspace = fixture.path().join("service-state");
    create_private_directory(&service_workspace)?;
    let endpoint = fixture.path().join("owner.sock");

    let package_root = fixture.path().join("real-cargo-package");
    fs::create_dir_all(package_root.join("src"))?;
    fs::write(
        package_root.join("Cargo.toml"),
        "[package]\nname = \"public_operation_lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(
        package_root.join("src/lib.rs"),
        format!("pub mod child;\npub fn {PUBLIC_MARKER}() -> u32 {{ child::answer() }}\n"),
    )?;
    fs::write(
        package_root.join("src/child.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )?;
    let package_lock = package_root.join("Cargo.lock");
    assert!(!package_lock.exists(), "the Cargo input starts lockless");

    let package =
        PackageReference::parse(package_root.canonicalize()?.to_string_lossy().into_owned())?;
    let key_file = fixture.path().join("caller-operation-key.txt");
    let operation_key = IndexOperationKey::from_bytes([0x6d; 32])?;
    {
        let mut key_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&key_file)?;
        key_file.write_all(operation_key.to_hex().as_bytes())?;
        key_file.sync_all()?;
    }

    let mut config = ProcessConfig::parse([
        "--endpoint".to_owned(),
        endpoint.to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        service_workspace.to_string_lossy().into_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.profile = "builtin".to_owned();
    config.worker_endpoint = None;
    config.authority_secret = Some(service_workspace.join("authority.secret"));
    config.compiler_environment = vec![
        (LocalHostVariable::NudoxRustc, executable_in_path("rustc")?),
        (LocalHostVariable::NudoxCargo, executable_in_path("cargo")?),
        (LocalHostVariable::NudoxCargoHome, cargo_home()?),
    ];

    let owner = EmbeddedLocalService::start(config.clone())?;
    let mut session = Session::connect(owner.endpoint())?;
    let started = session.start_index_operation(
        operation_key,
        package.clone(),
        CompileExecutionIntent::Interactive,
    )?;
    let published = wait_for_published(&mut session, operation_key, started)?;
    assert_eq!(published.package, package);
    assert_eq!(
        published.execution_intent,
        CompileExecutionIntent::Interactive
    );
    let IndexOperationState::Published(receipt) = &published.state else {
        panic!("the first real Cargo index operation must publish");
    };
    assert!(
        receipt.request_identity().is_some(),
        "the source package must produce a durable workspace commit"
    );
    assert!(receipt.workspace_sequence() > 0);
    assert_ne!(receipt.commit_identity(), &[0; 32]);
    assert!(
        !package_lock.exists(),
        "Cargo metadata must keep its generated lock out of the source package"
    );

    let names = session.names(PUBLIC_MARKER, 16)?;
    let CommandReply::Names(names) = names.reply else {
        panic!("authenticated name query must return the name view");
    };
    assert!(
        names
            .root
            .rows()
            .iter()
            .any(|row| row.label == PUBLIC_MARKER && row.kind.is_some()),
        "the public Session must expose the declaration compiled from the real package"
    );
    let published_root = session.revision()?.root;
    assert_eq!(published_root.as_bytes(), receipt.view_root());

    let first_receipt = receipt.clone();
    let first_observation = IndexOperationObservation::Known(published);
    drop(session);
    owner.close()?;

    let persisted = fs::read_to_string(&key_file)?;
    let restored_key = IndexOperationKey::parse_hex(persisted.trim())?;
    assert_eq!(restored_key, operation_key);
    let restarted_owner = EmbeddedLocalService::start(config)?;
    let mut restarted_session = Session::connect(restarted_owner.endpoint())?;

    let after_restart = restarted_session.index_operation_status(restored_key)?;
    assert_eq!(after_restart, first_observation);
    assert_same_publication_receipt(&after_restart, &first_receipt);
    let replay = restarted_session.start_index_operation(
        restored_key,
        package.clone(),
        CompileExecutionIntent::Interactive,
    )?;
    assert_eq!(replay, first_observation);
    assert_same_publication_receipt(&replay, &first_receipt);
    let status_after_replay = restarted_session.index_operation_status(restored_key)?;
    assert_eq!(
        status_after_replay, first_observation,
        "exact replay must retain the original publication receipt"
    );
    assert_same_publication_receipt(&status_after_replay, &first_receipt);
    let after_replay_root = restarted_session.revision()?.root;
    assert_eq!(
        after_replay_root.as_bytes(),
        first_receipt.view_root(),
        "exact replay must not publish a different view"
    );

    let conflict = restarted_session.start_index_operation(
        restored_key,
        package,
        CompileExecutionIntent::Background,
    );
    assert!(matches!(
        conflict,
        Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))
            if detail == "index-operation key was reused with another request"
    ));
    let status_after_conflict = restarted_session.index_operation_status(restored_key)?;
    assert_eq!(
        status_after_conflict, first_observation,
        "a conflicting replay must leave the published operation untouched"
    );
    assert_same_publication_receipt(&status_after_conflict, &first_receipt);
    let root_after_conflict = restarted_session.revision()?.root;
    assert_eq!(
        root_after_conflict.as_bytes(),
        first_receipt.view_root(),
        "a conflicting replay must not change the published view root"
    );

    drop(restarted_session);
    restarted_owner.close()?;
    Ok(())
}

fn assert_same_publication_receipt(
    observation: &IndexOperationObservation,
    expected: &IndexOperationPublicationReceipt,
) {
    let IndexOperationObservation::Known(status) = observation else {
        panic!("a persisted operation must remain known: {observation:?}");
    };
    let IndexOperationState::Published(receipt) = &status.state else {
        panic!("the persisted operation must remain published: {status:?}");
    };
    assert_eq!(receipt.request_identity(), expected.request_identity());
    assert_eq!(receipt.commit_identity(), expected.commit_identity());
    assert_eq!(receipt.workspace_root(), expected.workspace_root());
    assert_eq!(receipt.workspace_sequence(), expected.workspace_sequence());
    assert_eq!(receipt.view_root(), expected.view_root());
    assert_eq!(receipt.view_version(), expected.view_version());
    assert_eq!(receipt.view_recipe(), expected.view_recipe());
}

fn wait_for_published(
    session: &mut Session,
    operation_key: IndexOperationKey,
    mut observation: IndexOperationObservation,
) -> Result<backend_library::IndexOperationStatus, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(660);
    loop {
        let IndexOperationObservation::Known(status) = observation else {
            panic!("accepted operation must retain a known status: {observation:?}");
        };
        match &status.state {
            IndexOperationState::Published(_) => return Ok(status),
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {
                assert!(
                    Instant::now() < deadline,
                    "real Cargo compilation did not reach a terminal receipt"
                );
                thread::sleep(Duration::from_millis(50));
                observation = session.index_operation_status(operation_key)?;
            }
            IndexOperationState::Failed { .. } | IndexOperationState::Unresolved { .. } => {
                panic!("real Cargo operation did not publish: {status:?}");
            }
        }
    }
}

fn executable_in_path(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os("PATH").ok_or("the test process has no PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("could not find {name} in the test process PATH").into())
}

fn cargo_home() -> Result<PathBuf, Box<dyn Error>> {
    let path = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("the test process has no Cargo home")?;
    if !path.is_dir() {
        return Err(format!("Cargo home is not a directory: {}", path.display()).into());
    }
    Ok(path)
}

fn create_private_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)?;
    }
    Ok(())
}
