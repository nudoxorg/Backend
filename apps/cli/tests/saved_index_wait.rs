//! Executable Add caller lifetime against real Unix framing and scripted receipts.
//! No compiler/publication is simulated: the owner remains Accepted throughout.
#![cfg(unix)]

use backend_library::{
    Command, CommandDto, CommandReply, CompileExecutionIntent, IndexOperationKey,
    IndexOperationObservation, IndexOperationState, IndexOperationStatus, PackageReference,
    ReplyDto, SurfaceCommand, SurfaceReply,
};
use backend_local_service::service::{LocaldService, OwnerService};
use backend_local_service::{EngineRequest, EngineStatus, FrameLimits, ProtocolError};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ReceiptOwner {
    library: backend_library::Library,
    package: PackageReference,
    key: Option<IndexOperationKey>,
    starts: usize,
    reads: usize,
}

impl OwnerService for ReceiptOwner {
    fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        let request = CommandDto::decode_for_owner(body, self.library.cursor())
            .expect("actual owner request admission");
        let (key, started) = match &request.command {
            Command::Surface(SurfaceCommand::IndexOperationStart {
                cargo_target: None,
                operation_key,
                package,
                execution_intent,
            }) => {
                assert_eq!(package, &self.package);
                assert_eq!(*execution_intent, CompileExecutionIntent::Interactive);
                assert!(
                    self.key.replace(*operation_key).is_none(),
                    "only one Add send"
                );
                self.starts += 1;
                (*operation_key, true)
            }
            Command::Surface(SurfaceCommand::IndexOperationStatus { operation_key }) => {
                assert_eq!(Some(*operation_key), self.key, "read only the original key");
                self.reads += 1;
                (*operation_key, false)
            }
            Command::Health | Command::Revision => {
                return Ok(serde_json::to_vec(&self.library.execute_dto(request))
                    .expect("ordinary read response"));
            }
            command => panic!("unexpected command {command:?}"),
        };
        let observed = IndexOperationObservation::Known(IndexOperationStatus::new(
            key,
            self.package.clone(),
            CompileExecutionIntent::Interactive,
            IndexOperationState::Accepted,
        ));
        let reply = if started {
            SurfaceReply::IndexOperationStarted(observed)
        } else {
            SurfaceReply::IndexOperationStatus(observed)
        };
        Ok(serde_json::to_vec(&ReplyDto::new(
            request.request_id,
            CommandReply::Surface(reply),
        ))
        .expect("typed original receipt"))
    }
    fn engine(&mut self, _: u64, _: EngineRequest) -> Result<EngineStatus, ProtocolError> {
        panic!("receipt reads never issue engine mutations")
    }
    fn serve_one(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
}

#[test]
fn executable_add_wait_reobserves_same_key_across_retirements_until_original_deadline() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::path::PathBuf::from(format!("/tmp/nwi-{}-{nonce:x}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("private root");
    let project = root.join("project");
    std::fs::create_dir(&project).expect("actual Add operand");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(project.clone()),
        Some(root.join("state")),
        Some(root.join("o.sock")),
    )
    .expect("private configured workspace");
    paths.initialize().expect("actual workspace authority");
    let listener =
        std::os::unix::net::UnixListener::bind(paths.endpoint()).expect("private endpoint");
    std::fs::set_permissions(paths.endpoint(), std::fs::Permissions::from_mode(0o600))
        .expect("private socket");
    listener.set_nonblocking(true).expect("bounded accept");
    let package = PackageReference::parse(paths.project().to_str().expect("UTF8"))
        .expect("canonical real project");
    let expected_package = package.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        // A reduced fixture cap makes executable reconnect/deadline coverage short.
        // The separate socket law exercises three retirements at the real default 256.
        let mut service = LocaldService::new(
            ReceiptOwner {
                library: backend_library::Library::new(),
                package,
                key: None,
                starts: 0,
                reads: 0,
            },
            FrameLimits {
                max_frames_per_connection: 1,
                ..FrameLimits::default()
            },
        )
        .expect("production framing service");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut retired = 0;
        while !server_stop.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "CLI did not finish its original wait"
            );
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .expect("bounded read");
                    let frames = service
                        .serve_stream(&mut stream)
                        .expect("real connection service");
                    // Bootstrap's empty reachability probe is not a frame retirement.
                    if frames == 1 {
                        retired += 1;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("private accept: {error}"),
            }
        }
        (
            service.owner().key,
            service.owner().starts,
            service.owner().reads,
            retired,
        )
    });
    let before = Instant::now();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_backend-cli"))
        .current_dir(&project)
        .args([
            "--format",
            "json",
            "--project",
            paths.project().to_str().expect("UTF8"),
            "--workspace",
            paths.data().to_str().expect("UTF8"),
            "--endpoint",
            paths.endpoint().to_str().expect("UTF8"),
            "add",
            ".",
            "--wait",
            "--wait-timeout",
            "3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("actual CLI executable");
    let mut expired = false;
    while child.try_wait().expect("observe owned CLI child").is_none() {
        if before.elapsed() >= Duration::from_secs(10) {
            expired = true;
            child
                .kill()
                .expect("stop only this owned over-budget CLI child");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("drain owned CLI output");
    stop.store(true, Ordering::Release);
    let (key, starts, reads, retired) = server.join().expect("exact fixture thread joined");
    assert!(!expired, "CLI renewed the original caller wait deadline");
    assert_eq!(
        output.status.code(),
        Some(75),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        before.elapsed() >= Duration::from_millis(2800),
        "not the second retirement cutoff"
    );
    assert!(
        before.elapsed() < Duration::from_secs(12),
        "reconnect must not renew the 3s wait"
    );
    assert_eq!(starts, 1);
    assert!(
        reads >= 3 && retired >= 4,
        "must outlive two connection retirements"
    );
    let key = key.expect("one saved original key");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("machine response");
    let observed: IndexOperationObservation =
        serde_json::from_value(value["index_operation"].clone())
            .expect("last admitted original receipt");
    assert_eq!(
        observed,
        IndexOperationObservation::Known(IndexOperationStatus::new(
            key,
            expected_package,
            CompileExecutionIntent::Interactive,
            IndexOperationState::Accepted,
        ))
    );
    let bytes = String::from_utf8(output.stdout).expect("UTF8 output");
    assert!(bytes.contains(&format!("index_progress --operation-key {key}")));
    let note = value["note"].as_str().expect("specific pending-wait guidance");
    assert!(note.contains("The CLI wait deadline expired"));
    assert!(note.contains("the last admitted owner status was accepted"));
    assert!(note.contains("did not cancel owner work"));
    assert!(note.contains("Do not resubmit `add`"));
    assert!(value["fault"].is_null(), "caller deadline is not an endpoint failure");
    assert_eq!(value["index_poll"]["call"]["name"], "backend.index_progress");
    assert_eq!(
        value["index_poll"]["call"]["arguments"]["operation_key"],
        key.to_hex()
    );
    let claims: Vec<_> = std::fs::read_dir(paths.data().join("client-index-operations-v1"))
        .expect("retained journal")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(claims.len(), 1);
    let claim: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&claims[0]).expect("claim")).expect("claim JSON");
    assert_eq!(
        claim["operation_key"],
        serde_json::to_value(key).expect("saved key")
    );
    std::fs::remove_dir_all(root).expect("remove exact joined fixture");
}
