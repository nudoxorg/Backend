#![cfg(any(unix, windows))]
#![allow(clippy::expect_used, clippy::panic)]

// The public service/client lifecycle uses the repository's AF_UNIX transport.
// Unix and Windows implement it; other targets intentionally do not expose a
// local listener, so this integration test is not compiled there.

use backend_client::{ClientError, Session};
use backend_library::{
    CommandFailure, CommandReply, CompileExecutionIntent, IndexOperationKey,
    IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState,
    PackageReference,
};
use backend_local_service::{EmbeddedLocalService, LocalHostVariable, ProcessConfig};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

const PUBLIC_MARKER: &str = "operation_lifecycle_public_marker";

#[test]
fn public_index_operation_replays_and_conflicts_across_restart() -> Result<(), Box<dyn Error>> {
    let mut fixture = FailureFixture::new(lifecycle_tempdir()?);
    let result = run_public_index_operation_lifecycle(&fixture);
    if result.is_err() {
        fixture.preserve_after_failure();
    }
    result
}

fn run_public_index_operation_lifecycle(fixture: &FailureFixture) -> Result<(), Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::symlink_metadata(fixture.path())?.permissions().mode() & 0o777,
            0o700,
            "the temporary project root must be owner-only before workspace initialization"
        );
    }
    let service_workspace = fixture.path().join("service-state");
    let endpoint = fixture.path().join("owner.sock");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().to_path_buf()),
        Some(service_workspace),
        Some(endpoint),
    )?;
    let authority_secret = paths.authority_secret().to_path_buf();
    assert_eq!(
        authority_secret.parent(),
        Some(paths.data()),
        "the fixture authority credential must stay inside its selected workspace"
    );
    paths.initialize()?;
    let authority_credential = fs::read(&authority_secret)?;
    assert_eq!(authority_credential.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(paths.data())?.permissions().mode() & 0o777,
            0o700,
            "the initialized workspace directory must be owner-only"
        );
        assert_eq!(
            fs::metadata(&authority_secret)?.permissions().mode() & 0o777,
            0o600,
            "the initialized authority credential must be owner-only"
        );
    }
    assert!(
        backend_engine::UnixEndpointRef::new(paths.endpoint()).is_ok(),
        "the platform temp root leaves no room for the portable AF_UNIX endpoint"
    );

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
        PackageReference::parse(package_root.canonicalize()?.to_string_lossy().into_owned())
            .map_err(|error| {
                io::Error::other(format!("invalid real fixture package reference: {error}"))
            })?;
    let key_file = fixture.path().join("caller-operation-key.txt");
    let operation_key = IndexOperationKey::from_bytes([0x6d; 32])
        .map_err(|error| io::Error::other(format!("invalid fixture operation key: {error}")))?;
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
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        paths.data().to_string_lossy().into_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.profile = "builtin".to_owned();
    config.worker_endpoint = None;
    config.authority_secret = Some(authority_secret.clone());
    assert_eq!(
        config.authority_secret.as_deref(),
        Some(authority_secret.as_path())
    );
    config.compiler_environment = vec![
        (LocalHostVariable::NudoxRustc, executable_in_path("rustc")?),
        (LocalHostVariable::NudoxCargo, executable_in_path("cargo")?),
        (LocalHostVariable::NudoxCargoHome, cargo_home()?),
    ];

    let owner = with_phase_context(
        "start initial embedded owner",
        EmbeddedLocalService::start(config.clone()),
    )?;
    let mut session = with_phase_context(
        "connect initial authenticated public Session",
        Session::connect(owner.endpoint()),
    )?;
    let started = with_phase_context(
        "submit caller-keyed interactive index operation",
        session.start_index_operation(
            operation_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    let published = with_phase_context(
        "poll initial operation until publication",
        wait_for_published(&mut session, operation_key, started),
    )?;
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

    let names = with_phase_context(
        "query the published marker through public Session",
        session.names(PUBLIC_MARKER, 16),
    )?;
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
    let published_root = with_phase_context(
        "read initial published workspace revision",
        session.revision(),
    )?
    .root;
    assert_eq!(published_root.as_bytes(), receipt.view_root());

    let first_receipt = receipt.clone();
    let first_observation = IndexOperationObservation::Known(published);
    drop(session);
    with_phase_context("close initial embedded owner", owner.close())?;
    assert!(
        fs::read(&authority_secret)? == authority_credential,
        "closing the owner must preserve the initialized workspace credential"
    );

    let persisted = fs::read_to_string(&key_file)?;
    let restored_key = IndexOperationKey::parse_hex(persisted.trim()).map_err(|error| {
        io::Error::other(format!(
            "persisted caller operation key is invalid: {error}"
        ))
    })?;
    assert_eq!(restored_key, operation_key);
    let restarted_owner = with_phase_context(
        "restart embedded owner from the same durable workspace paths",
        EmbeddedLocalService::start(config),
    )?;
    assert!(
        fs::read(&authority_secret)? == authority_credential,
        "restarting the owner must reuse the same durable workspace credential"
    );
    let mut restarted_session = with_phase_context(
        "connect authenticated public Session after restart",
        Session::connect(restarted_owner.endpoint()),
    )?;

    let after_restart = with_phase_context(
        "read caller-keyed operation status after restart",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(after_restart, first_observation);
    assert_same_publication_receipt(&after_restart, &first_receipt);
    let replay = with_phase_context(
        "replay the exact caller-keyed request after restart",
        restarted_session.start_index_operation(
            restored_key,
            package.clone(),
            CompileExecutionIntent::Interactive,
        ),
    )?;
    assert_eq!(replay, first_observation);
    assert_same_publication_receipt(&replay, &first_receipt);
    let status_after_replay = with_phase_context(
        "read operation status after exact replay",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(
        status_after_replay, first_observation,
        "exact replay must retain the original publication receipt"
    );
    assert_same_publication_receipt(&status_after_replay, &first_receipt);
    let after_replay_root = with_phase_context(
        "read workspace revision after exact replay",
        restarted_session.revision(),
    )?
    .root;
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
    match conflict {
        Err(ClientError::CommandFailed(CommandFailure::InvalidQuery(detail)))
            if detail == "index-operation key was reused with another request" => {}
        Err(error) => {
            return Err(Box::new(PhaseError::from_source(
                "submit conflicting request under the persisted key",
                error,
            )));
        }
        Ok(observation) => {
            return Err(Box::new(PhaseError::message(
                "submit conflicting request under the persisted key",
                format!("expected the typed key-conflict rejection, got {observation:?}"),
            )));
        }
    }
    let status_after_conflict = with_phase_context(
        "read operation status after key-conflict rejection",
        restarted_session.index_operation_status(restored_key),
    )?;
    assert_eq!(
        status_after_conflict, first_observation,
        "a conflicting replay must leave the published operation untouched"
    );
    assert_same_publication_receipt(&status_after_conflict, &first_receipt);
    let root_after_conflict = with_phase_context(
        "read workspace revision after key-conflict rejection",
        restarted_session.revision(),
    )?
    .root;
    assert_eq!(
        root_after_conflict.as_bytes(),
        first_receipt.view_root(),
        "a conflicting replay must not change the published view root"
    );

    drop(restarted_session);
    with_phase_context("close restarted embedded owner", restarted_owner.close())?;
    Ok(())
}

#[derive(Debug)]
struct FailureFixture {
    tempdir: tempfile::TempDir,
    retained: bool,
}

impl FailureFixture {
    fn new(tempdir: tempfile::TempDir) -> Self {
        Self {
            tempdir,
            retained: false,
        }
    }

    fn path(&self) -> &Path {
        self.tempdir.path()
    }

    fn preserve_after_failure(&mut self) {
        if !self.retained {
            self.tempdir.disable_cleanup(true);
            self.retained = true;
            let _ = writeln!(
                io::stderr(),
                "public index-operation failure fixture retained at {}",
                self.tempdir.path().display()
            );
        }
    }
}

impl Drop for FailureFixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.preserve_after_failure();
        }
    }
}

#[derive(Debug)]
struct PhaseError {
    phase: &'static str,
    message: String,
    source: Option<Box<dyn Error>>,
}

impl PhaseError {
    fn from_source<E: Error + 'static>(phase: &'static str, source: E) -> Self {
        let message = source.to_string();
        Self {
            phase,
            message,
            source: Some(Box::new(source)),
        }
    }

    fn message(phase: &'static str, message: String) -> Self {
        Self {
            phase,
            message,
            source: None,
        }
    }
}

impl std::fmt::Display for PhaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.phase, self.message)
    }
}

impl Error for PhaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref()
    }
}

fn with_phase_context<T, E: Error + 'static>(
    phase: &'static str,
    result: Result<T, E>,
) -> Result<T, PhaseError> {
    result.map_err(|source| PhaseError::from_source(phase, source))
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
                observation = with_phase_context(
                    "poll durable operation status while awaiting publication",
                    session.index_operation_status(operation_key),
                )?;
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
        #[cfg(windows)]
        let candidate = directory.join(format!("{name}.exe"));
        #[cfg(not(windows))]
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
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .ok_or("the test process has no Cargo home")?;
    if !path.is_dir() {
        return Err(format!("Cargo home is not a directory: {}", path.display()).into());
    }
    Ok(path)
}

fn lifecycle_tempdir() -> Result<tempfile::TempDir, Box<dyn Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut builder = tempfile::Builder::new();
        builder
            .prefix("b-")
            .permissions(fs::Permissions::from_mode(0o700));
        Ok(builder.tempdir_in("/tmp")?)
    }
    #[cfg(windows)]
    {
        Ok(tempfile::Builder::new().prefix("b-").tempdir()?)
    }
}
