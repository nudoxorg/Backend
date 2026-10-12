//! One caller journal shared by command-line and MCP index requests.
//!
//! The random claim is durably saved before its first transport send. A
//! subsequent invocation reconciles that exact key before considering another
//! mutation. Missing, active and unresolved evidence never creates a new key.

use crate::ClientError;
use backend_library::{
    CompileExecutionIntent, IndexOperationKey, IndexOperationObservation, IndexOperationState,
    PackageReference, SurfaceCommand, SurfaceReply, index_operation_request_digest,
};
use backend_platform::DirectoryCapability;
use backend_runtime::WorkspacePaths;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const JOURNAL_DIRECTORY: &str = "client-index-operations-v1";
const MAX_CLAIM_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    project: PathBuf,
    workspace: PathBuf,
    endpoint: PathBuf,
    authority: PathBuf,
    authority_identity: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u8,
    operation_key: IndexOperationKey,
    package: PackageReference,
    execution_intent: CompileExecutionIntent,
    attachment: Attachment,
}

/// Authorization to replace only an exactly reconciled failed caller claim.
struct FailedOperation<'a>(&'a Claim);

enum ClaimPublication<'a> {
    New,
    AfterFailure(FailedOperation<'a>),
}

enum SavedOperationAction {
    Observe,
    ReplaySameKey,
}

fn saved_operation_action(observation: &IndexOperationObservation) -> SavedOperationAction {
    match observation {
        IndexOperationObservation::Unknown { .. } => SavedOperationAction::ReplaySameKey,
        IndexOperationObservation::Known(status) => match &status.state {
            // The owner's same-key start performs exact orphan recovery; this
            // observation alone never authorizes a replacement operation.
            IndexOperationState::Unresolved {
                reason:
                    backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture,
                ..
            } => SavedOperationAction::ReplaySameKey,
            IndexOperationState::Accepted
            | IndexOperationState::Active { .. }
            | IndexOperationState::Published(_)
            | IndexOperationState::PartiallyPublished { .. }
            | IndexOperationState::Failed { .. }
            | IndexOperationState::Unresolved { .. } => SavedOperationAction::Observe,
        },
        IndexOperationObservation::OutsideReceiptWindow { .. } => SavedOperationAction::Observe,
    }
}

fn failed_operation<'a>(
    claim: &'a Claim,
    observation: &IndexOperationObservation,
) -> Result<Option<FailedOperation<'a>>, ClientError> {
    admit_observation(claim, observation)?;
    Ok(
        matches!(observation, IndexOperationObservation::Known(status)
        if matches!(status.state, IndexOperationState::Failed { .. }))
        .then_some(FailedOperation(claim)),
    )
}

fn io_error(error: impl std::fmt::Display) -> ClientError {
    ClientError::Io(format!(
        "the saved index operation could not be admitted: {error}"
    ))
}

fn attachment(paths: &WorkspacePaths) -> Result<Attachment, ClientError> {
    let mut credential = [0_u8; 32];
    let mut source =
        backend_platform::durable::open_private_read(paths.authority_secret()).map_err(io_error)?;
    if source.metadata().map_err(io_error)?.len() != 32 {
        return Err(io_error("the owner credential is not exactly 32 bytes"));
    }
    source.read_exact(&mut credential).map_err(io_error)?;
    Ok(Attachment {
        project: paths.project().to_path_buf(),
        workspace: paths.data().to_path_buf(),
        endpoint: paths.endpoint().to_path_buf(),
        authority: paths.authority_secret().to_path_buf(),
        // Bind the saved request to this authority across process restarts
        // without storing its credential or treating a PID as its identity.
        authority_identity: blake3::derive_key("Nudox index caller authority v1", &credential),
    })
}

fn fresh_key() -> Result<IndexOperationKey, ClientError> {
    let mut bytes = [0_u8; 32];
    #[cfg(unix)]
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(io_error)?;
    #[cfg(windows)]
    backend_platform::win32::random::fill(&mut bytes).map_err(io_error)?;
    #[cfg(not(any(unix, windows)))]
    return Err(io_error(
        "this platform cannot allocate an index operation key",
    ));
    IndexOperationKey::from_bytes(bytes).map_err(io_error)
}

fn read_claim(directory: &DirectoryCapability, name: &str) -> Result<Option<Claim>, ClientError> {
    let file = match directory.open_private_file_read_write(name, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CLAIM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > MAX_CLAIM_BYTES {
        return Err(io_error("the saved claim exceeds its byte bound"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(io_error)
}

fn save_claim(
    directory: &DirectoryCapability,
    name: &str,
    claim: &Claim,
    publication: ClaimPublication<'_>,
) -> Result<(), ClientError> {
    let replace = match publication {
        ClaimPublication::New => false,
        ClaimPublication::AfterFailure(FailedOperation(previous)) => {
            if read_claim(directory, name)?.as_ref() != Some(previous) {
                return Err(io_error(
                    "the failed caller claim changed before replacement",
                ));
            }
            true
        }
    };
    let bytes = serde_json::to_vec(claim).map_err(io_error)?;
    if bytes.len() as u64 > MAX_CLAIM_BYTES {
        return Err(io_error("the request exceeds the saved claim byte bound"));
    }
    let stage = format!("{}.stage", claim.operation_key);
    let mut file = directory.create_file_exclusive(&stage).map_err(io_error)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = directory.remove_file(&stage);
        return Err(io_error(error));
    }
    drop(file);
    // The held request lock serializes our namespace. A post-rename flush
    // failure retains the canonical claim and never permits a transport send.
    directory.rename(&stage, name, replace).map_err(io_error)
}

fn verify_attachment(
    paths: &WorkspacePaths,
    root: &DirectoryCapability,
    directory: &DirectoryCapability,
    claim: &Claim,
) -> Result<(), ClientError> {
    root.verify_path(paths.data()).map_err(io_error)?;
    directory
        .verify_path(&paths.data().join(JOURNAL_DIRECTORY))
        .map_err(io_error)?;
    if attachment(paths)? != claim.attachment {
        return Err(io_error("the workspace authority changed before dispatch"));
    }
    Ok(())
}

fn start_claim(
    transport: &mut impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    claim: &Claim,
) -> Result<SurfaceReply, ClientError> {
    exchange(
        transport,
        SurfaceCommand::IndexOperationStart {
            operation_key: claim.operation_key,
            package: claim.package.clone(),
            execution_intent: claim.execution_intent,
        },
    )
}

fn reply_observation(reply: &SurfaceReply) -> Result<&IndexOperationObservation, ClientError> {
    match reply {
        SurfaceReply::IndexOperationStarted(observation)
        | SurfaceReply::IndexOperationStatus(observation) => Ok(observation),
        _ => Err(ClientError::Protocol(
            "durable index reply changed shape".to_owned(),
        )),
    }
}

fn admit_observation(
    claim: &Claim,
    observation: &IndexOperationObservation,
) -> Result<(), ClientError> {
    SurfaceReply::IndexOperationStatus(observation.clone())
        .admit(
            SurfaceCommand::IndexOperationStatus {
                operation_key: claim.operation_key,
            }
            .id(),
        )
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let digest = index_operation_request_digest(&claim.package, claim.execution_intent);
    let matches = match observation {
        IndexOperationObservation::Known(status) => {
            status.operation_key == claim.operation_key
                && status.package == claim.package
                && status.execution_intent == claim.execution_intent
                && status.request_digest == digest
        }
        IndexOperationObservation::OutsideReceiptWindow {
            operation_key,
            request_digest,
        } => *operation_key == claim.operation_key && *request_digest == digest,
        IndexOperationObservation::Unknown { operation_key } => {
            *operation_key == claim.operation_key
        }
    };
    if matches {
        Ok(())
    } else {
        Err(ClientError::Protocol(
            "index operation evidence does not match the saved request".to_owned(),
        ))
    }
}

fn terminal(observation: &IndexOperationObservation) -> bool {
    matches!(observation, IndexOperationObservation::Known(status) if matches!(status.state,
        IndexOperationState::Published(_) | IndexOperationState::PartiallyPublished { .. }
        | IndexOperationState::Failed { .. }))
}

fn exchange(
    transport: &mut impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
    command: SurfaceCommand,
) -> Result<SurfaceReply, ClientError> {
    let reply = transport(command.clone())?;
    backend_library::admit_surface_reply(&command, &reply)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    Ok(reply)
}

fn acknowledge_terminal(
    paths: &WorkspacePaths,
    observation: &IndexOperationObservation,
) -> Result<(), ClientError> {
    if !terminal(observation) {
        return Ok(());
    }
    let IndexOperationObservation::Known(status) = observation else {
        return Ok(());
    };
    let root = DirectoryCapability::open(paths.data()).map_err(io_error)?;
    root.validate_private().map_err(io_error)?;
    let directory = match root.open_dir(JOURNAL_DIRECTORY) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(error)),
    };
    directory.validate_private().map_err(io_error)?;
    let name = format!(
        "{}.json",
        blake3::Hash::from(status.request_digest).to_hex()
    );
    // A caller-owned explicit key may have no default claim. This read never
    // creates a journal or a lock for an unrelated operation.
    if read_claim(&directory, &name)?.is_none() {
        return Ok(());
    }
    let lock = directory
        .open_private_file_read_write(&format!("{name}.lock"), false)
        .map_err(io_error)?;
    lock.try_lock().map_err(io_error)?;
    let Some(claim) = read_claim(&directory, &name)? else {
        return Ok(());
    };
    if claim.operation_key != status.operation_key {
        return Ok(());
    }
    if claim.version != 1 || claim.attachment != attachment(paths)? {
        return Err(io_error(
            "the saved claim names a different workspace authority",
        ));
    }
    admit_observation(&claim, observation)?;
    root.verify_path(paths.data()).map_err(io_error)?;
    directory
        .verify_path(&paths.data().join(JOURNAL_DIRECTORY))
        .map_err(io_error)?;
    directory.remove_file(&name).map_err(io_error)?;
    Ok(())
}

/// Executes an existing surface command and acknowledges its exact terminal
/// operation receipt in the shared default-index caller journal.
///
/// Progress for an explicit or unrelated key never clears another request.
/// Accepted, active, unknown and unresolved observations retain their claim.
///
/// # Errors
/// Returns transport, reply admission or matching caller-journal failures.
pub fn surface_with_index_journal(
    paths: &WorkspacePaths,
    command: SurfaceCommand,
    mut transport: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
) -> Result<SurfaceReply, ClientError> {
    let reply = exchange(&mut transport, command)?;
    if let SurfaceReply::IndexOperationStarted(observation)
    | SurfaceReply::IndexOperationStatus(observation) = &reply
    {
        acknowledge_terminal(paths, observation)?;
    }
    Ok(reply)
}

/// Accepts or reconciles one index request under its durable caller identity.
///
/// The supplied transport executes existing typed surface commands. It never
/// receives a start before the exact payload and attachment are durable.
/// Interrupted delivery retains the claim; a retry first reads its status,
/// replaying that same key when it is unknown or its interrupted source
/// capture needs the owner's existing recovery path.
/// A repeated add reconciles an interrupted capture under the same key. Only
/// an exact failed receipt authorizes one fresh, durably saved retry within
/// that invocation. A new attempt is never automatically retried again.
/// Successful terminal results retain the existing acknowledgment behavior.
/// Owner restart, unknown delivery and unresolved publication cannot authorize
/// a replacement operation.
///
/// # Errors
/// Returns journal, attachment, entropy, transport or exact-receipt admission
/// failures. No such failure silently allocates a replacement operation.
pub fn index_with_journal(
    paths: &WorkspacePaths,
    package: PackageReference,
    execution_intent: CompileExecutionIntent,
    mut transport: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
) -> Result<SurfaceReply, ClientError> {
    let package = match package {
        PackageReference::Local(path) => {
            let canonical = backend_runtime::normalize_surface_path(Path::new(path.as_str()));
            PackageReference::parse(
                canonical
                    .to_str()
                    .ok_or_else(|| io_error("the selected package path is not UTF-8"))?
                    .to_owned(),
            )
            .map_err(io_error)?
        }
        package => package,
    };
    let bound_attachment = attachment(paths)?;
    let root = DirectoryCapability::open(paths.data()).map_err(io_error)?;
    root.validate_private().map_err(io_error)?;
    let directory = match root.create_private_dir(JOURNAL_DIRECTORY) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            root.open_dir(JOURNAL_DIRECTORY).map_err(io_error)?
        }
        Err(error) => return Err(io_error(error)),
    };
    directory.validate_private().map_err(io_error)?;
    let digest = index_operation_request_digest(&package, execution_intent);
    let name = format!("{}.json", blake3::Hash::from(digest).to_hex());
    let lock = directory
        .open_private_file_read_write(&format!("{name}.lock"), true)
        .map_err(io_error)?;
    lock.try_lock().map_err(|error| {
        io_error(format!(
            "another caller is reconciling this request; retry: {error}"
        ))
    })?;
    directory.sync_all().map_err(io_error)?;
    root.verify_path(paths.data()).map_err(io_error)?;
    let saved = read_claim(&directory, &name)?;
    let mut claim = match saved.as_ref() {
        Some(claim)
            if claim.version == 1
                && claim.package == package
                && claim.execution_intent == execution_intent
                && claim.attachment == bound_attachment =>
        {
            claim.clone()
        }
        Some(_) => {
            return Err(io_error(
                "the saved claim names a different request or workspace authority",
            ));
        }
        None => {
            let claim = Claim {
                version: 1,
                operation_key: fresh_key()?,
                package,
                execution_intent,
                attachment: bound_attachment,
            };
            save_claim(&directory, &name, &claim, ClaimPublication::New)?;
            claim
        }
    };
    verify_attachment(paths, &root, &directory, &claim)?;
    let observation = if saved.is_some() {
        let reply = exchange(
            &mut transport,
            SurfaceCommand::IndexOperationStatus {
                operation_key: claim.operation_key,
            },
        )?;
        let SurfaceReply::IndexOperationStatus(observation) = reply else {
            return Err(ClientError::Protocol(
                "durable index status reply changed shape".to_owned(),
            ));
        };
        admit_observation(&claim, &observation)?;
        Some(observation)
    } else {
        None
    };
    let mut reply = match observation {
        Some(observation) => match saved_operation_action(&observation) {
            SavedOperationAction::Observe => SurfaceReply::IndexOperationStatus(observation),
            SavedOperationAction::ReplaySameKey => {
                verify_attachment(paths, &root, &directory, &claim)?;
                start_claim(&mut transport, &claim)?
            }
        },
        None => start_claim(&mut transport, &claim)?,
    };
    let observation = reply_observation(&reply)?;
    admit_observation(&claim, observation)?;
    if saved.is_some() {
        if let Some(failed) = failed_operation(&claim, observation)? {
            verify_attachment(paths, &root, &directory, &claim)?;
            let mut next = claim.clone();
            next.operation_key = fresh_key()?;
            if next.operation_key == claim.operation_key {
                return Err(io_error("fresh retry identity equals the failed operation"));
            }
            save_claim(
                &directory,
                &name,
                &next,
                ClaimPublication::AfterFailure(failed),
            )?;
            claim = next;
            verify_attachment(paths, &root, &directory, &claim)?;
            reply = start_claim(&mut transport, &claim)?;
        }
    }
    let observation = reply_observation(&reply)?;
    admit_observation(&claim, observation)?;
    if terminal(observation) {
        root.verify_path(paths.data()).map_err(io_error)?;
        directory.remove_file(&name).map_err(io_error)?;
        directory.sync_all().map_err(io_error)?;
    }
    Ok(reply)
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use backend_library::{IndexOperationFailureReason, IndexOperationStatus, ProductText};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
        paths: WorkspacePaths,
        package: PackageReference,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "nic-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).expect("isolated fixture");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                    .expect("private fixture");
            }
            let project = root.join("project");
            let selected = project.join("backend");
            std::fs::create_dir_all(&selected).expect("selected source operand");
            let paths = WorkspacePaths::discover(
                Some(project),
                Some(root.join("state")),
                Some(root.join("sock")),
            )
            .expect("frozen runtime attachment");
            paths.initialize().expect("private workspace and authority");
            let package =
                PackageReference::parse(selected.to_str().expect("fixture UTF8").to_owned())
                    .expect("package");
            Self {
                root,
                paths,
                package,
            }
        }
        fn index(
            &self,
            call: impl FnMut(SurfaceCommand) -> Result<SurfaceReply, ClientError>,
        ) -> Result<SurfaceReply, ClientError> {
            index_with_journal(
                &self.paths,
                self.package.clone(),
                CompileExecutionIntent::Interactive,
                call,
            )
        }
        fn claims(&self) -> Vec<Claim> {
            std::fs::read_dir(self.paths.data().join(JOURNAL_DIRECTORY))
                .expect("journal")
                .map(|entry| entry.expect("entry").path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                })
                .map(|path| {
                    serde_json::from_slice(&std::fs::read(path).expect("claim bytes"))
                        .expect("claim")
                })
                .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn known(claim: &Claim, state: IndexOperationState) -> IndexOperationObservation {
        IndexOperationObservation::Known(IndexOperationStatus::new(
            claim.operation_key,
            claim.package.clone(),
            claim.execution_intent,
            state,
        ))
    }

    #[test]
    fn held_request_reconciliation_does_not_lock_an_unrelated_project() {
        let fixture = Fixture::new();
        let other_path = fixture.root.join("independent-project");
        std::fs::create_dir(&other_path).expect("independent actual project");
        let other = PackageReference::parse(other_path.to_str().expect("UTF-8 project"))
            .expect("different canonical package");
        let mut first_key = None;
        let mut other_key = None;
        fixture
            .index(|command| {
                let SurfaceCommand::IndexOperationStart { operation_key, .. } = command else {
                    panic!("first invocation starts one saved request");
                };
                first_key = Some(operation_key);
                // The first digest's real file lock is still held inside this
                // transport call. Another project in the same attachment must
                // reach its own typed start, not acquire a global caller lock.
                index_with_journal(
                    &fixture.paths,
                    other.clone(),
                    CompileExecutionIntent::Interactive,
                    |command| {
                        let SurfaceCommand::IndexOperationStart {
                            operation_key,
                            package,
                            execution_intent,
                        } = command
                        else {
                            panic!("independent invocation starts its own request");
                        };
                        assert_eq!(package, other);
                        assert_ne!(Some(operation_key), first_key);
                        other_key = Some(operation_key);
                        Ok(SurfaceReply::IndexOperationStarted(
                            IndexOperationObservation::Known(IndexOperationStatus::new(
                                operation_key,
                                package,
                                execution_intent,
                                IndexOperationState::Accepted,
                            )),
                        ))
                    },
                )
                .expect("no unrelated request serialization");
                let claim = fixture
                    .claims()
                    .into_iter()
                    .find(|claim| claim.operation_key == operation_key)
                    .expect("original exact saved claim");
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("original request retains its admission");
        let claims = fixture.claims();
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().any(
            |claim| Some(claim.operation_key) == first_key && claim.package == fixture.package
        ));
        assert!(
            claims
                .iter()
                .any(|claim| Some(claim.operation_key) == other_key && claim.package == other)
        );
    }

    #[test]
    fn saved_claim_precedes_first_send_and_lost_ack_reconciles_without_new_start() {
        let fixture = Fixture::new();
        let mut original = None;
        assert!(fixture.index(|command| {
            let claims: [Claim; 1] = fixture.claims().try_into().expect("one durable claim");
            let [claim] = claims;
            assert_eq!(claim.package, fixture.package, "selected backend operand is not the Git-root owner attachment");
            assert_eq!(claim.attachment.project, fixture.paths.project());
            assert!(matches!(command, SurfaceCommand::IndexOperationStart { operation_key, ref package, .. }
                if operation_key == claim.operation_key && package == &fixture.package));
            original = Some(claim);
            Err(ClientError::Disconnected(io::ErrorKind::ConnectionReset))
        }).is_err());
        let original = original.expect("sent claim");
        let reply = fixture
            .index(|command| {
                assert_eq!(
                    command,
                    SurfaceCommand::IndexOperationStatus {
                        operation_key: original.operation_key
                    }
                );
                Ok(SurfaceReply::IndexOperationStatus(known(
                    &original,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("exact retry reads the retained owner receipt");
        assert!(matches!(reply, SurfaceReply::IndexOperationStatus(_)));
        assert_eq!(fixture.claims(), [original]);
    }

    #[test]
    fn unknown_on_explicit_retry_replays_only_the_same_durable_key() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .index(|_| Err(ClientError::Disconnected(io::ErrorKind::BrokenPipe)))
                .is_err()
        );
        let claim = fixture.claims().pop().expect("ambiguous claim remains");
        let mut calls = Vec::new();
        fixture
            .index(|command| {
                calls.push(command.clone());
                match command {
                    SurfaceCommand::IndexOperationStatus { operation_key } => {
                        Ok(SurfaceReply::IndexOperationStatus(
                            IndexOperationObservation::Unknown { operation_key },
                        ))
                    }
                    SurfaceCommand::IndexOperationStart { operation_key, .. } => {
                        assert_eq!(operation_key, claim.operation_key);
                        Ok(SurfaceReply::IndexOperationStarted(known(
                            &claim,
                            IndexOperationState::Accepted,
                        )))
                    }
                    _ => Err(ClientError::Protocol("unexpected command".to_owned())),
                }
            })
            .expect("same-key replay");
        assert_eq!(calls.len(), 2);
        assert!(matches!(
            calls[0],
            SurfaceCommand::IndexOperationStatus { .. }
        ));
        assert_eq!(fixture.claims(), [claim]);
    }

    #[test]
    fn unresolved_and_consumed_receipts_never_allocate_a_replacement() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .index(|_| Err(ClientError::Disconnected(io::ErrorKind::BrokenPipe)))
                .is_err()
        );
        let claim = fixture.claims().pop().expect("claim");
        for observation in [
            known(&claim, IndexOperationState::Unresolved {
                reason: backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture,
                detail: ProductText::new("owner restarted before the semantic worker returned").expect("detail"),
            }),
            IndexOperationObservation::OutsideReceiptWindow {
                operation_key: claim.operation_key,
                request_digest: index_operation_request_digest(&claim.package, claim.execution_intent),
            },
        ] {
            let mut calls = 0;
            fixture.index(|command| {
                calls += 1;
                match command {
                    SurfaceCommand::IndexOperationStatus { operation_key } => {
                        assert_eq!(operation_key, claim.operation_key);
                        Ok(SurfaceReply::IndexOperationStatus(observation.clone()))
                    }
                    SurfaceCommand::IndexOperationStart { operation_key, package, execution_intent } => {
                        assert_eq!(operation_key, claim.operation_key);
                        assert_eq!(package, claim.package);
                        assert_eq!(execution_intent, claim.execution_intent);
                        assert!(matches!(observation, IndexOperationObservation::Known(ref status)
                            if matches!(status.state, IndexOperationState::Unresolved {
                                reason: backend_library::IndexOperationUnresolvedReason::SemanticWorkInterruptedAfterCapture, .. })));
                        Ok(SurfaceReply::IndexOperationStarted(observation.clone()))
                    }
                    _ => panic!("only exact reconciliation is allowed"),
                }
            }).expect("truthful retained observation");
            assert_eq!(calls, if matches!(observation, IndexOperationObservation::Known(_)) { 2 } else { 1 });
            assert_eq!(fixture.claims(), [claim.clone()]);
        }
    }

    #[test]
    fn proven_terminal_clears_the_claim_and_a_later_refresh_gets_a_fresh_key() {
        let fixture = Fixture::new();
        let mut keys = Vec::new();
        for _ in 0..2 {
            fixture
                .index(|command| {
                    let claim = fixture.claims().pop().expect("persisted first");
                    assert!(matches!(
                        command,
                        SurfaceCommand::IndexOperationStart { .. }
                    ));
                    keys.push(claim.operation_key);
                    Ok(SurfaceReply::IndexOperationStarted(known(
                        &claim,
                        IndexOperationState::Failed {
                            reason: IndexOperationFailureReason::Cancelled,
                            detail: ProductText::new(
                                "the exact operation was cancelled before publication",
                            )
                            .expect("detail"),
                            compiler_failure: None,
                        },
                    )))
                })
                .expect("admitted terminal");
            assert!(fixture.claims().is_empty());
        }
        assert_ne!(
            keys[0], keys[1],
            "refresh identities come from entropy, not a path or root"
        );
    }

    #[test]
    fn a_terminal_progress_read_allows_the_next_explicit_refresh_immediately() {
        let fixture = Fixture::new();
        fixture
            .index(|_| {
                let claim = fixture.claims().pop().expect("claim before start");
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("accepted");
        let original = fixture.claims().pop().expect("pending claim");
        let command = SurfaceCommand::IndexOperationStatus {
            operation_key: original.operation_key,
        };
        surface_with_index_journal(&fixture.paths, command, |_| {
            Ok(SurfaceReply::IndexOperationStatus(known(
                &original,
                IndexOperationState::Failed {
                    reason: IndexOperationFailureReason::Cancelled,
                    detail: ProductText::new("cancelled before publication").expect("detail"),
                    compiler_failure: None,
                },
            )))
        })
        .expect("terminal observed");
        assert!(fixture.claims().is_empty());
        fixture
            .index(|command| {
                let claim = fixture.claims().pop().expect("fresh claim");
                assert_ne!(claim.operation_key, original.operation_key);
                assert!(
                    matches!(command, SurfaceCommand::IndexOperationStart { .. }),
                    "one refresh invocation starts new work"
                );
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("fresh refresh");
    }

    #[test]
    fn a_terminal_receipt_for_an_explicit_different_key_cannot_clear_pending_work() {
        let fixture = Fixture::new();
        fixture
            .index(|_| {
                let claim = fixture.claims().pop().expect("saved claim");
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("accepted");
        let original = fixture.claims().pop().expect("pending claim");
        let mut other = original.clone();
        other.operation_key = fresh_key().expect("independent key");
        surface_with_index_journal(
            &fixture.paths,
            SurfaceCommand::IndexOperationStatus {
                operation_key: other.operation_key,
            },
            |_| {
                Ok(SurfaceReply::IndexOperationStatus(known(
                    &other,
                    IndexOperationState::Failed {
                        reason: IndexOperationFailureReason::Cancelled,
                        detail: ProductText::new("another operation ended").expect("detail"),
                        compiler_failure: None,
                    },
                )))
            },
        )
        .expect("unrelated terminal reply");
        assert_eq!(fixture.claims(), [original]);
    }

    #[test]
    fn a_status_for_a_different_requested_key_cannot_acknowledge_the_saved_claim() {
        let fixture = Fixture::new();
        fixture
            .index(|_| {
                let claim = fixture.claims().pop().expect("saved claim");
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("accepted");
        let original = fixture.claims().pop().expect("pending claim");
        let requested_key = fresh_key().expect("different requested key");
        assert_ne!(requested_key, original.operation_key);
        let result = surface_with_index_journal(
            &fixture.paths,
            SurfaceCommand::IndexOperationStatus {
                operation_key: requested_key,
            },
            |_| {
                Ok(SurfaceReply::IndexOperationStatus(known(
                    &original,
                    IndexOperationState::Failed {
                        reason: IndexOperationFailureReason::Cancelled,
                        detail: ProductText::new("the saved operation ended").expect("detail"),
                        compiler_failure: None,
                    },
                )))
            },
        );
        assert!(matches!(result, Err(ClientError::Protocol(_))));
        assert_eq!(fixture.claims(), [original]);
    }

    #[test]
    fn legacy_ticket_progress_cannot_acknowledge_a_durable_operation_receipt() {
        let fixture = Fixture::new();
        fixture
            .index(|_| {
                let claim = fixture.claims().pop().expect("saved claim");
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("accepted");
        let original = fixture.claims().pop().expect("pending claim");
        let ticket = backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::new(7).expect("ticket"),
            [0x61; 16],
            fixture.package.clone(),
        );
        let result = surface_with_index_journal(
            &fixture.paths,
            SurfaceCommand::IndexProgress {
                ticket,
                after_sequence: 0,
            },
            |_| {
                Ok(SurfaceReply::IndexOperationStatus(known(
                    &original,
                    IndexOperationState::Failed {
                        reason: IndexOperationFailureReason::Cancelled,
                        detail: ProductText::new("the saved operation ended").expect("detail"),
                        compiler_failure: None,
                    },
                )))
            },
        );
        assert!(matches!(result, Err(ClientError::Protocol(_))));
        assert_eq!(fixture.claims(), [original]);
    }

    #[test]
    fn foreign_reply_and_changed_authority_preserve_the_exact_original_claim() {
        let fixture = Fixture::new();
        assert!(
            fixture
                .index(|command| {
                    let SurfaceCommand::IndexOperationStart { operation_key, .. } = command else {
                        panic!("start");
                    };
                    let mut foreign = operation_key.to_bytes();
                    foreign[0] ^= 0xff;
                    Ok(SurfaceReply::IndexOperationStarted(
                        IndexOperationObservation::Unknown {
                            operation_key: IndexOperationKey::from_bytes(foreign)
                                .expect("foreign nonzero key"),
                        },
                    ))
                })
                .is_err()
        );
        let claim = fixture.claims().pop().expect("original retained");
        backend_platform::durable::write_private_atomic(fixture.paths.authority_secret(), &[9; 32])
            .expect("simulate replaced authority");
        let mut dispatched = false;
        assert!(
            fixture
                .index(|_| {
                    dispatched = true;
                    Err(ClientError::Protocol("must not send".to_owned()))
                })
                .is_err()
        );
        assert!(!dispatched);
        assert_eq!(fixture.claims(), [claim]);
    }

    #[test]
    fn simultaneous_caller_cannot_dispatch_a_second_operation() {
        let fixture = Fixture::new();
        fixture
            .index(|_| {
                let claim = fixture.claims().pop().expect("locked claim");
                let mut dispatched = false;
                assert!(
                    fixture
                        .index(|_| {
                            dispatched = true;
                            Err(ClientError::Protocol("must not send".to_owned()))
                        })
                        .is_err()
                );
                assert!(!dispatched);
                Ok(SurfaceReply::IndexOperationStarted(known(
                    &claim,
                    IndexOperationState::Accepted,
                )))
            })
            .expect("first operation accepted");
        assert_eq!(fixture.claims().len(), 1);
    }

    mod retry_laws {
        use super::*;
        use backend_library::IndexOperationUnresolvedReason as U;

        fn saved(fixture: &Fixture) -> Claim {
            fixture
                .index(|_| {
                    let claim = fixture.claims().pop().expect("durable before send");
                    Ok(SurfaceReply::IndexOperationStarted(known(
                        &claim,
                        IndexOperationState::Accepted,
                    )))
                })
                .expect("saved accepted operation");
            fixture.claims().pop().expect("accepted claim retained")
        }

        fn failed(claim: &Claim, reason: IndexOperationFailureReason) -> IndexOperationObservation {
            known(
                claim,
                IndexOperationState::Failed {
                    reason,
                    detail: ProductText::from_static(
                        "the exact operation ended without publication",
                    ),
                    compiler_failure: None,
                },
            )
        }

        fn interrupted(claim: &Claim) -> IndexOperationObservation {
            known(
                claim,
                IndexOperationState::Unresolved {
                    reason: U::SemanticWorkInterruptedAfterCapture,
                    detail: ProductText::from_static(
                        "source capture committed but no semantic terminal is proven",
                    ),
                },
            )
        }

        #[test]
        fn repeated_add_retries_each_saved_failed_operation_once_with_a_durable_fresh_key() {
            for reason in [
                IndexOperationFailureReason::Cancelled,
                IndexOperationFailureReason::Refused,
                IndexOperationFailureReason::WorkerFailed,
                IndexOperationFailureReason::LedgerFull,
            ] {
                let fixture = Fixture::new();
                let original = saved(&fixture);
                let terminal = failed(&original, reason);
                let mut commands = Vec::new();
                let mut replacement = None;
                let reply = fixture
                    .index(|command| {
                        commands.push(command.clone());
                        match command {
                            SurfaceCommand::IndexOperationStatus { operation_key } => {
                                assert_eq!(operation_key, original.operation_key);
                                Ok(SurfaceReply::IndexOperationStatus(terminal.clone()))
                            }
                            SurfaceCommand::IndexOperationStart {
                                operation_key,
                                package,
                                execution_intent,
                            } => {
                                let next = fixture
                                    .claims()
                                    .pop()
                                    .expect("replacement is durable before send");
                                assert_ne!(operation_key, original.operation_key);
                                assert_eq!(operation_key, next.operation_key);
                                assert_eq!(package, original.package);
                                assert_eq!(execution_intent, original.execution_intent);
                                assert_eq!(next.attachment, original.attachment);
                                replacement = Some(next.clone());
                                Ok(SurfaceReply::IndexOperationStarted(known(
                                    &next,
                                    IndexOperationState::Accepted,
                                )))
                            }
                            _ => panic!("unexpected command"),
                        }
                    })
                    .expect("one ordinary repeat performs deliberate retry");
                let next = replacement.expect("fresh mutation sent");
                assert_eq!(commands.len(), 2);
                assert_eq!(
                    reply,
                    SurfaceReply::IndexOperationStarted(known(
                        &next,
                        IndexOperationState::Accepted
                    ))
                );
                assert_eq!(fixture.claims(), [next]);
            }
        }

        #[test]
        fn interrupted_capture_recovery_uses_original_key_then_stops_after_one_fresh_failure() {
            let fixture = Fixture::new();
            let original = saved(&fixture);
            let mut commands = Vec::new();
            let mut fresh = None;
            let reply = fixture
                .index(|command| {
                    commands.push(command.clone());
                    match command {
                        SurfaceCommand::IndexOperationStatus { operation_key } => {
                            assert_eq!(operation_key, original.operation_key);
                            Ok(SurfaceReply::IndexOperationStatus(interrupted(&original)))
                        }
                        SurfaceCommand::IndexOperationStart { operation_key, .. }
                            if operation_key == original.operation_key =>
                        {
                            assert_eq!(fixture.claims(), [original.clone()]);
                            Ok(SurfaceReply::IndexOperationStarted(failed(
                                &original,
                                IndexOperationFailureReason::WorkerFailed,
                            )))
                        }
                        SurfaceCommand::IndexOperationStart { operation_key, .. } => {
                            let next = fixture
                                .claims()
                                .pop()
                                .expect("fresh claim before new attempt");
                            assert_eq!(operation_key, next.operation_key);
                            assert_ne!(operation_key, original.operation_key);
                            fresh = Some(next.clone());
                            Ok(SurfaceReply::IndexOperationStarted(failed(
                                &next,
                                IndexOperationFailureReason::Cancelled,
                            )))
                        }
                        _ => panic!("unexpected command"),
                    }
                })
                .expect("bounded recovery and one retry");
            assert_eq!(
                commands.len(),
                3,
                "new terminal failure cannot start a retry loop"
            );
            assert_eq!(
                reply,
                SurfaceReply::IndexOperationStarted(failed(
                    &fresh.expect("one fresh attempt"),
                    IndexOperationFailureReason::Cancelled
                ))
            );
            assert!(
                fixture.claims().is_empty(),
                "terminal fresh receipt is acknowledged"
            );
        }

        #[test]
        fn unknown_delivery_replays_saved_key_before_a_proven_failure_allows_fresh_retry() {
            let fixture = Fixture::new();
            let original = saved(&fixture);
            let mut calls = 0;
            fixture
                .index(|command| {
                    calls += 1;
                    match command {
                        SurfaceCommand::IndexOperationStatus { operation_key } => {
                            Ok(SurfaceReply::IndexOperationStatus(
                                IndexOperationObservation::Unknown { operation_key },
                            ))
                        }
                        SurfaceCommand::IndexOperationStart { operation_key, .. }
                            if operation_key == original.operation_key =>
                        {
                            assert_eq!(fixture.claims(), [original.clone()]);
                            Ok(SurfaceReply::IndexOperationStarted(failed(
                                &original,
                                IndexOperationFailureReason::WorkerFailed,
                            )))
                        }
                        SurfaceCommand::IndexOperationStart { operation_key, .. } => {
                            let next = fixture.claims().pop().expect("durable fresh retry");
                            assert_eq!(operation_key, next.operation_key);
                            Ok(SurfaceReply::IndexOperationStarted(known(
                                &next,
                                IndexOperationState::Accepted,
                            )))
                        }
                        _ => panic!("unexpected command"),
                    }
                })
                .expect("only admitted same-key failure permits replacement");
            assert_eq!(calls, 3);
            assert_ne!(fixture.claims()[0].operation_key, original.operation_key);
        }

        #[test]
        fn other_unresolved_publication_reasons_never_replay_or_replace() {
            for reason in [
                U::RestartedDuringPublication,
                U::WorkspaceEvidenceMismatch,
                U::ViewEvidenceMismatch,
                U::ReceiptPersistenceFailed,
            ] {
                let fixture = Fixture::new();
                let original = saved(&fixture);
                let observation = known(
                    &original,
                    IndexOperationState::Unresolved {
                        reason,
                        detail: ProductText::from_static("publication remains unproven"),
                    },
                );
                let mut calls = 0;
                assert_eq!(
                    fixture
                        .index(|command| {
                            calls += 1;
                            assert_eq!(
                                command,
                                SurfaceCommand::IndexOperationStatus {
                                    operation_key: original.operation_key
                                }
                            );
                            Ok(SurfaceReply::IndexOperationStatus(observation.clone()))
                        })
                        .expect("retained uncertainty"),
                    SurfaceReply::IndexOperationStatus(observation)
                );
                assert_eq!(calls, 1);
                assert_eq!(fixture.claims(), [original]);
            }
        }

        fn publication() -> backend_library::IndexOperationPublicationReceipt {
            let root = backend_library::view_state_root(&[]);
            let basis = backend_library::Basis::new(
                root,
                backend_library::object_version(b"retry-success-fixture"),
            );
            let frontier = backend_library::canonical::Frontier::new(
                backend_library::branch_key("main"),
                backend_library::log_key("library"),
                backend_library::CURSOR_SCHEMA,
                root,
                0,
            );
            let view = backend_library::ViewRoot::new_incomplete(
                backend_library::view_key(b"retry-success-view"),
                basis,
                frontier,
                Vec::new(),
                Vec::new(),
            )
            .expect("actual typed view");
            backend_library::IndexOperationPublicationReceipt::from_published_view(
                Some([1; 32]),
                [2; 32],
                [3; 32],
                2,
                &view,
                backend_library::Cursor::for_view_root(&view),
            )
            .expect("bound publication")
        }

        #[test]
        fn accepted_active_and_published_saved_operations_are_only_observed() {
            let publication = publication();
            for state in [
                IndexOperationState::Accepted,
                IndexOperationState::Active {
                    ticket: backend_library::IndexJobTicket::new(
                        std::num::NonZeroU64::new(7).expect("ticket"),
                        [1; 16],
                        PackageReference::parse("/fixture/placeholder").expect("package"),
                    ),
                    stage: backend_library::IndexJobStage::Scanning,
                },
                IndexOperationState::Published(publication),
            ] {
                let fixture = Fixture::new();
                let original = saved(&fixture);
                let state = match state {
                    IndexOperationState::Active { stage, .. } => IndexOperationState::Active {
                        ticket: backend_library::IndexJobTicket::new(
                            std::num::NonZeroU64::new(7).expect("ticket"),
                            [1; 16],
                            original.package.clone(),
                        ),
                        stage,
                    },
                    state => state,
                };
                let terminal = matches!(state, IndexOperationState::Published(_));
                let observation = known(&original, state);
                let mut calls = 0;
                assert_eq!(
                    fixture
                        .index(|command| {
                            calls += 1;
                            assert_eq!(
                                command,
                                SurfaceCommand::IndexOperationStatus {
                                    operation_key: original.operation_key
                                }
                            );
                            Ok(SurfaceReply::IndexOperationStatus(observation.clone()))
                        })
                        .expect("safe original operation replay"),
                    SurfaceReply::IndexOperationStatus(observation)
                );
                assert_eq!(calls, 1, "no second mutation for live or successful work");
                assert_eq!(fixture.claims().is_empty(), terminal);
                if !terminal {
                    assert_eq!(fixture.claims(), [original]);
                }
            }
        }

        #[test]
        fn partial_publication_retains_its_exact_refusals_without_a_replacement_start() {
            use backend_library::{
                IndexOperationSemanticProfileState as S,
                IndexOperationSemanticUnavailableReason as R,
            };
            let fixture = Fixture::new();
            let original = saved(&fixture);
            let published_profile =
                backend_library::SemanticLanguageProfile::from_name("typescript")
                    .expect("closed TypeScript profile");
            let refused_profile = backend_library::SemanticLanguageProfile::from_name("go")
                .expect("closed Go profile");
            let capture = backend_library::IndexOperationSourceCaptureReceipt::from_checked_parts(
                original.operation_key,
                [7; 32],
                [8; 32],
                1,
                Box::new([
                    backend_library::IndexOperationSourceProfile {
                        profile: published_profile,
                        source_version: [4; 32],
                        input_digest: [5; 32],
                        observation_sequence: 1,
                        source_count: 1,
                        state: S::Published {
                            generation: [6; 32],
                            coverage: backend_library::IndexOperationSemanticCoverage::Complete,
                        },
                    },
                    backend_library::IndexOperationSourceProfile {
                        profile: refused_profile,
                        source_version: [4; 32],
                        input_digest: [9; 32],
                        observation_sequence: 1,
                        source_count: 1,
                        state: S::Unavailable {
                            reason: R::Toolchain,
                        },
                    },
                ]),
            )
            .expect("admitted source profile partition");
            let observation = IndexOperationObservation::Known(
                IndexOperationStatus::new(
                    original.operation_key,
                    original.package.clone(),
                    original.execution_intent,
                    IndexOperationState::PartiallyPublished {
                        receipt: publication(),
                        refused_profiles: Box::new([
                            backend_library::IndexOperationProfileRefusal {
                                profile: refused_profile,
                                reason: R::Toolchain,
                                compiler_failure: None,
                            },
                        ]),
                    },
                )
                .with_source_capture(Some(capture)),
            );
            let mut calls = 0;
            assert_eq!(
                fixture
                    .index(|command| {
                        calls += 1;
                        assert_eq!(
                            command,
                            SurfaceCommand::IndexOperationStatus {
                                operation_key: original.operation_key
                            }
                        );
                        Ok(SurfaceReply::IndexOperationStatus(observation.clone()))
                    })
                    .expect("partial is observed as its own terminal receipt"),
                SurfaceReply::IndexOperationStatus(observation)
            );
            assert_eq!(
                calls, 1,
                "partial publication is never mistaken for failed work"
            );
            assert!(
                fixture.claims().is_empty(),
                "existing terminal acknowledgment retained"
            );
        }

        #[test]
        fn lost_fresh_retry_delivery_reconciles_fresh_identity_instead_of_allocating_another() {
            let fixture = Fixture::new();
            let original = saved(&fixture);
            assert!(
                fixture
                    .index(|command| match command {
                        SurfaceCommand::IndexOperationStatus { .. } =>
                            Ok(SurfaceReply::IndexOperationStatus(failed(
                                &original,
                                IndexOperationFailureReason::WorkerFailed
                            ))),
                        SurfaceCommand::IndexOperationStart { .. } => Err(
                            ClientError::MutationOutcomeUnknown(io::ErrorKind::ConnectionReset)
                        ),
                        _ => panic!("unexpected command"),
                    })
                    .is_err()
            );
            let next = fixture
                .claims()
                .pop()
                .expect("fresh delivery ambiguity remains durable");
            assert_ne!(next.operation_key, original.operation_key);
            let mut calls = 0;
            fixture
                .index(|command| {
                    calls += 1;
                    assert_eq!(
                        command,
                        SurfaceCommand::IndexOperationStatus {
                            operation_key: next.operation_key
                        }
                    );
                    Ok(SurfaceReply::IndexOperationStatus(known(
                        &next,
                        IndexOperationState::Accepted,
                    )))
                })
                .expect("reconcile ambiguous fresh delivery");
            assert_eq!(calls, 1);
            assert_eq!(fixture.claims(), [next]);
        }

        #[test]
        fn foreign_failed_receipt_and_changed_authority_cannot_replace_claim() {
            for foreign in [true, false] {
                let fixture = Fixture::new();
                let original = saved(&fixture);
                let mut calls = 0;
                let result = fixture.index(|command| {
                    calls += 1;
                    assert_eq!(
                        command,
                        SurfaceCommand::IndexOperationStatus {
                            operation_key: original.operation_key
                        }
                    );
                    let mut claim = original.clone();
                    if foreign {
                        claim.operation_key = fresh_key().expect("foreign key");
                    } else {
                        backend_platform::durable::write_private_atomic(
                            fixture.paths.authority_secret(),
                            &[9; 32],
                        )
                        .expect("replace owner authority");
                    }
                    Ok(SurfaceReply::IndexOperationStatus(failed(
                        &claim,
                        IndexOperationFailureReason::WorkerFailed,
                    )))
                });
                assert!(result.is_err());
                assert_eq!(calls, 1, "no replacement mutation is sent");
                assert_eq!(fixture.claims(), [original]);
            }
        }

        #[test]
        fn concurrent_caller_cannot_dispatch_during_failed_claim_replacement() {
            let fixture = Fixture::new();
            let original = saved(&fixture);
            let mut calls = 0;
            fixture
                .index(|command| {
                    calls += 1;
                    let mut conflicting_send = false;
                    assert!(
                        fixture
                            .index(|_| {
                                conflicting_send = true;
                                Err(ClientError::Io("must not send".into()))
                            })
                            .is_err()
                    );
                    assert!(!conflicting_send);
                    match command {
                        SurfaceCommand::IndexOperationStatus { .. } => {
                            Ok(SurfaceReply::IndexOperationStatus(failed(
                                &original,
                                IndexOperationFailureReason::Cancelled,
                            )))
                        }
                        SurfaceCommand::IndexOperationStart { operation_key, .. } => {
                            let next = fixture
                                .claims()
                                .pop()
                                .expect("new claim under same held lock");
                            assert_eq!(operation_key, next.operation_key);
                            assert_ne!(operation_key, original.operation_key);
                            Ok(SurfaceReply::IndexOperationStarted(known(
                                &next,
                                IndexOperationState::Accepted,
                            )))
                        }
                        _ => panic!("unexpected command"),
                    }
                })
                .expect("one owning caller");
            assert_eq!(calls, 2);
            assert_eq!(fixture.claims().len(), 1);
        }
    }
}
