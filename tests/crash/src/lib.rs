//! Production filesystem/process fault matrix.
//!
//! These tests intentionally operate through the real checked workspace
//! owner and the real hash-chain journal. A child process arms one seam in
//! production code and is then terminated; the parent reopens the directory
//! through recovery and verifies an old or new checked publication.
#![deny(unsafe_code)]
#![cfg(test)]
// This harness intentionally panics in child processes and assertion helpers:
// the parent observes those exits as part of the crash protocol.
#![cfg_attr(test, allow(clippy::panic))]

use backend_engine::effects::{EffectCodec, EffectSnapshotLimits};
use backend_engine::{
    AuthorityScopeClaim, Boundary, ClosureManifest, Commit, CommitProvenance, CoverageWitness,
    EffectCoordinator, EffectError, EffectJournalPersistence, EffectSink, EffectSpec, Faults,
    GcLimits, GcRoot, HashChainJournal, JournalCodec, JournalDomain, JournalError, JournalLimits,
    ObjectKey, ObjectVersion, PreparedTransition, ProducerObservationClaims,
    ProducerObservationVerifier, Schema, SinkApply, SinkObservation, TransactionId,
    TransactionSchema, TypedObject, UntrustedProducerObservation, WorkspaceClosure, WorkspaceDelta,
    WorkspaceError, WorkspaceHead, WorkspaceManifest, WorkspaceModel, WorkspaceOwner,
    WorkspaceSnapshot, admit_complete_scope, admit_producer_observation, effect_key,
};
use backend_store::{
    FileStore, LayoutId, OrderedMap, PublicationAuthorityError, StoreError, StoredValue,
};
use backend_version::{ObjectClosure, commit_checked};

mod dispatch_durability;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AUTHORITY_VALUE: u64 = 1;

#[derive(Debug)]
struct FaultAuthority;

impl Schema for FaultAuthority {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 1;
    type Value = u64;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

#[derive(Debug)]
struct FaultIntentSchema;

impl Schema for FaultIntentSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 2;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Fixture-only producer observation for the crash model. Product authority
/// claims must pass the engine's sealed admission path.
fn crash_scope_equality_fixture() -> Result<CoverageWitness, PlanningError> {
    let authority = ObjectVersion::<FaultAuthority>::from_value(&AUTHORITY_VALUE);
    let declared = AuthorityScopeClaim::from_object_version(authority);
    let observation = UntrustedProducerObservation::new(
        [0x31; 32],
        declared.scope_root(),
        [0x32; 32],
        b"backend-crash/fixture-authority/v1".to_vec(),
    );
    let verifier = CrashCoverageVerifier(observation.clone());
    let admitted = admit_producer_observation(observation, &verifier)
        .map_err(|error| PlanningError(error.to_string()))?;
    admit_complete_scope(declared, admitted)
        .map(CoverageWitness::Complete)
        .map_err(|error| PlanningError(error.to_string()))
}

#[derive(Clone, Debug)]
struct CrashCoverageVerifier(UntrustedProducerObservation);

impl ProducerObservationVerifier for CrashCoverageVerifier {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation != &self.0 {
            return Err("crash fixture producer observation mismatch");
        }
        Ok(ProducerObservationClaims::new(
            self.0.producer_identity(),
            self.0.scope_root(),
            self.0.context(),
            *blake3::hash(self.0.evidence()).as_bytes(),
        ))
    }
}

fn checked_manifest() -> Result<WorkspaceManifest, PlanningError> {
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        ObjectVersion::<FaultAuthority>::from_value(&AUTHORITY_VALUE),
        crash_scope_equality_fixture()?,
    )
    .map_err(|error| PlanningError(error.to_string()))
}

fn closure_for(
    manifest: &WorkspaceManifest,
    transaction: Option<TransactionId>,
) -> Result<WorkspaceClosure, PlanningError> {
    let authority_key = ObjectKey::<FaultAuthority>::from_value(&AUTHORITY_VALUE);
    let mut objects = vec![TypedObject::from_value(&authority_key, &AUTHORITY_VALUE)];
    if let Some(transaction) = transaction {
        let bytes = transaction.as_bytes();
        let key = ObjectKey::<TransactionSchema>::from_value(&bytes[..]);
        objects.push(TypedObject::from_value(&key, &bytes[..]));
    }
    objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let objects =
        ClosureManifest::new(objects).map_err(|error| PlanningError(format!("{error:?}")))?;
    if let Some(transaction) = transaction {
        let delta = WorkspaceDelta::new(manifest, manifest, Vec::new())
            .map_err(|error| PlanningError(error.to_string()))?
            .into_checked();
        let provenance = CommitProvenance::new(
            ObjectClosure::from_version(ObjectVersion::<FaultAuthority>::from_value(
                &AUTHORITY_VALUE,
            )),
            ObjectClosure::from_version(transaction.version()),
            b"backend.engine.fault-genesis.v1".to_vec(),
        );
        let commit = commit_checked(manifest, Vec::new(), provenance)
            .map_err(|error| PlanningError(error.to_string()))?;
        let checked_commit = backend_version::commit_capability(
            manifest,
            commit.parents().to_vec(),
            CommitProvenance::new(
                commit.authority(),
                commit.transaction(),
                commit.provenance().to_vec(),
            ),
        )
        .map_err(|error| PlanningError(format!("{error:?}")))?;
        return WorkspaceClosure::from_checked_transition(
            manifest,
            &delta,
            Some(&checked_commit),
            objects,
        )
        .map_err(|error| PlanningError(format!("{error:?}")));
    }
    WorkspaceClosure::from_checked_manifest(manifest, objects)
        .map_err(|error| PlanningError(format!("{error:?}")))
}

fn genesis() -> Result<WorkspaceHead, PlanningError> {
    let manifest = checked_manifest()?;
    // `WorkspaceHead::genesis` owns the bootstrap commit and transition
    // identities. Seed it with the checked manifest closure only so the
    // closure cannot carry a transaction object from a separately derived
    // provenance record that differs from the head's private bootstrap.
    let closure = closure_for(&manifest, None)?;
    WorkspaceHead::genesis(manifest, closure).map_err(|error| PlanningError(error.to_string()))
}

#[derive(Clone, Copy, Debug, Default)]
struct FaultModel;

impl WorkspaceModel for FaultModel {
    type Intent = Vec<u8>;
    type Error = PlanningError;

    fn request_id(&self, intent: &Self::Intent) -> [u8; 32] {
        ObjectVersion::<FaultIntentSchema>::from_value(intent).to_bytes()
    }

    fn prepare(
        &self,
        base: &WorkspaceSnapshot,
        intent: &Self::Intent,
        transaction: TransactionId,
    ) -> Result<PreparedTransition, Self::Error> {
        let manifest = checked_manifest()?;
        if base.manifest() != &manifest {
            return Err(PlanningError(
                "fault base is not checked genesis".to_owned(),
            ));
        }
        let request = self.request_id(intent);
        let delta = WorkspaceDelta::new(&manifest, &manifest, Vec::new())
            .map_err(|error| PlanningError(error.to_string()))?;
        let provenance = CommitProvenance::new(
            ObjectClosure::from_version(ObjectVersion::<FaultAuthority>::from_value(
                &AUTHORITY_VALUE,
            )),
            ObjectClosure::from_version(transaction.version()),
            request.to_vec(),
        );
        let commit = commit_checked(&manifest, Vec::new(), provenance)
            .map_err(|error| PlanningError(error.to_string()))?;
        let closure = closure_for(&manifest, Some(transaction))?;
        PreparedTransition::new(request, transaction, delta, commit, closure)
            .map_err(|error| PlanningError(error.to_string()))
    }

    fn admit_persisted(
        &self,
        persisted: &backend_engine::PersistedTransition,
    ) -> Result<PreparedTransition, Self::Error> {
        let expected = checked_manifest()?;
        let untrusted_manifest = WorkspaceManifest::decode_untrusted(persisted.manifest_bytes())
            .map_err(|error| PlanningError(error.to_string()))?;
        let manifest = untrusted_manifest
            .admit_checked(
                Vec::new(),
                Vec::new(),
                ObjectClosure::from_version(ObjectVersion::<FaultAuthority>::from_value(
                    &AUTHORITY_VALUE,
                )),
                crash_scope_equality_fixture()?,
            )
            .map_err(|error| PlanningError(error.to_string()))?;
        if manifest != expected {
            return Err(PlanningError("persisted manifest changed".to_owned()));
        }
        let untrusted_delta = WorkspaceDelta::decode_untrusted(persisted.delta_bytes())
            .map_err(|error| PlanningError(error.to_string()))?;
        let delta = untrusted_delta
            .admit(&manifest, &manifest, Vec::new())
            .map_err(|error| PlanningError(error.to_string()))?;
        let untrusted_commit = Commit::decode_untrusted(persisted.commit_bytes())
            .map_err(|error| PlanningError(error.to_string()))?;
        let provenance = untrusted_commit
            .provenance()
            .clone()
            .admit(
                ObjectClosure::from_version(ObjectVersion::<FaultAuthority>::from_value(
                    &AUTHORITY_VALUE,
                )),
                ObjectClosure::from_version(persisted.transaction().version()),
            )
            .map_err(|error| PlanningError(error.to_string()))?;
        if provenance.detail() != persisted.request() {
            return Err(PlanningError(
                "persisted request provenance changed".to_owned(),
            ));
        }
        let commit = untrusted_commit
            .admit(&manifest, provenance)
            .map_err(|error| PlanningError(error.to_string()))?;
        let checked_delta = delta.clone().into_checked();
        let checked_commit = commit.clone().into_checked();
        let closure = WorkspaceClosure::from_checked_transition(
            &manifest,
            &checked_delta,
            Some(&checked_commit),
            persisted.closure_manifest().clone(),
        )
        .map_err(|error| PlanningError(format!("{error:?}")))?;
        PreparedTransition::new(
            persisted.request(),
            persisted.transaction(),
            delta,
            commit,
            closure,
        )
        .map_err(|error| PlanningError(error.to_string()))
    }
}

#[derive(Clone, Debug)]
struct PlanningError(String);

impl fmt::Display for PlanningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PlanningError {}

fn temporary_directory(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let path = std::env::temp_dir().join(format!(
        "backend-fault-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap_or_else(|error| panic!("create test directory: {error}"));
    path
}

fn open_owner(path: &Path) -> Result<WorkspaceOwner<FaultModel>, WorkspaceError> {
    WorkspaceOwner::open(
        path,
        FaultModel,
        genesis().map_err(|error| WorkspaceError::Model(error.to_string()))?,
    )
}

#[test]
fn live_engine_owner_fences_independent_store_publication() {
    let path = temporary_directory("owner-store-divergence");
    let owner = open_owner(&path).unwrap_or_else(|error| panic!("open owner: {error:?}"));
    let store = FileStore::open(path.join("objects"), 64 * 1024)
        .unwrap_or_else(|error| panic!("open independent store handle: {error:?}"));
    let prepared = store
        .prepare_map(
            &raw_map(7),
            LayoutId::derive(b"engine-divergence-layout.v1\0"),
        )
        .unwrap_or_else(|error| panic!("prepare independent publication: {error:?}"));
    let durable = prepared
        .durable()
        .unwrap_or_else(|error| panic!("durable independent publication: {error:?}"));
    assert!(matches!(
        durable.publish(),
        Err(StoreError::PublicationAuthorityBusy)
    ));
    assert_eq!(owner.head().sequence(), 0);
    drop(owner);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup owner/store: {error}"));
}

fn raw_map(value: u8) -> OrderedMap {
    OrderedMap::try_from_iter([(
        b"race-key".to_vec(),
        StoredValue::new(vec![value], 1, Vec::new()),
    )])
    .unwrap_or_else(|error| panic!("construct race map: {error:?}"))
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

/// Upper bound on a child process reaching a readiness point or exiting. It
/// is not a latency budget: 15s was exceeded on the loaded Linux PR worker
/// while the stale owner was still starting (builds 2397 and 2486).
const PROCESS_READY_BOUND: Duration = Duration::from_secs(60);

fn wait_for_file(path: &Path, label: &str) {
    let deadline = Instant::now() + PROCESS_READY_BOUND;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {label}: {}",
            path.display()
        );
        thread::yield_now();
    }
}

fn wait_for_any_file(paths: &[PathBuf], label: &str) -> usize {
    let deadline = Instant::now() + PROCESS_READY_BOUND;
    loop {
        if let Some(index) = paths.iter().position(|path| path.exists()) {
            return index;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::yield_now();
    }
}

fn wait_child(mut child: Child, label: &str) -> ExitStatus {
    let deadline = Instant::now() + PROCESS_READY_BOUND;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() < deadline => thread::yield_now(),
            Ok(None) => {
                let _ = child.kill();
                let reap_deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap_deadline {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) => thread::yield_now(),
                    }
                }
                panic!("timed out waiting for {label}");
            }
            Err(error) => panic!("wait for {label}: {error}"),
        }
    }
}

fn race_child_mode() -> bool {
    let Some(root) = std::env::var_os("BACKEND_STORE_RACE_CHILD") else {
        return false;
    };
    let root = PathBuf::from(root);
    let id = std::env::var("BACKEND_STORE_RACE_ID")
        .unwrap_or_else(|error| panic!("store race child id: {error}"));
    let ready = root.join(format!("race-ready-{id}"));
    let start = root.join("race-start");
    let done = root.join(format!("race-done-{id}"));
    fs::write(&ready, b"ready").unwrap_or_else(|error| panic!("write race readiness: {error}"));
    wait_for_file(&start, "store race start");

    let value = if id == "one" { 1 } else { 2 };
    let result = FileStore::open(&root, 64 * 1024).and_then(|store| {
        store
            .prepare_map(&raw_map(value), LayoutId::derive(b"store-race-layout"))
            .and_then(|prepared| {
                fs::write(root.join(format!("race-prepared-{id}")), b"prepared")
                    .map_err(|error| StoreError::Io(error.to_string()))?;
                wait_for_file(&root.join("race-durable-start"), "store race durable start");
                let durable = prepared.durable()?;
                fs::write(root.join(format!("race-durable-{id}")), b"durable")
                    .map_err(|error| StoreError::Io(error.to_string()))?;
                wait_for_file(&root.join("race-publish-start"), "store race publish start");
                let authority =
                    store
                        .acquire_publication_authority()
                        .map_err(|error| match error {
                            PublicationAuthorityError::Busy => StoreError::PublicationAuthorityBusy,
                            PublicationAuthorityError::Io(error) => StoreError::Io(error),
                        })?;
                durable
                    .publish_with_authority(&authority)
                    .map(|published| format!("ok:{}", hex(published.root().as_bytes())))
            })
    });
    let status = match result {
        Ok(status) => status,
        Err(StoreError::StaleHead) => "stale".to_owned(),
        Err(error) => format!("error:{error:?}"),
    };
    fs::write(&done, status.as_bytes())
        .unwrap_or_else(|error| panic!("write store race result: {error}"));
    true
}

fn workspace_race_child_mode() -> bool {
    let Some(root) = std::env::var_os("BACKEND_WORKSPACE_RACE_CHILD") else {
        return false;
    };
    let root = PathBuf::from(root);
    let id = std::env::var("BACKEND_WORKSPACE_RACE_ID")
        .unwrap_or_else(|error| panic!("workspace race child id: {error}"));
    let ready = root.join(format!("workspace-race-ready-{id}"));
    let start = root.join("workspace-race-start");
    let done = root.join(format!("workspace-race-done-{id}"));
    fs::write(&ready, b"ready")
        .unwrap_or_else(|error| panic!("write workspace race readiness: {error}"));
    wait_for_file(&start, "workspace race start");

    let status = match open_owner(&root) {
        Ok(mut owner) => {
            let owned_marker = root.join(format!("workspace-race-owned-{id}"));
            fs::write(&owned_marker, b"owned")
                .unwrap_or_else(|error| panic!("write workspace race owner marker: {error}"));
            wait_for_file(
                &root.join("workspace-race-release"),
                "workspace race owner release",
            );
            let expected = owner.head().expectation();
            let intent = format!("workspace-race-{id}").into_bytes();
            let status = owner
                .prepare(expected, intent)
                .and_then(|prepared| owner.durable(prepared))
                .and_then(|durable| owner.publish(durable))
                .map_or_else(
                    |error| format!("writer-error:{error:?}"),
                    |_| "published".to_owned(),
                );
            drop(owner);
            status
        }
        Err(WorkspaceError::AlreadyOwned) => "already-owned".to_owned(),
        Err(error) => format!("open-error:{error:?}"),
    };
    fs::write(&done, status.as_bytes())
        .unwrap_or_else(|error| panic!("write workspace race result: {error}"));
    true
}

fn stale_owner_child_mode() -> bool {
    let Some(root) = std::env::var_os("BACKEND_STALE_OWNER_CHILD") else {
        return false;
    };
    let root = PathBuf::from(root);
    let owner =
        open_owner(&root).unwrap_or_else(|error| panic!("stale owner child open: {error:?}"));
    let ready = root.join("stale-owner-ready");
    let takeover = root.join("stale-owner-takeover");
    let done = root.join("stale-owner-done");
    fs::write(&ready, b"ready")
        .unwrap_or_else(|error| panic!("write stale owner readiness: {error}"));
    wait_for_file(&takeover, "stale owner takeover");
    let result = owner
        .prepare(owner.head().expectation(), b"stale-owner-intent".to_vec())
        .map_or_else(
            |error| format!("rejected:{error:?}"),
            |_| "unexpectedly-accepted".to_owned(),
        );
    fs::write(&done, result.as_bytes())
        .unwrap_or_else(|error| panic!("write stale owner result: {error}"));
    let release = root.join("stale-owner-release");
    wait_for_file(&release, "stale owner release");
    drop(owner);
    true
}

fn parse_boundary(name: &str) -> Option<Boundary> {
    Some(match name {
        "prepare" => Boundary::Prepare,
        "object-write" => Boundary::ObjectWrite,
        "closure-write" => Boundary::ClosureWrite,
        "object" => Boundary::ObjectFlush,
        "transfer" => Boundary::Transfer,
        "journal-prepared" => Boundary::JournalPrepared,
        "journal" | "journal-publish" => Boundary::JournalFlush,
        "journal-select" => Boundary::JournalSelect,
        "head-write" => Boundary::HeadWrite,
        "temp-create" => Boundary::TempCreate,
        "temp-write" => Boundary::TempWrite,
        "file-sync" => Boundary::FileSync,
        "rename" => Boundary::Rename,
        "dir-sync" => Boundary::DirSync,
        "head" => Boundary::HeadSelection,
        "journal-published" => Boundary::JournalPublished,
        "notify" => Boundary::Notification,
        "recovery" => Boundary::Recovery,
        "effect-prepared" => Boundary::EffectPrepared,
        "effect-executing" => Boundary::EffectExecuting,
        "effect-call" => Boundary::EffectCall,
        "effect-ambiguous" => Boundary::EffectAmbiguous,
        "effect-confirmed" => Boundary::EffectConfirmed,
        _ => return None,
    })
}

fn mark_boundary(directory: &Path, faults: &Faults, boundary: Boundary) {
    let fired = faults.trip(boundary).is_ok();
    fs::write(
        directory.join("fault-fired"),
        if fired {
            &b"fired"[..]
        } else {
            &b"not-fired"[..]
        },
    )
    .unwrap_or_else(|error| panic!("write fault marker: {error}"));
}

fn child_fault_mode() -> bool {
    let Ok(path) = std::env::var("BACKEND_FAULT_CHILD") else {
        return false;
    };
    let Some(boundary_name) = std::env::var("BACKEND_FAULT_BOUNDARY")
        .ok()
        .and_then(|name| parse_boundary(&name))
    else {
        return false;
    };
    if boundary_name == Boundary::Recovery {
        let faults = Arc::new(Faults::default());
        faults.arm(boundary_name);
        let _ = WorkspaceOwner::open_with_faults(
            &path,
            FaultModel,
            genesis().unwrap_or_else(|error| panic!("child genesis: {error}")),
            Arc::clone(&faults),
        );
        mark_boundary(Path::new(&path), &faults, boundary_name);
    } else {
        let mut owner =
            open_owner(Path::new(&path)).unwrap_or_else(|error| panic!("child open: {error}"));
        let publish_flush =
            std::env::var("BACKEND_FAULT_BOUNDARY").ok().as_deref() == Some("journal-publish");
        if !publish_flush {
            owner.faults().arm(boundary_name);
        }
        let expected = owner.head().expectation();
        if let Ok(prepared) = owner.prepare(expected, b"child-intent".to_vec())
            && let Ok(durable) = owner.durable(prepared)
        {
            if publish_flush {
                owner.faults().arm(Boundary::JournalFlush);
            }
            if let Ok(published) = owner.publish(durable) {
                let _ = published;
            }
        }
        // `Faults` is one-shot. A successful probe means the production path
        // consumed the requested hook (including hooks such as head selection
        // whose error is intentionally converted into pending status). An
        // error means the child reached no named seam; the matrix must reject
        // that row instead of treating the final process exit as a crash at
        // the requested boundary.
        let owner_faults = owner.faults();
        mark_boundary(Path::new(&path), &owner_faults, boundary_name);
    }
    // Exit without running destructors so the owner lock and open files are
    // left to the OS exactly as they would be after a crash. `abort()` starts
    // the platform crash reporter and can leave a test child uncollectable.
    std::process::exit(101);
}

#[derive(Clone, Copy, Debug)]
struct BytesLog;

impl JournalDomain for BytesLog {
    const DOMAIN: u8 = 0x91;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
}

impl JournalCodec for BytesLog {
    type Record = Vec<u8>;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        output.extend_from_slice(record);
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        Ok(bytes.to_vec())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FaultEffect;

impl EffectSpec for FaultEffect {
    type Intent = Vec<u8>;
    type Receipt = Vec<u8>;
    type Request = Vec<u8>;

    fn key(&self, intent: &Self::Intent) -> backend_engine::EffectKey {
        effect_key(intent)
    }

    fn request(&self, intent: &Self::Intent) -> Self::Request {
        intent.clone()
    }

    fn validate_receipt(
        &self,
        _key: backend_engine::EffectKey,
        request: &Self::Request,
        receipt: &Self::Receipt,
    ) -> Result<(), EffectError> {
        (request == receipt)
            .then_some(())
            .ok_or(EffectError::InvalidReceipt(
                "receipt does not bind".to_owned(),
            ))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FaultEffectCodec;

impl EffectCodec<FaultEffect> for FaultEffectCodec {
    fn encode_intent(
        &self,
        intent: &<FaultEffect as EffectSpec>::Intent,
    ) -> Result<Vec<u8>, EffectError> {
        Ok(intent.clone())
    }

    fn decode_intent(
        &self,
        bytes: &[u8],
    ) -> Result<<FaultEffect as EffectSpec>::Intent, EffectError> {
        Ok(bytes.to_vec())
    }

    fn encode_receipt(
        &self,
        receipt: &<FaultEffect as EffectSpec>::Receipt,
    ) -> Result<Vec<u8>, EffectError> {
        Ok(receipt.clone())
    }

    fn decode_receipt(
        &self,
        bytes: &[u8],
    ) -> Result<<FaultEffect as EffectSpec>::Receipt, EffectError> {
        Ok(bytes.to_vec())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FaultEffectSink;

impl EffectSink<FaultEffect> for FaultEffectSink {
    fn apply(
        &mut self,
        _key: backend_engine::EffectKey,
        request: Vec<u8>,
    ) -> Result<SinkApply<Vec<u8>>, backend_engine::SinkError> {
        Ok(SinkApply::Confirmed(request))
    }

    fn reconcile(
        &mut self,
        _key: backend_engine::EffectKey,
    ) -> Result<SinkObservation<Vec<u8>>, backend_engine::SinkError> {
        Ok(SinkObservation::Unknown)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct UnknownEffectSink;

impl EffectSink<FaultEffect> for UnknownEffectSink {
    fn apply(
        &mut self,
        _key: backend_engine::EffectKey,
        _request: Vec<u8>,
    ) -> Result<SinkApply<Vec<u8>>, backend_engine::SinkError> {
        Ok(SinkApply::Unknown)
    }

    fn reconcile(
        &mut self,
        _key: backend_engine::EffectKey,
    ) -> Result<SinkObservation<Vec<u8>>, backend_engine::SinkError> {
        Ok(SinkObservation::Unknown)
    }
}

#[derive(Debug, Default)]
struct UnknownOnceSink {
    applied: Option<Vec<u8>>,
    apply_calls: usize,
}

impl EffectSink<FaultEffect> for UnknownOnceSink {
    fn apply(
        &mut self,
        _key: backend_engine::EffectKey,
        request: Vec<u8>,
    ) -> Result<SinkApply<Vec<u8>>, backend_engine::SinkError> {
        self.apply_calls += 1;
        if let Some(receipt) = &self.applied {
            return Ok(SinkApply::Confirmed(receipt.clone()));
        }
        self.applied = Some(request);
        Ok(SinkApply::Unknown)
    }

    fn reconcile(
        &mut self,
        _key: backend_engine::EffectKey,
    ) -> Result<SinkObservation<Vec<u8>>, backend_engine::SinkError> {
        self.applied
            .clone()
            .map_or(Ok(SinkObservation::Unknown), |receipt| {
                Ok(SinkObservation::Applied(receipt))
            })
    }
}

fn open_fault_effect(path: &Path) -> EffectJournalPersistence<FaultEffect, FaultEffectCodec> {
    EffectJournalPersistence::open_streaming(
        path,
        FaultEffectCodec,
        JournalLimits {
            max_frames: 1024,
            max_bytes: 4 * 1024 * 1024,
        },
    )
    .unwrap_or_else(|error| panic!("open effect journal: {error}"))
    .0
}

fn seed_fault_effect(path: &Path) {
    let persistence = open_fault_effect(path);
    let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
    let prepared = coordinator
        .prepare(b"checkpoint-intent".to_vec())
        .unwrap_or_else(|error| panic!("prepare effect fixture: {error}"));
    let executed = coordinator
        .execute(prepared.key())
        .unwrap_or_else(|error| panic!("execute effect fixture: {error}"));
    assert_eq!(executed.phase, backend_engine::EffectPhase::Confirmed);
}

fn effect_crash_child_mode() -> bool {
    let Some(path) = std::env::var_os("BACKEND_EFFECT_CRASH_CHILD") else {
        return false;
    };
    let path = PathBuf::from(path);
    let boundary = std::env::var("BACKEND_EFFECT_BOUNDARY")
        .ok()
        .and_then(|name| parse_boundary(&name))
        .unwrap_or_else(|| panic!("effect boundary is invalid"));
    let faults = Arc::new(Faults::default());
    let persistence = open_fault_effect(Path::new(&path)).with_faults(Arc::clone(&faults));
    let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
    let recovered = coordinator
        .persistence_mut()
        .replay(&FaultEffect)
        .unwrap_or_else(|error| panic!("replay effect fixture: {error}"));
    faults.arm(boundary);
    let _ = coordinator.persistence_mut().checkpoint_and_compact(
        &FaultEffect,
        recovered,
        EffectSnapshotLimits {
            max_entries: 16,
            max_bytes: 64 * 1024,
        },
        JournalLimits {
            max_frames: 1024,
            max_bytes: 4 * 1024 * 1024,
        },
    );
    mark_boundary(
        path.parent().unwrap_or_else(|| Path::new(".")),
        &faults,
        boundary,
    );
    std::process::exit(101);
}

fn run_effect_phase_child<S>(path: &Path, faults: &Arc<Faults>, boundary: Boundary, sink: S)
where
    S: EffectSink<FaultEffect>,
{
    let persistence = open_fault_effect(path).with_faults(Arc::clone(faults));
    let mut coordinator =
        EffectCoordinator::new(FaultEffect, persistence, sink).with_faults(Arc::clone(faults));
    if let Ok(prepared) = coordinator.prepare(b"phase-boundary-intent".to_vec()) {
        let _ = coordinator.execute(prepared.key());
    }
    mark_boundary(
        path.parent().unwrap_or_else(|| Path::new(".")),
        faults,
        boundary,
    );
    std::process::exit(101);
}

fn effect_phase_crash_child_mode() -> bool {
    let Some(path) = std::env::var_os("BACKEND_EFFECT_PHASE_CHILD") else {
        return false;
    };
    let path = PathBuf::from(path);
    let boundary = std::env::var("BACKEND_EFFECT_PHASE_BOUNDARY")
        .ok()
        .and_then(|name| parse_boundary(&name))
        .unwrap_or_else(|| panic!("effect phase boundary is invalid"));
    let faults = Arc::new(Faults::default());
    faults.arm(boundary);
    if boundary == Boundary::EffectAmbiguous {
        run_effect_phase_child(&path, &faults, boundary, UnknownEffectSink);
    } else {
        run_effect_phase_child(&path, &faults, boundary, FaultEffectSink);
    }
    true
}

const MATRIX: &[(&str, bool)] = &[
    ("prepare", false),
    ("object-write", false),
    ("closure-write", false),
    ("object", false),
    ("transfer", false),
    ("journal-prepared", false),
    ("journal", false),
    ("journal-publish", true),
    ("journal-select", true),
    ("head-write", true),
    ("temp-create", true),
    ("temp-write", true),
    ("file-sync", true),
    ("rename", true),
    ("dir-sync", true),
    ("head", true),
    ("journal-published", true),
    ("notify", true),
    ("recovery", false),
];

const EFFECT_CHECKPOINT_BOUNDARIES: &[&str] = &[
    "temp-create",
    "temp-write",
    "file-sync",
    "rename",
    "dir-sync",
];

const EFFECT_PHASE_BOUNDARIES: &[(&str, Option<backend_engine::EffectPhase>)] = &[
    ("effect-prepared", None),
    (
        "effect-executing",
        Some(backend_engine::EffectPhase::Prepared),
    ),
    ("effect-call", Some(backend_engine::EffectPhase::Ambiguous)),
    (
        "effect-ambiguous",
        Some(backend_engine::EffectPhase::Ambiguous),
    ),
    (
        "effect-confirmed",
        Some(backend_engine::EffectPhase::Ambiguous),
    ),
];

#[test]
fn production_fault_matrix_reopens_old_or_new_checked_head() {
    if child_fault_mode() {
        return;
    }
    for (boundary, expects_new) in MATRIX {
        let path = temporary_directory(boundary);
        let output = Command::new(
            std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
        )
        .arg("--exact")
        .arg("production_fault_matrix_reopens_old_or_new_checked_head")
        .arg("--nocapture")
        .env("BACKEND_FAULT_CHILD", &path)
        .env("BACKEND_FAULT_BOUNDARY", boundary)
        .output()
        .unwrap_or_else(|error| panic!("spawn child: {error}"));
        assert!(
            !output.status.success(),
            "fault child unexpectedly survived"
        );
        assert_eq!(
            fs::read_to_string(path.join("fault-fired"))
                .unwrap_or_else(|error| panic!("read fault marker for {boundary}: {error}")),
            "fired",
            "fault boundary {boundary} was never reached"
        );
        let owner = WorkspaceOwner::reclaim(
            &path,
            FaultModel,
            genesis().unwrap_or_else(|error| panic!("genesis {boundary}: {error}")),
        )
        .unwrap_or_else(|error| panic!("recover {boundary}: {error:?}"));
        assert!(owner.head().manifest().is_checked());
        assert_eq!(owner.head().closure().root(), owner.head().root());
        assert_eq!(owner.head().sequence(), u64::from(*expects_new));
        drop(owner);
        fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }
}

#[test]
fn effect_checkpoint_faults_recover_the_previous_or_selected_snapshot() {
    if effect_crash_child_mode() {
        return;
    }
    for boundary in EFFECT_CHECKPOINT_BOUNDARIES {
        let directory = temporary_directory(&format!("effect-{boundary}"));
        let path = directory.join("effects.journal");
        seed_fault_effect(&path);
        let child = Command::new(
            std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
        )
        .arg("--exact")
        .arg("effect_checkpoint_faults_recover_the_previous_or_selected_snapshot")
        .arg("--nocapture")
        .env("BACKEND_EFFECT_CRASH_CHILD", &path)
        .env("BACKEND_EFFECT_BOUNDARY", boundary)
        .spawn()
        .unwrap_or_else(|error| panic!("spawn effect fault child {boundary}: {error}"));
        assert!(
            !wait_child(child, "effect fault child").success(),
            "effect fault child survived boundary {boundary}"
        );
        assert_eq!(
            fs::read_to_string(directory.join("fault-fired"))
                .unwrap_or_else(|error| panic!("read effect fault marker {boundary}: {error}")),
            "fired",
            "effect checkpoint boundary {boundary} was never reached"
        );

        let persistence = open_fault_effect(&path);
        let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
        let recovered = coordinator
            .persistence_mut()
            .replay(&FaultEffect)
            .unwrap_or_else(|error| panic!("recover effect after {boundary}: {error}"));
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].state.phase(),
            backend_engine::EffectPhase::Confirmed
        );
        drop(coordinator);
        fs::remove_dir_all(directory)
            .unwrap_or_else(|error| panic!("cleanup effect {boundary}: {error}"));
    }
}

#[test]
fn effect_journal_phase_faults_leave_only_a_replayable_prefix() {
    if effect_phase_crash_child_mode() {
        return;
    }
    for (boundary, expected_phase) in EFFECT_PHASE_BOUNDARIES {
        let directory = temporary_directory(&format!("effect-phase-{boundary}"));
        let path = directory.join("effects.journal");
        let child = Command::new(
            std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
        )
        .arg("--exact")
        .arg("effect_journal_phase_faults_leave_only_a_replayable_prefix")
        .arg("--nocapture")
        .env("BACKEND_EFFECT_PHASE_CHILD", &path)
        .env("BACKEND_EFFECT_PHASE_BOUNDARY", boundary)
        .spawn()
        .unwrap_or_else(|error| panic!("spawn effect phase child {boundary}: {error}"));
        assert!(
            !wait_child(child, "effect phase fault child").success(),
            "effect phase child survived boundary {boundary}"
        );
        assert_eq!(
            fs::read_to_string(directory.join("fault-fired")).unwrap_or_else(|error| {
                panic!("read effect phase fault marker {boundary}: {error}")
            }),
            "fired",
            "effect phase boundary {boundary} was never reached"
        );

        let persistence = open_fault_effect(&path);
        let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
        let recovered = coordinator
            .persistence_mut()
            .replay(&FaultEffect)
            .unwrap_or_else(|error| panic!("recover effect phase {boundary}: {error}"));
        match expected_phase {
            Some(expected) => {
                assert_eq!(recovered.len(), 1);
                assert_eq!(recovered[0].state.phase(), *expected);
            }
            None => assert!(recovered.is_empty()),
        }
        drop(coordinator);
        fs::remove_dir_all(directory)
            .unwrap_or_else(|error| panic!("cleanup effect phase {boundary}: {error}"));
    }
}

#[test]
fn ambiguous_external_effect_reconciles_exactly_once_and_retries_by_key() {
    let directory = temporary_directory("effect-ambiguous-retry");
    let path = directory.join("effects.journal");
    let persistence = open_fault_effect(&path);
    let mut coordinator =
        EffectCoordinator::new(FaultEffect, persistence, UnknownOnceSink::default());
    let intent = b"ambiguous-retry-intent".to_vec();
    let prepared = coordinator
        .prepare(intent.clone())
        .unwrap_or_else(|error| panic!("prepare ambiguous effect: {error}"));
    let executing = coordinator
        .execute(prepared.key())
        .unwrap_or_else(|error| panic!("execute ambiguous effect: {error}"));
    assert_eq!(executing.phase(), backend_engine::EffectPhase::Ambiguous);
    let confirmed = coordinator
        .reconcile(prepared.key())
        .unwrap_or_else(|error| panic!("reconcile ambiguous effect: {error}"));
    assert_eq!(confirmed.phase(), backend_engine::EffectPhase::Confirmed);
    assert_eq!(coordinator.sink_mut().apply_calls, 1);

    let retry = coordinator
        .prepare(intent)
        .unwrap_or_else(|error| panic!("retry prepared effect: {error}"));
    assert_eq!(retry.phase(), backend_engine::EffectPhase::Confirmed);
    assert!(matches!(
        coordinator.execute(retry.key()),
        Err(EffectError::InvalidTransition)
    ));
    assert_eq!(coordinator.sink_mut().apply_calls, 1);
    let recovered = coordinator
        .persistence_mut()
        .replay(&FaultEffect)
        .unwrap_or_else(|error| panic!("replay reconciled effect: {error}"));
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].state.phase(),
        backend_engine::EffectPhase::Confirmed
    );
    drop(coordinator);
    fs::remove_dir_all(directory)
        .unwrap_or_else(|error| panic!("cleanup ambiguous effect: {error}"));
}

#[test]
fn effect_replay_rejects_a_torn_compaction_replacement_without_its_marker() {
    let directory = temporary_directory("effect-torn-compaction");
    let path = directory.join("effects.journal");
    seed_fault_effect(&path);
    let persistence = open_fault_effect(&path);
    let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
    let recovered = coordinator
        .persistence_mut()
        .replay(&FaultEffect)
        .unwrap_or_else(|error| panic!("replay before compaction: {error}"));
    coordinator
        .persistence_mut()
        .checkpoint_and_compact(
            &FaultEffect,
            recovered,
            EffectSnapshotLimits {
                max_entries: 16,
                max_bytes: 64 * 1024,
            },
            JournalLimits {
                max_frames: 1024,
                max_bytes: 4 * 1024 * 1024,
            },
        )
        .unwrap_or_else(|error| panic!("compact effect fixture: {error}"));
    drop(coordinator);
    let compacted =
        fs::read(&path).unwrap_or_else(|error| panic!("read compacted journal: {error}"));
    let persistence = open_fault_effect(&path);
    let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
    let recovered = coordinator
        .persistence_mut()
        .replay(&FaultEffect)
        .unwrap_or_else(|error| panic!("replay complete compacted journal: {error}"));
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].state.phase(),
        backend_engine::EffectPhase::Confirmed
    );
    drop(coordinator);
    // Try every possible crash truncation point in the compacted replacement.
    // Only the complete authenticated marker is allowed to select the
    // snapshot; every shorter prefix must fail closed after tail repair.
    for cut in 0..=compacted.len() {
        fs::write(&path, &compacted[..cut])
            .unwrap_or_else(|error| panic!("tear compacted journal at {cut}: {error}"));
        let persistence = open_fault_effect(&path);
        let mut coordinator = EffectCoordinator::new(FaultEffect, persistence, FaultEffectSink);
        if cut == compacted.len() {
            let recovered = coordinator
                .persistence_mut()
                .replay(&FaultEffect)
                .unwrap_or_else(|error| panic!("replay complete compaction at {cut}: {error:?}"));
            assert_eq!(recovered.len(), 1);
            assert_eq!(
                recovered[0].state.phase(),
                backend_engine::EffectPhase::Confirmed
            );
        } else {
            let replay = coordinator.persistence_mut().replay(&FaultEffect);
            assert!(matches!(
                replay,
                Err(EffectError::Journal(JournalError::Corrupt(
                    "checkpoint receipt"
                )))
            ));
        }
        drop(coordinator);
        if cut < compacted.len() {
            // Recovery repaired only the incomplete physical suffix; it must
            // not promote the snapshot until a complete authenticated marker
            // exists.
            assert_eq!(
                fs::metadata(&path)
                    .unwrap_or_else(|error| panic!("stat repaired compacted journal: {error}"))
                    .len(),
                0
            );
        }
    }
    fs::remove_dir_all(directory)
        .unwrap_or_else(|error| panic!("cleanup effect compaction: {error}"));
}

#[test]
fn two_process_store_writers_select_one_complete_head() {
    if race_child_mode() {
        return;
    }
    let path = temporary_directory("store-two-process");
    let store = FileStore::open(&path, 64 * 1024)
        .unwrap_or_else(|error| panic!("open race store: {error:?}"));
    store
        .publish(&raw_map(0), LayoutId::derive(b"store-race-layout"))
        .unwrap_or_else(|error| panic!("write race genesis: {error:?}"));
    drop(store);

    let mut children = Vec::new();
    for id in ["one", "two"] {
        let child = Command::new(
            std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
        )
        .arg("--exact")
        .arg("two_process_store_writers_select_one_complete_head")
        .arg("--nocapture")
        .env("BACKEND_STORE_RACE_CHILD", &path)
        .env("BACKEND_STORE_RACE_ID", id)
        .spawn()
        .unwrap_or_else(|error| panic!("spawn store race child {id}: {error}"));
        children.push(child);
    }
    for id in ["one", "two"] {
        wait_for_file(
            &path.join(format!("race-ready-{id}")),
            "store race readiness",
        );
    }
    fs::write(path.join("race-start"), b"start")
        .unwrap_or_else(|error| panic!("start store race: {error}"));
    for id in ["one", "two"] {
        wait_for_file(
            &path.join(format!("race-prepared-{id}")),
            "store race prepared publication",
        );
    }
    fs::write(path.join("race-durable-start"), b"durable")
        .unwrap_or_else(|error| panic!("start store race durable phase: {error}"));
    for id in ["one", "two"] {
        wait_for_file(
            &path.join(format!("race-durable-{id}")),
            "store race durable publication",
        );
    }
    fs::write(path.join("race-publish-start"), b"publish")
        .unwrap_or_else(|error| panic!("start store race publish phase: {error}"));
    for id in ["one", "two"] {
        wait_for_file(&path.join(format!("race-done-{id}")), "store race result");
    }
    for (id, child) in ["one", "two"].into_iter().zip(children) {
        assert!(
            wait_child(child, "store race child").success(),
            "store race child {id} failed"
        );
    }

    let one = fs::read_to_string(path.join("race-done-one"))
        .unwrap_or_else(|error| panic!("read store race one: {error}"));
    let two = fs::read_to_string(path.join("race-done-two"))
        .unwrap_or_else(|error| panic!("read store race two: {error}"));
    let statuses = [one.trim(), two.trim()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| status.starts_with("ok:"))
            .count(),
        1,
        "exactly one process may publish the observed base"
    );
    let fenced = statuses
        .iter()
        .filter(|status| **status == "stale" || **status == "error:PublicationAuthorityBusy")
        .count();
    assert_eq!(
        fenced, 1,
        "the loser must observe a fenced publication authority or stale base; statuses={statuses:?}"
    );

    let store = FileStore::open(&path, 64 * 1024)
        .unwrap_or_else(|error| panic!("reopen raced store: {error:?}"));
    let selected = store
        .head()
        .unwrap_or_else(|error| panic!("read raced head: {error:?}"))
        .unwrap_or_else(|| unreachable!("genesis and one winner must select a head"));
    let recovered = store
        .recover()
        .unwrap_or_else(|error| panic!("recover raced store: {error:?}"))
        .unwrap_or_else(|| unreachable!("raced store must contain a map"));
    assert!(
        recovered.state_root() == raw_map(1).state_root()
            || recovered.state_root() == raw_map(2).state_root()
    );
    assert_eq!(
        selected.descriptor().target(),
        *recovered.state_root().as_bytes()
    );
    drop(store);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn two_process_workspace_writers_have_one_owner_and_one_published_transition() {
    if workspace_race_child_mode() {
        return;
    }
    let path = temporary_directory("workspace-two-process");
    let mut children = Vec::new();
    for id in ["one", "two"] {
        let child = Command::new(
            std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
        )
        .arg("--exact")
        .arg("two_process_workspace_writers_have_one_owner_and_one_published_transition")
        .arg("--nocapture")
        .env("BACKEND_WORKSPACE_RACE_CHILD", &path)
        .env("BACKEND_WORKSPACE_RACE_ID", id)
        .spawn()
        .unwrap_or_else(|error| panic!("spawn workspace race child {id}: {error}"));
        children.push(child);
    }
    for id in ["one", "two"] {
        wait_for_file(
            &path.join(format!("workspace-race-ready-{id}")),
            "workspace race readiness",
        );
    }
    fs::write(path.join("workspace-race-start"), b"start")
        .unwrap_or_else(|error| panic!("start workspace race: {error}"));
    let owned = wait_for_any_file(
        &[
            path.join("workspace-race-owned-one"),
            path.join("workspace-race-owned-two"),
        ],
        "workspace race owner",
    );
    let loser = if owned == 0 { "two" } else { "one" };
    wait_for_file(
        &path.join(format!("workspace-race-done-{loser}")),
        "workspace race loser",
    );
    fs::write(path.join("workspace-race-release"), b"release")
        .unwrap_or_else(|error| panic!("release workspace race owner: {error}"));
    for id in ["one", "two"] {
        wait_for_file(
            &path.join(format!("workspace-race-done-{id}")),
            "workspace race result",
        );
    }
    for (id, child) in ["one", "two"].into_iter().zip(children) {
        assert!(
            wait_child(child, "workspace race child").success(),
            "workspace race child {id} failed"
        );
    }

    let one = fs::read_to_string(path.join("workspace-race-done-one"))
        .unwrap_or_else(|error| panic!("read workspace race one: {error}"));
    let two = fs::read_to_string(path.join("workspace-race-done-two"))
        .unwrap_or_else(|error| panic!("read workspace race two: {error}"));
    let statuses = [one.trim(), two.trim()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "published")
            .count(),
        1,
        "exactly one process may publish while the owner lock is held"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "already-owned")
            .count(),
        1,
        "the second process must be rejected at owner acquisition"
    );

    let reopened_owner = open_owner(&path)
        .unwrap_or_else(|error| panic!("reopen workspace after two-process race: {error:?}"));
    assert_eq!(reopened_owner.head().sequence(), 1);
    drop(reopened_owner);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup workspace race: {error}"));
}

#[test]
fn truncated_visible_head_is_rejected_as_corruption() {
    let path = temporary_directory("torn-head");
    let store = FileStore::open(&path, 64 * 1024)
        .unwrap_or_else(|error| panic!("open torn-head store: {error:?}"));
    store
        .publish(&raw_map(0), LayoutId::derive(b"torn-head-layout"))
        .unwrap_or_else(|error| panic!("write torn-head genesis: {error:?}"));
    store
        .publish(&raw_map(1), LayoutId::derive(b"torn-head-layout"))
        .unwrap_or_else(|error| panic!("write torn-head target: {error:?}"));
    drop(store);

    let head_path = path.join("HEAD");
    let original = fs::read(&head_path).unwrap_or_else(|error| panic!("read HEAD: {error}"));
    let cuts = [0, 1, original.len() / 2, original.len().saturating_sub(1)];
    for cut in cuts {
        fs::write(&head_path, &original[..cut])
            .unwrap_or_else(|error| panic!("tear HEAD at {cut}: {error}"));
        assert!(matches!(
            FileStore::open(&path, 64 * 1024),
            Err(StoreError::Corrupt)
        ));
        fs::write(&head_path, &original)
            .unwrap_or_else(|error| panic!("restore HEAD after cut {cut}: {error}"));
    }
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn hash_chain_repairs_only_a_torn_tail_and_rejects_complete_corruption() {
    let directory = temporary_directory("journal");
    let path = directory.join("journal");
    let (journal, recovery) = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("open journal: {error}"));
    assert_eq!(recovery.last_sequence, None);
    journal
        .append(&b"first".to_vec())
        .unwrap_or_else(|error| panic!("append first: {error}"));
    journal
        .append(&b"second".to_vec())
        .unwrap_or_else(|error| panic!("append second: {error}"));
    drop(journal);
    let mut bytes = fs::read(&path).unwrap_or_else(|error| panic!("read journal: {error}"));
    let truncated = bytes
        .len()
        .checked_sub(3)
        .unwrap_or_else(|| unreachable!("two complete frames"));
    bytes.truncate(truncated);
    fs::write(&path, &bytes).unwrap_or_else(|error| panic!("truncate journal: {error}"));
    let (journal, repaired) = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("repair journal: {error}"));
    assert!(repaired.truncated_tail);
    assert_eq!(repaired.last_sequence, Some(0));
    drop(journal);
    let mut bytes =
        fs::read(&path).unwrap_or_else(|error| panic!("read repaired journal: {error}"));
    bytes[70] ^= 1;
    fs::write(&path, bytes).unwrap_or_else(|error| panic!("corrupt journal: {error}"));
    assert!(HashChainJournal::<BytesLog>::open(&path).is_err());
    fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn exact_retry_adopts_an_existing_ambiguous_append_without_duplicate_frames() {
    let directory = temporary_directory("journal-retry");
    let path = directory.join("journal");
    let (journal, _) = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("open retry journal: {error}"));
    let payload = b"one-exact-record".to_vec();
    let receipt = journal
        .append(&payload)
        .unwrap_or_else(|error| panic!("append retry record: {error}"));

    // Model a process receiving an ambiguous post-sync error. The receipt is
    // deliberately reconstructed through the public error seam, then retried
    // against the same journal and canonical payload.
    let uncertain = JournalError::AppendUncertain {
        offset: receipt.offset,
        sequence: receipt.sequence,
        chain: *receipt.chain.as_bytes(),
        record: *receipt.record.as_bytes(),
    };
    let candidate = uncertain
        .uncertain_receipt::<BytesLog>()
        .unwrap_or_else(|| unreachable!("uncertain append carries a receipt"));
    let before_mismatch =
        fs::read(&path).unwrap_or_else(|error| panic!("read retry frame: {error}"));
    drop(journal);
    let journal = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("reopen retry journal: {error}"))
        .0;
    assert!(matches!(
        journal.retry(candidate, &b"different-record".to_vec()),
        Err(JournalError::Corrupt("append receipt"))
    ));
    drop(journal);
    assert_eq!(
        fs::read(&path).unwrap_or_else(|error| panic!("read unchanged retry frame: {error}")),
        before_mismatch
    );

    let journal = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("reopen exact retry journal: {error}"))
        .0;
    let retried = journal
        .retry(candidate, &payload)
        .unwrap_or_else(|error| panic!("retry exact append: {error}"));
    assert_eq!(retried, receipt);

    let next = journal
        .append(&b"second-record".to_vec())
        .unwrap_or_else(|error| panic!("append after exact retry: {error}"));
    assert_eq!(next.sequence, receipt.sequence + 1);
    let recovery = journal
        .recover()
        .unwrap_or_else(|error| panic!("recover after exact retry: {error}"));
    assert_eq!(recovery.frames.len(), 2);
    assert_eq!(recovery.frames[0].payload.as_ref(), payload.as_slice());
    fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn checkpoint_recovery_scans_only_the_suffix_and_reuses_one_payload_buffer() {
    const PAYLOAD_BYTES: usize = 1024;
    const PREFIX_FRAMES: usize = 32;
    const SUFFIX_FRAMES: usize = 32;
    let directory = temporary_directory("journal-checkpoint");
    let path = directory.join("journal");
    let (journal, _) = HashChainJournal::<BytesLog>::open(&path)
        .unwrap_or_else(|error| panic!("open checkpoint journal: {error}"));
    let mut checkpoint = None;
    let mut frame_bytes = None;
    for sequence in 0..(PREFIX_FRAMES + SUFFIX_FRAMES) {
        let receipt = journal
            .append(&vec![
                u8::try_from(sequence).unwrap_or(u8::MAX);
                PAYLOAD_BYTES
            ])
            .unwrap_or_else(|error| panic!("append checkpoint frame {sequence}: {error}"));
        if sequence == 1 {
            frame_bytes = Some(
                usize::try_from(receipt.offset)
                    .unwrap_or_else(|_| unreachable!("frame offset fits usize")),
            );
        }
        if sequence + 1 == PREFIX_FRAMES {
            checkpoint = Some(receipt.checkpoint());
        }
    }
    let checkpoint = checkpoint.unwrap_or_else(|| unreachable!("checkpoint frame"));
    let frame_bytes = frame_bytes.unwrap_or_else(|| unreachable!("frame size"));
    drop(journal);
    let original =
        fs::read(&path).unwrap_or_else(|error| panic!("read checkpoint journal: {error}"));
    for tail_cut in [0, 1, 17, frame_bytes.saturating_sub(1)] {
        let bytes = original
            .len()
            .checked_sub(tail_cut)
            .unwrap_or_else(|| unreachable!("tail cut within journal"));
        fs::write(&path, &original[..bytes])
            .unwrap_or_else(|error| panic!("tear checkpoint suffix by {tail_cut}: {error}"));
        let mut first_pointer = None;
        let mut pointer_reused = true;
        let (journal, scan) = HashChainJournal::<BytesLog>::open_from_checkpoint_streaming_with(
            &path,
            checkpoint,
            JournalLimits {
                max_frames: SUFFIX_FRAMES + 1,
                max_bytes: original.len(),
            },
            |frame| {
                let pointer = frame.payload.as_ptr() as usize;
                if let Some(first) = first_pointer {
                    pointer_reused &= first == pointer;
                } else {
                    first_pointer = Some(pointer);
                }
                assert_eq!(frame.payload.len(), PAYLOAD_BYTES);
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("stream checkpoint recovery cut {tail_cut}: {error}"));
        assert!(scan.frames_scanned <= SUFFIX_FRAMES);
        assert!(scan.peak_payload_bytes <= PAYLOAD_BYTES);
        if scan.frames_scanned > 1 {
            assert!(
                pointer_reused,
                "stream recovery allocated one payload slot per frame"
            );
        }
        let appended = journal
            .append(&vec![0xee; PAYLOAD_BYTES])
            .unwrap_or_else(|error| panic!("append after checkpoint recovery: {error}"));
        assert_eq!(appended.sequence, scan.last_sequence.unwrap_or(0) + 1);
        drop(journal);
        let (_, recovery) = HashChainJournal::<BytesLog>::open(&path).unwrap_or_else(|error| {
            panic!("full recovery after checkpoint cut {tail_cut}: {error}")
        });
        assert!(!recovery.truncated_tail);
        let expected = vec![0xee; PAYLOAD_BYTES];
        assert_eq!(
            recovery.frames.last().map(|frame| frame.payload.as_ref()),
            Some(expected.as_slice())
        );
    }
    fs::remove_dir_all(directory).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn exact_workspace_retry_is_idempotent_and_object_identity_conflicts_fail_closed() {
    let path = temporary_directory("retry-and-collision");
    let mut owner = open_owner(&path).unwrap_or_else(|error| panic!("open retry owner: {error}"));
    let expected = owner.head().expectation();
    let intent = b"retry-intent".to_vec();
    let prepared = owner
        .prepare(expected, intent.clone())
        .unwrap_or_else(|error| panic!("prepare retry intent: {error}"));
    let durable = owner
        .durable(prepared)
        .unwrap_or_else(|error| panic!("durable retry intent: {error}"));
    let published = owner
        .publish(durable)
        .unwrap_or_else(|error| panic!("publish retry intent: {error}"));
    let selected = owner.head().clone();
    let retry = owner
        .prepare(expected, intent)
        .unwrap_or_else(|error| panic!("prepare exact retry: {error}"));
    let retry = owner
        .durable(retry)
        .unwrap_or_else(|error| panic!("durable exact retry: {error}"));
    let retry = owner
        .publish(retry)
        .unwrap_or_else(|error| panic!("publish exact retry: {error}"));
    assert_eq!(published.target(), retry.target());
    assert_eq!(owner.head(), &selected);
    drop(owner);

    let store_root = path.join("object-collision");
    let store = FileStore::open(&store_root, 64 * 1024)
        .unwrap_or_else(|error| panic!("open collision store: {error:?}"));
    let key_a = ObjectKey::<FaultAuthority>::from_value(&1);
    let key_b = ObjectKey::<FaultAuthority>::from_value(&2);
    let object_a = TypedObject::from_value(&key_a, &1_u64);
    let object_b = TypedObject::from_value(&key_b, &2_u64);
    let object_a_path = store
        .root()
        .join("objects")
        .join(format!("{}.object", hex(object_a.id().as_bytes())));
    let object_b_path = store
        .root()
        .join("objects")
        .join(format!("{}.object", hex(object_b.id().as_bytes())));
    store
        .write_object(&object_a)
        .unwrap_or_else(|error| panic!("write collision source: {error:?}"));
    let bytes =
        fs::read(&object_a_path).unwrap_or_else(|error| panic!("read collision source: {error}"));
    fs::write(&object_b_path, bytes)
        .unwrap_or_else(|error| panic!("install forged collision object: {error}"));
    assert!(matches!(
        store.write_object(&object_b),
        Err(StoreError::Corrupt)
    ));
    assert!(matches!(
        store.read_object(object_b.id()),
        Err(StoreError::Corrupt)
    ));
    drop(store);

    let pack_root = path.join("pack-collision");
    let store = FileStore::open(&pack_root, 64 * 1024)
        .unwrap_or_else(|error| panic!("open pack collision store: {error:?}"));
    let layout = LayoutId::derive(b"pack-collision-layout");
    store
        .publish(&raw_map(3), layout)
        .unwrap_or_else(|error| panic!("write pack collision source: {error:?}"));
    let source = store
        .head()
        .unwrap_or_else(|error| panic!("read pack collision head: {error:?}"))
        .unwrap_or_else(|| unreachable!("pack collision source head"))
        .descriptor()
        .pack();
    let prepared = store
        .prepare_map(&raw_map(4), layout)
        .unwrap_or_else(|error| panic!("prepare pack collision target: {error:?}"));
    let target = prepared.descriptor().pack();
    let source_path = store
        .root()
        .join("packs")
        .join(format!("{}.tree", hex(source.as_bytes())));
    let target_path = store
        .root()
        .join("packs")
        .join(format!("{}.tree", hex(target.as_bytes())));
    fs::copy(&source_path, &target_path)
        .unwrap_or_else(|error| panic!("install forged pack collision: {error}"));
    assert!(matches!(
        prepared
            .durable()
            .and_then(backend_store::FileDurable::publish),
        Err(StoreError::Corrupt)
    ));
    drop(store);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn shared_authority_reference_is_admitted_once_across_transition_layers() {
    let manifest =
        checked_manifest().unwrap_or_else(|error| panic!("build checked manifest: {error}"));
    let closure = closure_for(&manifest, None)
        .unwrap_or_else(|error| panic!("build checked closure: {error}"));

    let head = WorkspaceHead::genesis(manifest, closure)
        .unwrap_or_else(|error| panic!("admit repeated authority reference: {error}"));
    assert_eq!(head.sequence(), 0);
    assert!(head.manifest().is_checked());
}

#[test]
fn owner_lock_is_exclusive_and_live_owner_cannot_be_reclaimed() {
    let path = temporary_directory("lock");
    let first = open_owner(&path).unwrap_or_else(|error| panic!("first owner: {error}"));
    let first_epoch = first.lease().epoch();
    assert!(matches!(
        open_owner(&path),
        Err(WorkspaceError::AlreadyOwned)
    ));
    assert!(matches!(
        backend_engine::OwnerLease::reclaim(&path),
        Err(WorkspaceError::AlreadyOwned)
    ));
    drop(first);
    let second = open_owner(&path).unwrap_or_else(|error| panic!("second owner: {error}"));
    assert!(second.lease().epoch() > first_epoch);
    drop(second);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn owner_state_with_a_valid_new_fence_rejects_the_old_lease() {
    let path = temporary_directory("fence");
    let owner = open_owner(&path).unwrap_or_else(|error| panic!("owner: {error}"));
    let state_path = path.join("OWNER.state");
    let mut bytes =
        fs::read(&state_path).unwrap_or_else(|error| panic!("read canonical owner state: {error}"));
    let magic = b"LUNA_OWNER_STATE_V1\0";
    let fence_at = magic.len() + 8;
    bytes[fence_at] ^= 1;
    let checksum_at = fence_at + 32;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.engine.owner-state.v1\0");
    hasher.update(&bytes[..checksum_at]);
    bytes[checksum_at..].copy_from_slice(hasher.finalize().as_bytes());
    fs::write(&state_path, bytes).unwrap_or_else(|error| panic!("replace owner fence: {error}"));
    assert!(matches!(
        owner.lease().assert_current(),
        Err(WorkspaceError::Fenced)
    ));
    drop(owner);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn owner_gc_snapshot_pin_preserves_then_releases_an_object() {
    let path = temporary_directory("owner-gc-pin");
    let owner = open_owner(&path).unwrap_or_else(|error| panic!("owner: {error}"));
    let value = 77_u64;
    let key = ObjectKey::<FaultAuthority>::from_value(&value);
    let object = TypedObject::from_value(&key, &value);
    let id = owner
        .write_object(&object)
        .unwrap_or_else(|error| panic!("write GC pin object: {error:?}"));
    let pin = owner
        .pin_reader_root(GcRoot::Object(id))
        .unwrap_or_else(|error| panic!("pin reader object: {error}"));
    owner
        .collect_garbage(GcLimits::default())
        .unwrap_or_else(|error| panic!("collect with live pin: {error}"));
    assert!(
        owner
            .contains_object(id)
            .unwrap_or_else(|error| panic!("check pinned object: {error:?}"))
    );
    drop(pin);
    owner
        .collect_garbage(GcLimits::default())
        .unwrap_or_else(|error| panic!("collect after pin release: {error}"));
    assert!(
        !owner
            .contains_object(id)
            .unwrap_or_else(|error| panic!("check released object: {error:?}"))
    );
    drop(owner);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn old_process_is_fenced_after_a_new_owner_takes_over() {
    if stale_owner_child_mode() {
        return;
    }
    let path = temporary_directory("stale-owner");
    let mut child = Command::new(
        std::env::current_exe().unwrap_or_else(|error| panic!("test executable: {error}")),
    )
    .arg("--exact")
    .arg("old_process_is_fenced_after_a_new_owner_takes_over")
    .arg("--nocapture")
    .env("BACKEND_STALE_OWNER_CHILD", &path)
    .spawn()
    .unwrap_or_else(|error| panic!("spawn stale owner child: {error}"));
    wait_for_file(&path.join("stale-owner-ready"), "stale owner readiness");

    // A hard process failure releases the kernel-held owner lease. Reclaim is
    // then an ordinary lock acquisition; no PID liveness guess or marker
    // rename is involved.
    child
        .kill()
        .unwrap_or_else(|error| panic!("kill crashed owner child: {error}"));
    assert!(!wait_child(child, "crashed owner child").success());
    let replacement =
        open_owner(&path).unwrap_or_else(|error| panic!("open replacement owner: {error}"));
    assert!(replacement.lease().epoch() >= 2);
    drop(replacement);
    fs::remove_dir_all(path).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn transaction_identity_is_deterministic_and_full_width() {
    let root = genesis()
        .unwrap_or_else(|error| panic!("transaction test genesis: {error}"))
        .root();
    let left = TransactionId::derive(9, root, [3; 32], 1);
    assert_eq!(left, TransactionId::derive(9, root, [3; 32], 1));
    assert_ne!(left, TransactionId::derive(9, root, [3; 32], 2));
    assert_ne!(left.as_bytes()[8..], [0; 24]);
}
