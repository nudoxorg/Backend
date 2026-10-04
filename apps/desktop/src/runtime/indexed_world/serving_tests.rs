//! The real graph request boundary with a Cargo-free local owner.

#![allow(clippy::expect_used)]

use super::{Cancellation, Key, RootedGraphSession, VersionedRoot};
use crate::host::lease::OwnerConfiguration;
use crate::host::registry::{self, CargoCache, OwnerContext, ServingComposition};
use crate::model::ServiceMode;
use crate::runtime::owner::{OwnerGate, OwnerState};
use backend_client::Session;
use backend_local_service::{
    ClosedLocalHostEnvironmentSnapshot, EmbeddedLocalService, LocalHostVariable, ProcessConfig,
};
use backend_runtime::WorkspacePaths;
use gpui::{AppContext as _, Entity, TestAppContext};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

const CHILD_ENV: &str = "NUDOX_TEST_NO_CARGO_GRAPH_CHILD";
const TEST_NAME: &str = "runtime::indexed_world::serving_tests::cargo_free_graph_reads_the_owner_and_rejects_a_replacement";
const CHILD_RECEIPT: &str = "NO-CARGO-GRAPH: actual owner request and replacement admitted";
const EMPTY_GRAPH: &str = "the current index has no packages to show in the graph";

#[test]
fn cargo_free_graph_reads_the_owner_and_rejects_a_replacement() {
    // Registry installation is process-global. Isolate the real production
    // handoff from unrelated fixtures instead of introducing another lock or slot.
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .output()
            .expect("isolated graph fixture process");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "graph fixture failed: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains(CHILD_RECEIPT),
            "the exact fixture ran: {stdout}\n{stderr}"
        );
        return;
    }

    exercise_serving_graph(OwnerFixture::start());
    println!("{CHILD_RECEIPT}");
}

struct OwnerFixture {
    service: EmbeddedLocalService,
    configuration: OwnerConfiguration,
    paths: WorkspacePaths,
    home: PathBuf,
    scratch: PathBuf,
}

impl OwnerFixture {
    fn start() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let scratch = crate::host::scratch_base()
            .join(format!("nx-graph-free-{}-{nonce}", std::process::id()));
        crate::host::private_dir(&scratch).expect("private graph workspace");
        let home = scratch.join("home");
        let project = scratch.join("project");
        crate::host::private_dir(&home).expect("fresh user home");
        crate::host::private_dir(&project).expect("project directory");
        crate::host::private_dir(&scratch.join("data")).expect("private owner state");
        let paths = WorkspacePaths::discover(
            Some(project),
            Some(scratch.join("data")),
            Some(scratch.with_extension("sock")),
        )
        .expect("local owner paths");
        paths.initialize().expect("authenticated owner paths");
        let mut config = offline_config(&paths);
        let compiler_environment = ClosedLocalHostEnvironmentSnapshot::from_paths([(
            LocalHostVariable::Home,
            home.clone(),
        )])
        .expect("closed non-Cargo environment");
        assert!(
            CargoCache::from_snapshot(&compiler_environment, scratch.join("unpacked")).is_none()
        );
        config.compiler_environment = Some(compiler_environment.clone());
        let configuration = OwnerConfiguration {
            compiler_environment,
            runtime_policy: config
                .runtime_policy()
                .expect("effective offline owner policy"),
        };
        let service =
            EmbeddedLocalService::start(config).expect("actual Cargo-free embedded owner");
        Self {
            service,
            configuration,
            paths,
            home,
            scratch,
        }
    }
}

fn offline_config(paths: &WorkspacePaths) -> ProcessConfig {
    ProcessConfig::parse(
        [
            "--endpoint",
            paths.endpoint().to_str().expect("endpoint UTF-8"),
            "--workspace",
            paths.data().to_str().expect("workspace UTF-8"),
            "--authority-secret-file",
            paths
                .authority_secret()
                .to_str()
                .expect("credential path UTF-8"),
            "--profile",
            "builtin",
            "--registry-offline",
            "--registry-discovery-offline",
            "--advisory-offline",
            "--forge-offline",
        ]
        .map(ToOwned::to_owned),
    )
    .expect("offline embedded owner configuration")
}

struct GraphFixture {
    cx: TestAppContext,
    requester: Entity<()>,
    root: VersionedRoot,
    cancellation: Cancellation,
}

impl GraphFixture {
    fn new(root: VersionedRoot) -> Self {
        let mut cx = TestAppContext::single();
        let requester = cx.new(|_| ());
        Self {
            cx,
            requester,
            root,
            cancellation: Cancellation::default(),
        }
    }

    fn key(&mut self) -> Option<Key> {
        self.requester
            .update(&mut self.cx, |_, cx| super::key(self.root, None, cx))
    }

    fn failure(&self, key: &Key) -> Arc<str> {
        self.cx
            .foreground_executor
            .block_test(super::read(key, &self.cancellation))
            .err()
            .expect("a fresh empty owner or withdrawn generation has a precise graph diagnostic")
    }
}

fn exercise_serving_graph(fixture: OwnerFixture) {
    let OwnerFixture {
        service,
        configuration,
        paths,
        home,
        scratch,
    } = fixture;
    let mut proof = Session::connect(paths.endpoint()).expect("authenticated owner session");
    let revision = proof.revision().expect("actual producer root");
    let root = VersionedRoot::from_revision(1, revision.cursor(), 0);
    let gate = OwnerGate::starting();
    let lifetime = Arc::new(());
    let lease = registry::publish_serving(
        paths.endpoint(),
        paths.data(),
        None,
        Some(OwnerContext::new(
            configuration,
            Arc::downgrade(&lifetime),
            gate.clone(),
        )),
    );
    let serving = registry::serving_composed().expect("serving owner without registry source");
    // Neither a Cargo capability nor pending MCP configuration is fabricated.
    assert!(registry::composed().is_none());
    assert!(serving.owner_configuration().is_none());
    gate.publish(OwnerState::Ready {
        key: root,
        mode: ServiceMode::Embedded,
    });
    assert!(serving.owner_configuration().is_some());

    let mut graph = GraphFixture::new(root);
    let key = graph
        .key()
        .expect("the production graph key exists without a Cargo cache");
    let mut session = RootedGraphSession::connect(&serving, key.authority)
        .expect("graph authenticates the actual owner");
    // This diagnostic is produced only after the actual PackagePage reply
    // passes bounded reply/terminal admission and contains no package rows.
    assert_eq!(graph.failure(&key).as_ref(), EMPTY_GRAPH);
    assert!(
        !home.join(".cargo").exists(),
        "graph requests cannot create a Cargo cache"
    );

    verify_replacement(&mut graph, &serving, &key, &mut session, &paths, lease);
    gate.close();
    drop(lifetime);
    drop(session);
    drop(proof);
    service.close().expect("clean owner shutdown");
    std::fs::remove_dir_all(scratch).expect("private fixture cleanup");
}

fn verify_replacement(
    graph: &mut GraphFixture,
    serving: &ServingComposition,
    key: &Key,
    session: &mut RootedGraphSession,
    paths: &WorkspacePaths,
    lease: registry::CompositionLease<'_>,
) {
    let replacement_lease = registry::publish_serving(paths.endpoint(), paths.data(), None, None);
    let replacement = registry::serving_composed().expect("replacement serving generation");
    assert_eq!(replacement.endpoint, serving.endpoint);
    assert_ne!(replacement.generation, serving.generation);
    assert!(serving.owner_configuration().is_none());
    let replaced_key = graph.key().expect("replacement graph key");
    assert_ne!(
        key, &replaced_key,
        "same producer root and endpoint cannot share a replaced owner key"
    );
    assert!(
        session.confirm("after replacement").is_err(),
        "old graph session cannot contribute its same-root facts"
    );
    assert_eq!(
        graph.failure(key).as_ref(),
        "the local service connection changed before the graph was read"
    );
    drop(lease);
    assert_eq!(
        registry::serving_composed()
            .expect("new host survives old drop")
            .generation,
        replacement.generation
    );
    assert_eq!(
        graph.failure(&replaced_key).as_ref(),
        EMPTY_GRAPH,
        "attached/unknown compiler configuration does not block owner-indexed graph facts"
    );
    // An unknown owner's graph connection cannot authorize an MCP export.
    assert!(replacement.owner_configuration().is_none());
    drop(replacement_lease);
    assert!(
        graph.key().is_none(),
        "withdrawal removes the graph connection"
    );
    assert!(session.confirm("after withdrawal").is_err());
}
