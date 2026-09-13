//! Process coverage for local daemon attach and retirement.
//!
//! Two facts about a local workspace are easy to let drift apart: ownership is
//! a kernel lock on an inode, and the endpoint is a socket named by a hash of a
//! path. Every case here crosses a real process boundary, because that is the
//! only place the drift shows up — an in-process check would share one address
//! space, one environment, and one spelling of the workspace.
//!
//! * A second surface naming the same directory a different way must attach to
//!   the live owner, not die on its lock.
//! * A killed owner leaves a socket behind; the next start must sweep it.
//! * A refusal must mean an owner is provably alive and unreachable, not that
//!   this process lost a race it could have joined.
//! * A daemon nobody is talking to must retire itself, and must not retire
//!   while somebody is.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use backend_locald::{ProcessConfig, ServiceStart};
use backend_runtime::WorkspacePaths;
use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How long a composed builtin owner may take to publish its endpoint.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How long a retiring daemon may take to exit after its last client leaves.
const RETIRE_DEADLINE: Duration = Duration::from_secs(30);

/// Idle window used by the retirement cases. Long enough to survive a loaded
/// machine's scheduling jitter, short enough that the case stays bounded.
const SHORT_IDLE: Duration = Duration::from_millis(400);

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
/// Composing the builtin owner is expensive and these cases each start real
/// daemons; serialize them so a loaded machine does not trip its own deadlines.
static LIFECYCLE_LEASE: Mutex<()> = Mutex::new(());

fn lifecycle_lease() -> MutexGuard<'static, ()> {
    LIFECYCLE_LEASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Allocates a fixture root directly under `/tmp`.
///
/// The per-session macOS temporary directory is too long to leave room for a
/// bindable `sun_path`, and these cases derive their endpoints rather than
/// naming them.
fn unique_root(label: &str) -> PathBuf {
    let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    let root = Path::new("/tmp").join(format!(
        "backend-lifecycle-{label}-{}-{serial}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("state")).expect("create workspace fixture");
    root
}

/// A spawned daemon that is always killed and reaped, whatever a case does.
#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
    endpoint: PathBuf,
}

impl ChildGuard {
    fn spawn(args: &[OsString], endpoint: &Path) -> Self {
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-locald"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn locald");
        Self {
            child: Some(child),
            endpoint: endpoint.to_path_buf(),
        }
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().expect("poll locald"))
            .is_none()
    }

    /// Sends `SIGKILL` and reaps, leaving the socket file on disk exactly as a
    /// crashed daemon would.
    fn kill_now(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.child = None;
    }

    /// Waits for a self-retiring daemon and returns its exit success.
    fn wait_for_exit(&mut self, deadline: Duration) -> bool {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            let Some(child) = self.child.as_mut() else {
                return true;
            };
            if let Some(status) = child.try_wait().expect("poll locald") {
                self.child = None;
                assert!(
                    status.success(),
                    "a self-retiring daemon must exit cleanly: {status}"
                );
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().expect("poll locald on cleanup").is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

/// Builds daemon arguments that deliberately omit `--endpoint`, so the child
/// derives it from the workspace exactly as a zero-configuration surface does.
fn derived_args(workspace: &Path, idle: Option<Duration>) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--workspace"),
        workspace.as_os_str().to_owned(),
        OsString::from("--profile"),
        OsString::from("builtin"),
        // The per-connection read deadline must outlast any idle window used
        // here, or a held-open client would be retired by its own worker.
        OsString::from("--timeout-ms"),
        OsString::from("120000"),
    ];
    if let Some(idle) = idle {
        args.push(OsString::from("--idle-timeout-ms"));
        args.push(OsString::from(idle.as_millis().to_string()));
    }
    args
}

fn endpoint_for(workspace: &Path) -> PathBuf {
    WorkspacePaths::discover(None, Some(workspace.to_path_buf()), None)
        .expect("derive workspace paths")
        .endpoint()
        .to_path_buf()
}

fn wait_for_socket(endpoint: &Path, child: &mut ChildGuard) {
    let end = Instant::now() + READY_DEADLINE;
    while Instant::now() < end {
        assert!(
            child.running(),
            "locald exited before it published {}",
            endpoint.display()
        );
        if std::fs::symlink_metadata(endpoint)
            .is_ok_and(|metadata| metadata.file_type().is_socket())
            && UnixStream::connect(endpoint).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for locald socket {}", endpoint.display());
}

/// Parses an in-process surface configuration for a workspace spelling.
fn surface_config(workspace: &str, endpoint: Option<&Path>) -> ProcessConfig {
    let mut args = vec![
        "--workspace".to_owned(),
        workspace.to_owned(),
        "--profile".to_owned(),
        "builtin".to_owned(),
        "--timeout-ms".to_owned(),
        "120000".to_owned(),
    ];
    if let Some(endpoint) = endpoint {
        args.push("--endpoint".to_owned());
        args.push(endpoint.to_string_lossy().into_owned());
    }
    ProcessConfig::parse(args).expect("parse surface configuration")
}

#[test]
fn a_second_surface_spelling_the_same_workspace_differently_attaches() {
    let _lease = lifecycle_lease();
    let root = unique_root("divergent-spelling");
    let workspace = root.join("state");

    // Spellings of one directory: a `.` segment, a doubled separator, a
    // trailing separator, and a `..` that walks back through a name that does
    // not exist.
    let mut spellings = vec![
        format!("{}/./state", root.display()),
        format!("{}//state", root.display()),
        format!("{}/state/", root.display()),
        format!("{}/state/nested/..", root.display()),
    ];
    // `/tmp` is a symlink to `/private/tmp` on macOS, so the absolute prefix
    // itself is a spelling there. It names nothing on other platforms.
    if Path::new("/private/tmp").is_dir() {
        spellings.push(format!("/private{}", workspace.display()));
    }

    let endpoint = endpoint_for(&workspace);
    for spelling in &spellings {
        assert_eq!(
            endpoint_for(Path::new(spelling)),
            endpoint,
            "spelling {spelling} derived a second endpoint for one workspace"
        );
    }

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);

    for spelling in &spellings {
        let start = backend_locald::start_or_attach(surface_config(spelling, None))
            .unwrap_or_else(|error| {
                panic!("surface spelled {spelling} failed instead of attaching: {error}")
            });
        match start {
            ServiceStart::Attached {
                endpoint: attached_endpoint,
            } => assert_eq!(
                attached_endpoint, endpoint,
                "spelling {spelling} attached to the wrong endpoint"
            ),
            ServiceStart::Owned(_) => panic!(
                "spelling {spelling} became a second owner of a live workspace"
            ),
        }
    }

    assert!(owner.running(), "the live owner must survive every attach");
    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

#[test]
fn a_killed_owner_leaves_a_socket_that_the_next_start_sweeps() {
    let _lease = lifecycle_lease();
    let root = unique_root("dead-owner");
    let workspace = root.join("state");
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);
    owner.kill_now();

    // SIGKILL cannot run a destructor, so the socket file outlives the owner
    // while the kernel releases both the listener and the workspace lock.
    assert!(
        std::fs::symlink_metadata(&endpoint)
            .is_ok_and(|metadata| metadata.file_type().is_socket()),
        "a killed owner must leave its socket behind for this case to mean anything"
    );
    assert!(
        UnixStream::connect(&endpoint).is_err(),
        "a killed owner must not still answer"
    );

    let start = backend_locald::start_or_attach(surface_config(
        &workspace.to_string_lossy(),
        None,
    ))
    .expect("recover a workspace whose owner was killed");
    match start {
        ServiceStart::Owned(service) => {
            assert_eq!(service.endpoint(), endpoint);
            assert!(
                UnixStream::connect(&endpoint).is_ok(),
                "the recovered owner must answer on the swept endpoint"
            );
            drop(service);
        }
        ServiceStart::Attached { .. } => {
            panic!("a dead owner was mistaken for a live one")
        }
    }
    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

#[test]
fn a_busy_refusal_requires_a_live_owner_at_an_unreachable_endpoint() {
    let _lease = lifecycle_lease();
    let root = unique_root("provably-busy");
    let workspace = root.join("state");
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);

    // Same workspace, an endpoint the live owner is not listening on. The lock
    // is genuinely held and no amount of waiting will make this path answer,
    // so this is the one shape that must still be reported as a failure.
    let unreachable = root.join("unreachable.sock");
    let refusal = backend_locald::start_or_attach(surface_config(
        &workspace.to_string_lossy(),
        Some(&unreachable),
    ));
    let Err(error) = refusal else {
        panic!("a surface pointed at an unreachable endpoint became an owner")
    };
    let message = error.to_string();
    assert!(
        message.contains("AlreadyOwned") || message.contains("already"),
        "a busy workspace must say so: {message}"
    );
    assert!(
        !unreachable.exists(),
        "a refused start must not leave a socket behind"
    );
    assert!(
        owner.running(),
        "the live owner must be untouched by the refusal"
    );

    // The same surface at the derived endpoint attaches instead of failing,
    // which is what makes the refusal above specific rather than generic.
    let attached =
        backend_locald::start_or_attach(surface_config(&workspace.to_string_lossy(), None))
            .expect("attach at the derived endpoint");
    assert!(
        matches!(attached, ServiceStart::Attached { .. }),
        "a reachable live owner must be attached to, never refused"
    );

    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

#[test]
fn an_abandoned_daemon_retires_itself_but_never_under_a_live_client() {
    let _lease = lifecycle_lease();
    let root = unique_root("idle-retire");
    let workspace = root.join("state");
    let endpoint = endpoint_for(&workspace);

    let mut daemon = ChildGuard::spawn(&derived_args(&workspace, Some(SHORT_IDLE)), &endpoint);
    wait_for_socket(&endpoint, &mut daemon);

    let held = UnixStream::connect(&endpoint).expect("hold one client connection open");
    let hold_until = Instant::now() + SHORT_IDLE * 5;
    while Instant::now() < hold_until {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        daemon.running(),
        "a daemon retired while a client was connected"
    );
    assert!(
        UnixStream::connect(&endpoint).is_ok(),
        "a daemon with a live client must keep accepting"
    );

    drop(held);
    assert!(
        daemon.wait_for_exit(RETIRE_DEADLINE),
        "an abandoned daemon never retired; it would have leaked for the life of the machine"
    );
    assert!(
        !endpoint.exists(),
        "a retiring daemon must unlink {}",
        endpoint.display()
    );

    drop(daemon);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}
