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
) -> Result<(), ClientError> {
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
    directory.rename(&stage, name, false).map_err(io_error)
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
/// replaying only that same key when the owner explicitly returns Unknown.
/// A proven terminal result clears the claim, so a later explicit refresh
/// allocates a new random operation. Owner restart alone cannot clear it.
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
    let claim = match saved.as_ref() {
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
            save_claim(&directory, &name, &claim)?;
            claim
        }
    };
    root.verify_path(paths.data()).map_err(io_error)?;
    directory
        .verify_path(&paths.data().join(JOURNAL_DIRECTORY))
        .map_err(io_error)?;
    if attachment(paths)? != claim.attachment {
        return Err(io_error("the workspace authority changed before dispatch"));
    }
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
    let reply = match observation {
        Some(observation) if !matches!(observation, IndexOperationObservation::Unknown { .. }) => {
            SurfaceReply::IndexOperationStatus(observation)
        }
        _ => exchange(
            &mut transport,
            SurfaceCommand::IndexOperationStart {
                operation_key: claim.operation_key,
                package: claim.package.clone(),
                execution_intent,
            },
        )?,
    };
    let observation = match &reply {
        SurfaceReply::IndexOperationStarted(observation)
        | SurfaceReply::IndexOperationStatus(observation) => observation,
        _ => {
            return Err(ClientError::Protocol(
                "durable index reply changed shape".to_owned(),
            ));
        }
    };
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
            fixture.index(|command| {
                assert!(matches!(command, SurfaceCommand::IndexOperationStatus { operation_key } if operation_key == claim.operation_key));
                Ok(SurfaceReply::IndexOperationStatus(observation.clone()))
            }).expect("truthful retained observation");
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
}
