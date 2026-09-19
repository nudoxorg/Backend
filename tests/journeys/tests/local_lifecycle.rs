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
use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// How long a composed builtin owner may take to publish its endpoint.
const READY_DEADLINE: Duration = Duration::from_secs(75);

/// How long a retiring daemon may take to exit after its last client leaves.
const RETIRE_DEADLINE: Duration = Duration::from_secs(30);

/// Idle window used by the retirement cases.
///
/// The window has to clear the owner's own maintenance cadence, not just the
/// machine's scheduling jitter. `UnixListenerService::run` treats owner
/// progress as progress, so a window shorter than the interval between the
/// composed builtin owner's own queue ticks is reset before it can ever
/// elapse and the daemon never retires. Four hundred milliseconds was inside
/// that cadence and made this case fail against a daemon that retires
/// correctly at every realistic window, including the ten-minute default.
const SHORT_IDLE: Duration = Duration::from_secs(3);

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
    let hold_until = Instant::now() + SHORT_IDLE * 3;
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

/// How long one CLI invocation inside the durability case may take.
const CLI_DEADLINE: Duration = Duration::from_secs(150);

/// Source files written by the durability fixture.
///
/// The defect this case exists for is invisible below a threshold: a workspace
/// whose relation tree is a single node has no child edges to lose, so it
/// reopens whatever rule the reader applies. The fixture therefore has to be
/// large enough to make the tree grow an internal node, and the case asserts
/// that it did rather than trusting this constant.
const DURABLE_FILES: usize = 48;

/// Declarations per fixture file.
const DURABLE_DECLARATIONS_PER_FILE: usize = 40;

/// Runs one journey CLI invocation against an already-running owner.
fn run_cli(project: &Path, workspace: &Path, args: &[&str]) -> String {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--project")
        .arg(project)
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn journey cli");
    let deadline = Instant::now() + CLI_DEADLINE;
    let mut child = child;
    loop {
        match child.try_wait().expect("poll journey cli") {
            Some(_) => break,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("journey cli {args:?} exceeded its bounded deadline");
            }
        }
    }
    let output = child.wait_with_output().expect("collect journey cli output");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Writes a project whose source is dense enough to outgrow one canonical node.
fn write_dense_project(root: &Path) {
    let source = root.join("src");
    std::fs::create_dir_all(&source).expect("create fixture source directory");
    for file in 0..DURABLE_FILES {
        let mut text = String::new();
        for declaration in 0..DURABLE_DECLARATIONS_PER_FILE {
            let _ = write!(
                text,
                "/// Durable fixture declaration {file}/{declaration}.\n\
                 pub fn durable_{file}_{declaration}(argument: u64) -> u64 {{\n\
                 \x20   let scaled = argument.wrapping_mul({});\n\
                 \x20   let shifted = scaled.rotate_left({});\n\
                 \x20   shifted ^ {}\n\
                 }}\n\n",
                declaration + 3,
                u32::try_from(declaration % 63).unwrap_or(0),
                file + declaration,
            );
        }
        std::fs::write(source.join(format!("module_{file}.rs")), text)
            .expect("write fixture source file");
    }
    std::fs::write(
        source.join("lib.rs"),
        "//! Durable reopen fixture root module.\npub fn durable_marker() -> &'static str {\n    \"durable-marker\"\n}\n",
    )
    .expect("write fixture root module");
}

/// Returns how many persisted nodes each canonical relation retains.
///
/// The store names a node file `rel-<domain>-<type>-<version>-<hash>.ref`, so
/// grouping by the first four fields counts one relation's tree without
/// knowing which relation it is.
fn relation_node_counts(workspace: &Path) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(workspace.join("objects").join("nodes")) else {
        return counts;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relation = name.split('-').take(4).collect::<Vec<_>>().join("-");
        *counts.entry(relation).or_insert(0_usize) += 1;
    }
    counts
}

/// Polls health until the owner publishes rows, and returns the reply text.
fn wait_for_rows(project: &Path, workspace: &Path) -> String {
    let deadline = Instant::now() + CLI_DEADLINE;
    loop {
        let text = run_cli(project, workspace, &["--json", "health"]);
        if let Some(rows) = json_rows(&text)
            && rows > 0
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "the owner never published a row: {text}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

/// Reads the `"rows": N` field out of a `--json health` reply.
fn json_rows(text: &str) -> Option<u64> {
    let tail = text.split("\"rows\":").nth(1)?;
    let digits = tail
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

#[test]
fn a_workspace_whose_relation_tree_outgrew_one_node_reopens_with_its_rows() {
    let _lease = lifecycle_lease();
    let root = unique_root("durable-reopen");
    let workspace = root.join("state");
    let project = root.join("project");
    write_dense_project(&project);
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);

    let accepted = run_cli(&project, &workspace, &["index"]);
    assert!(
        accepted.contains("accepted"),
        "the owner refused a dense project instead of indexing it: {accepted}"
    );
    let before = wait_for_rows(&project, &workspace);
    let rows = json_rows(&before).expect("published row count");
    let found = run_cli(&project, &workspace, &["search", "durable_marker"]);
    assert!(
        found.contains("durable_marker"),
        "a freshly indexed project did not answer for its own declaration: {found}"
    );

    // Without this the case cannot see the defect it exists for: a relation
    // whose whole tree is one node has no child edges for a reader to demand.
    let counts = relation_node_counts(&workspace);
    let widest = counts.values().copied().max().unwrap_or(0);
    assert!(
        widest >= 3,
        "the fixture never grew an internal relation node, so a reopen that \
         only fails for multi-node trees would pass vacuously: {counts:?}"
    );

    // A killed owner is the harsh case: the index reply already returned, so
    // the transition is durable and no amount of lost process state may cost
    // a committed row.
    owner.kill_now();
    drop(owner);

    let mut reopened = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut reopened);
    let after = run_cli(&project, &workspace, &["--json", "health"]);
    assert_eq!(
        json_rows(&after),
        Some(rows),
        "a reopened workspace published a different row count: {after}"
    );
    let found_again = run_cli(&project, &workspace, &["search", "durable_marker"]);
    assert!(
        found_again.contains("durable_marker"),
        "a reopened workspace lost the declaration it had already published: {found_again}"
    );

    drop(reopened);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

/// Reads one string field out of a `--json` reply.
fn json_string(text: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let tail = text.split(&needle).nth(1)?;
    let tail = tail.trim_start().strip_prefix('"')?;
    tail.split('"').next().map(str::to_owned)
}

/// Writes a small project that indexes in well under a second.
fn write_small_project(root: &Path) {
    let source = root.join("src");
    std::fs::create_dir_all(&source).expect("create fixture source directory");
    std::fs::write(
        source.join("lib.rs"),
        "//! Idempotence fixture.\npub fn stable_marker() -> u8 {\n    7\n}\n",
    )
    .expect("write fixture module");
    std::fs::write(
        source.join("other.rs"),
        "pub fn second_marker() -> u8 {\n    9\n}\n",
    )
    .expect("write second fixture module");
}

#[test]
fn reindexing_an_unchanged_project_publishes_the_same_revision() {
    let _lease = lifecycle_lease();
    let root = unique_root("reindex-noop");
    let workspace = root.join("state");
    let project = root.join("project");
    write_small_project(&project);
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);

    run_cli(&project, &workspace, &["index"]);
    let first = wait_for_rows(&project, &workspace);
    let revision = json_string(&first, "revision").expect("first revision");
    let rows = json_rows(&first).expect("first row count");

    // Nothing about the project changed, so the content-versioned scan must
    // find the same frontier and publish nothing. A revision that moves here
    // would invalidate every surface's cursor on a command that did no work.
    for attempt in 0..3 {
        run_cli(&project, &workspace, &["index"]);
        let repeat = run_cli(&project, &workspace, &["--json", "health"]);
        assert_eq!(
            json_string(&repeat, "revision").as_deref(),
            Some(revision.as_str()),
            "re-index {attempt} of an unchanged project moved the revision: {repeat}"
        );
        assert_eq!(
            json_rows(&repeat),
            Some(rows),
            "re-index {attempt} of an unchanged project changed the row count: {repeat}"
        );
    }

    // A project that really did change must still move, or the case above
    // would pass for an owner that ignores `index` entirely.
    std::fs::write(
        project.join("src").join("third.rs"),
        "pub fn third_marker() -> u8 {\n    11\n}\n",
    )
    .expect("write third fixture module");
    run_cli(&project, &workspace, &["index"]);
    let changed = run_cli(&project, &workspace, &["--json", "health"]);
    assert_ne!(
        json_string(&changed, "revision").as_deref(),
        Some(revision.as_str()),
        "a project that gained a file published the same revision: {changed}"
    );

    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

#[test]
fn two_surfaces_indexing_one_project_converge_on_one_revision() {
    let _lease = lifecycle_lease();
    let root = unique_root("concurrent-index");
    let workspace = root.join("state");
    let project = root.join("project");
    write_small_project(&project);
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);

    // Two independent surface processes ask for the same project at the same
    // time. The owner serializes them, so the second sees the frontier the
    // first published and has nothing left to do.
    let workers = (0..2)
        .map(|_| {
            let project = project.clone();
            let workspace = workspace.clone();
            thread::spawn(move || run_cli(&project, &workspace, &["index"]))
        })
        .collect::<Vec<_>>();
    let replies = workers
        .into_iter()
        .map(|worker| worker.join().expect("join concurrent index surface"))
        .collect::<Vec<_>>();
    for reply in &replies {
        assert!(
            reply.contains("accepted"),
            "a concurrent surface was refused instead of converging: {reply}"
        );
    }

    let after = wait_for_rows(&project, &workspace);
    let revision = json_string(&after, "revision").expect("converged revision");
    let rows = json_rows(&after).expect("converged row count");

    // One project, indexed twice, is still one project. A second intent that
    // re-published the same files would show up as duplicated rows.
    let projects = run_cli(&project, &workspace, &["--json", "packages"]);
    assert_eq!(
        projects.matches("\"readiness\":").count(),
        1,
        "concurrent indexing produced more than one project: {projects}"
    );

    // Asking once more changes nothing, which is what makes the convergence
    // above a fixed point rather than a race that happened to settle.
    run_cli(&project, &workspace, &["index"]);
    let settled = run_cli(&project, &workspace, &["--json", "health"]);
    assert_eq!(
        json_string(&settled, "revision").as_deref(),
        Some(revision.as_str()),
        "the converged revision moved on a later identical request: {settled}"
    );
    assert_eq!(json_rows(&settled), Some(rows));

    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}

/// Asserts the exact counts the two-module, one-binary-file fixture must yield.
fn assert_fixture_progress(progress: &backend_library::IngestProgress) {
    assert_eq!(
        progress.files_discovered(),
        3,
        "the owner did not report every file it walked: {progress:?}"
    );
    assert_eq!(progress.files_indexed(), 2);
    assert_eq!(progress.files_unavailable(), 1);
    assert_eq!(
        progress.faults(),
        [backend_library::FaultRows::new(
            backend_library::SourceUnavailableReason::NotText,
            1,
        )]
        .as_slice(),
        "the unreadable file lost its typed reason on the wire"
    );
    // Two readable modules, each publishing its own module declaration and its
    // one function; the unreadable file publishes nothing but still belongs to
    // the language that claimed it.
    assert_eq!(
        progress.languages(),
        [backend_library::LanguageRows::new(
            backend_library::SourceLanguage::Rust,
            3,
            4,
        )]
        .as_slice(),
        "per-language counts did not survive the process boundary"
    );
    let attributed = progress
        .languages()
        .iter()
        .map(|row| row.declarations())
        .sum::<u64>();
    assert_eq!(
        progress.declarations(),
        attributed,
        "the declaration total and the per-language rows disagree, so one of \
         them is describing a revision the other is not"
    );
    let claimed = progress.languages().iter().map(|row| row.files()).sum::<u64>();
    assert_eq!(
        claimed,
        progress.files_discovered(),
        "a discovered file was attributed to no language"
    );
}

#[test]
fn the_owner_publishes_typed_ingest_counts_over_the_wire() {
    let _lease = lifecycle_lease();
    let root = unique_root("ingest-progress");
    let workspace = root.join("state");
    let project = root.join("project");
    write_small_project(&project);
    // One file the owner cannot read as text. It must stay in the denominator
    // with its typed reason instead of vanishing, which is the whole reason
    // these counts exist rather than a single "files indexed" number.
    std::fs::write(
        project.join("src").join("blob.rs"),
        [0xff_u8, 0xfe, 0x00, 0x01],
    )
    .expect("write a file that is not text");
    let endpoint = endpoint_for(&workspace);

    let mut owner = ChildGuard::spawn(&derived_args(&workspace, None), &endpoint);
    wait_for_socket(&endpoint, &mut owner);
    run_cli(&project, &workspace, &["index"]);
    wait_for_rows(&project, &workspace);

    let mut session = backend_client::Session::connect(&endpoint).expect("connect to the owner");
    let report = session.health().expect("read admitted health");
    assert_fixture_progress(report.progress());

    drop(session);
    drop(owner);
    std::fs::remove_dir_all(&root).expect("remove lifecycle fixture");
}
