//! Owner-managed, permission-restricted local-cluster identity and trust config.
//!
//! This file is deliberately separate from runtime networking: enrollment is an explicit local
//! command, direct addresses are pinned out of band, and no discovery service is consulted.

use crate::cluster_embedding::{
    WorkerEmbeddingInstall, WorkerEmbeddingLimits, WorkerEmbeddingProvision, WorkerEmbeddingStatus,
    configured_resident_credit_bytes, inspect_worker_embedding_runtime,
    install_worker_embedding_runtime, remove_worker_embedding_runtime,
};
use crate::cluster_runtime::{
    ClusterCoordinator, ClusterExecutionPolicy, ClusterWorker, ClusterWorkerConfig,
    ClusterWorkerError, ClusterWorkerPolicy, PendingResultSummary, WorkerExecutionClass,
    WorkerExecutionGrant, abandon_pending_result, collect_worker_store_garbage,
    inspect_pending_results, reap_orphaned_workspace_snapshots,
};
use backend_engine::application::{
    LocalCompilerCapability, LocalCompilerCapabilityState, LocalCompilerClient, LocalCompilerHost,
};
use backend_engine::cluster_transport::{
    AssignmentScope, ClusterExecutionClass, ClusterInviteError, EndpointAddr, EndpointId,
    MAX_CONTROL_GRANT_PAGES, MAX_OFFER_CAPABILITIES, SecretKey,
};
use backend_store::FileStore;
use backend_version::{ContentId, ToolchainDomain};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

const CONFIG_MAGIC: &[u8; 8] = b"BKCWKID3";
const MAX_CONFIG_BYTES: usize = 512 * 1024;
const MAX_COORDINATORS: usize = 256;
const MAX_RECIPES: usize = 256;
const MAX_GRANTS: usize = 4096;
const MAX_USED_INVITES: usize = 4096;
const MAX_ADDRESSES: usize = 16;
const MAX_ADDRESS_TEXT: usize = 128;
#[cfg(unix)]
const CONFIG_FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const CONFIG_DIR_MODE: u32 = 0o700;

/// Durable worker identity, local trust policy, and one-time invite history.
#[derive(Debug)]
pub struct PersistedClusterConfig {
    /// Secret identity and policy used to bind the direct worker endpoint.
    pub worker: ClusterWorkerConfig,
    consumed_invites: BTreeSet<[u8; 32]>,
}

pub use backend_engine::cluster_transport::ScopedClusterInvite;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalCompilerBinding {
    profile: [u8; 2],
    toolchain: ContentId<ToolchainDomain>,
    environment: [u8; 32],
    target_platform: [u8; 32],
    local_authority_fingerprint: [u8; 32],
}

impl LocalCompilerBinding {
    fn from_capability(capability: LocalCompilerCapability) -> Result<Self, ClusterConfigError> {
        if capability.state() != LocalCompilerCapabilityState::Ready {
            return Err(ClusterConfigError::CapabilityUnavailable {
                profile: <[u8; 2]>::from(capability.profile()),
                state: Some(capability.state()),
            });
        }
        let profile = <[u8; 2]>::from(capability.profile());
        let raw_toolchain = capability.toolchain_identity().ok_or(
            ClusterConfigError::CapabilityEvidenceUnavailable {
                profile,
                evidence: "toolchain identity",
            },
        )?;
        if raw_toolchain == [0; 32] {
            return Err(ClusterConfigError::CapabilityEvidenceUnavailable {
                profile,
                evidence: "nonzero toolchain identity",
            });
        }
        Ok(Self {
            profile,
            toolchain: ContentId::<ToolchainDomain>::from_digest(raw_toolchain),
            environment: capability.environment_identity().ok_or(
                ClusterConfigError::CapabilityEvidenceUnavailable {
                    profile,
                    evidence: "execution environment identity",
                },
            )?,
            target_platform: capability.target_platform_identity().ok_or(
                ClusterConfigError::CapabilityEvidenceUnavailable {
                    profile,
                    evidence: "target platform identity",
                },
            )?,
            local_authority_fingerprint: capability.local_authority_fingerprint().ok_or(
                ClusterConfigError::CapabilityEvidenceUnavailable {
                    profile,
                    evidence: "local authority fingerprint",
                },
            )?,
        })
    }

    fn mismatched_invite_field(self, invite: &ScopedClusterInvite) -> Option<&'static str> {
        if self.profile != invite.profile() {
            return Some("profile");
        }
        let Ok(invite_toolchain) = ContentId::<ToolchainDomain>::try_from(invite.toolchain())
        else {
            return Some("canonical ToolchainDomain identity");
        };
        if self.toolchain != invite_toolchain {
            return Some("canonical ToolchainDomain identity");
        }
        if self.environment != invite.environment() {
            return Some("execution environment identity");
        }
        if self.target_platform != invite.target_platform() {
            return Some("target platform identity");
        }
        if self.local_authority_fingerprint == [0; 32] {
            return Some("nonzero local authority fingerprint");
        }
        None
    }
}

/// Error reading, validating, or atomically writing worker identity configuration.
#[derive(Debug, Error)]
pub enum ClusterConfigError {
    /// The supplied config or invitation is malformed, expired, or outside fixed limits.
    #[error("cluster config or invitation is invalid")]
    Invalid,
    /// An invitation belongs to another namespace or was imported before.
    #[error("cluster invitation is for another namespace or was already consumed")]
    InviteScopeOrReplay,
    /// The invitation fingerprint does not match its exact claims.
    #[error("cluster invitation fingerprint does not match its claims")]
    Fingerprint,
    /// The shared invite codec rejected a malformed or noncanonical invitation.
    #[error("cluster invitation is invalid: {0}")]
    Invite(#[from] ClusterInviteError),
    /// The requested coordinator is not currently trusted.
    #[error("coordinator identity is not in the local allowlist")]
    NotTrusted,
    /// Safe restrictive permissions cannot be enforced on this platform.
    #[error("permission-restricted cluster config is unsupported on this platform")]
    PermissionsUnavailable,
    /// Windows worker state must use an absolute local-drive UTF-8 path.
    #[cfg(windows)]
    #[error(
        "Windows cluster state requires an absolute UTF-8 local-drive path such as C:\\worker\\data; UNC, device, and relative paths are rejected"
    )]
    WindowsLocalDriveRequired,
    /// The invited profile is absent or has not reached exact local toolchain admission.
    #[error("worker compiler capability for profile {profile:?} is unavailable (state: {state:?})")]
    CapabilityUnavailable {
        /// Exact requested language-profile code.
        profile: [u8; 2],
        /// None means the worker has no capability row for this profile.
        state: Option<LocalCompilerCapabilityState>,
    },
    /// A ready capability omitted one of the local facts required for host execution.
    #[error("worker compiler capability for profile {profile:?} omitted {evidence}")]
    CapabilityEvidenceUnavailable {
        /// Exact requested language-profile code.
        profile: [u8; 2],
        /// Required local runtime fact that was absent.
        evidence: &'static str,
    },
    /// This worker does not implement the invited pure-parser execution class.
    #[error("this worker does not support the invited pure in-process parser execution class")]
    PureExecutionUnavailable,
    /// One or more independently admitted compiler identities differ from the invite.
    #[error("worker compiler capability does not match invited {field}")]
    ExecutionIdentityMismatch {
        /// First compiler-scope field that differs from the decoded invite.
        field: &'static str,
    },
    /// A filesystem operation failed.
    #[error("cluster config filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// A local compiler, FileStore, or cluster runtime failed.
    #[error("cluster worker runtime failed: {0}")]
    Runtime(String),
}

impl PersistedClusterConfig {
    /// Creates a fresh direct-worker identity with secure deny-by-default execution policy.
    pub fn create(
        bind_address: SocketAddr,
        namespace_id: [u8; 16],
        accepted_recipes: Vec<[u8; 32]>,
    ) -> Result<Self, ClusterConfigError> {
        let worker = ClusterWorkerConfig {
            bind_address,
            identity: SecretKey::generate(),
            coordinators: Vec::new(),
            policy: ClusterWorkerPolicy {
                namespace_id,
                accepted_recipes,
                execution_policy: ClusterExecutionPolicy::DenyUnconfined,
                max_output_bytes: 1024 * 1024 * 1024,
                cpu_millicores: 1_000,
                memory_bytes: 2 * 1024 * 1024 * 1024,
                max_input_objects: 16_384,
                max_input_bytes: 4 * 1024 * 1024 * 1024,
                max_capabilities: MAX_CONTROL_GRANT_PAGES as usize * MAX_OFFER_CAPABILITIES,
                max_grant_pages: MAX_CONTROL_GRANT_PAGES,
                io_timeout: Duration::from_secs(30),
            },
        };
        let config = Self {
            worker,
            consumed_invites: BTreeSet::new(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Loads a cold-start config from the owner-only file and validates all persisted claims.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ClusterConfigError> {
        let bytes = read_private_file(path.as_ref())?;
        let config = decode_config(&bytes)?;
        config.validate()?;
        Ok(config)
    }

    /// Opens the configured direct-only worker endpoint from this cold-reopened identity.
    pub async fn bind_worker(
        &self,
    ) -> Result<crate::cluster_runtime::ClusterWorker, crate::cluster_runtime::ClusterWorkerError>
    {
        crate::cluster_runtime::ClusterWorker::bind(self.worker.clone()).await
    }

    /// Atomically persists this identity, allowlist, grants, and consumed invite fingerprints.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), ClusterConfigError> {
        self.validate()?;
        let bytes = encode_config(self)?;
        write_private_atomic(path.as_ref(), &bytes)
    }

    /// Imports an exact, short-lived invite after its fingerprint was checked out of band.
    pub fn import_invite(
        &mut self,
        invite: ScopedClusterInvite,
        now_unix_ms: u64,
        capability: LocalCompilerCapability,
    ) -> Result<(), ClusterConfigError> {
        let binding = LocalCompilerBinding::from_capability(capability)?;
        self.import_invite_with_binding(invite, now_unix_ms, binding)
    }

    fn import_invite_with_binding(
        &mut self,
        invite: ScopedClusterInvite,
        now_unix_ms: u64,
        binding: LocalCompilerBinding,
    ) -> Result<(), ClusterConfigError> {
        let class = worker_execution_class(invite.execution_class());
        if class == WorkerExecutionClass::PureInProcessParser {
            return Err(ClusterConfigError::PureExecutionUnavailable);
        }
        if let Some(field) = binding.mismatched_invite_field(&invite) {
            return Err(ClusterConfigError::ExecutionIdentityMismatch { field });
        }
        if invite.namespace_id() != self.worker.policy.namespace_id
            || invite.expires_unix_ms() <= now_unix_ms
            || invite.expires_unix_ms().saturating_sub(now_unix_ms) > 24 * 60 * 60 * 1000
            || self.consumed_invites.contains(&invite.fingerprint())
        {
            return Err(ClusterConfigError::InviteScopeOrReplay);
        }
        let coordinator = self
            .worker
            .coordinators
            .iter_mut()
            .find(|coordinator| coordinator.identity == invite.coordinator());
        if let Some(coordinator) = coordinator {
            if coordinator.address
                != EndpointAddr::new(invite.coordinator()).with_ip_addr(invite.address())
            {
                return Err(ClusterConfigError::Invalid);
            }
        } else {
            self.worker.coordinators.push(ClusterCoordinator {
                identity: invite.coordinator(),
                address: EndpointAddr::new(invite.coordinator()).with_ip_addr(invite.address()),
            });
        }
        if !self
            .worker
            .policy
            .accepted_recipes
            .contains(&invite.recipe())
        {
            self.worker.policy.accepted_recipes.push(invite.recipe());
        }
        let mut grants = match std::mem::replace(
            &mut self.worker.policy.execution_policy,
            ClusterExecutionPolicy::DenyUnconfined,
        ) {
            ClusterExecutionPolicy::DenyUnconfined => Vec::new(),
            ClusterExecutionPolicy::Allowlisted(grants) => grants.into_vec(),
        };
        let grant = WorkerExecutionGrant {
            coordinator: invite.coordinator(),
            namespace_id: invite.namespace_id(),
            recipe: invite.recipe(),
            profile: invite.profile(),
            stage: invite.stage(),
            toolchain: binding.toolchain,
            environment: invite.environment(),
            target_platform: invite.target_platform(),
            local_authority_fingerprint: binding.local_authority_fingerprint,
            class,
        };
        if !grants.contains(&grant) {
            grants.push(grant);
        }
        self.worker.policy.execution_policy =
            ClusterExecutionPolicy::Allowlisted(grants.into_boxed_slice());
        self.consumed_invites.insert(invite.fingerprint());
        self.validate()?;
        Ok(())
    }

    /// Removes one coordinator and all invocation grants scoped to that peer.
    pub fn revoke_coordinator(
        &mut self,
        coordinator: EndpointId,
    ) -> Result<(), ClusterConfigError> {
        let prior_len = self.worker.coordinators.len();
        self.worker
            .coordinators
            .retain(|entry| entry.identity != coordinator);
        if prior_len == self.worker.coordinators.len() {
            return Err(ClusterConfigError::NotTrusted);
        }
        if let ClusterExecutionPolicy::Allowlisted(grants) = &self.worker.policy.execution_policy {
            let retained = grants
                .iter()
                .copied()
                .filter(|grant| grant.coordinator != coordinator)
                .collect::<Vec<_>>();
            self.worker.policy.execution_policy =
                ClusterExecutionPolicy::Allowlisted(retained.into_boxed_slice());
        }
        if self.worker.coordinators.is_empty() {
            self.worker.policy.execution_policy = ClusterExecutionPolicy::DenyUnconfined;
        }
        self.validate()?;
        Ok(())
    }

    /// Public endpoint fingerprint shown to a local operator before starting the worker.
    #[must_use]
    pub fn identity_fingerprint(&self) -> String {
        self.worker.identity.public().to_string()
    }

    fn validate(&self) -> Result<(), ClusterConfigError> {
        self.worker
            .policy
            .validate()
            .map_err(|_| ClusterConfigError::Invalid)?;
        if self.worker.coordinators.len() > MAX_COORDINATORS
            || self.worker.policy.accepted_recipes.len() > MAX_RECIPES
            || self.consumed_invites.len() > MAX_USED_INVITES
        {
            return Err(ClusterConfigError::Invalid);
        }
        let mut peers = BTreeSet::new();
        for coordinator in &self.worker.coordinators {
            if coordinator.identity != coordinator.address.id
                || coordinator.address.ip_addrs().next().is_none()
                || coordinator
                    .address
                    .ip_addrs()
                    .any(|address| address.port() == 0)
                || coordinator.address.relay_urls().next().is_some()
                || coordinator
                    .address
                    .addrs
                    .iter()
                    .any(|address| !address.is_ip())
                || !peers.insert(coordinator.identity)
            {
                return Err(ClusterConfigError::Invalid);
            }
        }
        if self.worker.coordinators.is_empty()
            && matches!(
                self.worker.policy.execution_policy,
                ClusterExecutionPolicy::Allowlisted(_)
            )
        {
            return Err(ClusterConfigError::Invalid);
        }
        if let ClusterExecutionPolicy::Allowlisted(grants) = &self.worker.policy.execution_policy {
            if grants.len() > MAX_GRANTS
                || grants
                    .iter()
                    .any(|grant| !peers.contains(&grant.coordinator))
            {
                return Err(ClusterConfigError::Invalid);
            }
        }
        Ok(())
    }
}

/// Runs local `backend-worker cluster` identity and enrollment commands.
///
/// Supported forms are `init`, `identity show`, `trust import|revoke`, `run`, `pending`,
/// and exact-scope `abandon`; no command uses public peer discovery.
pub fn run_cluster_cli(args: &[String]) -> Result<(), ClusterConfigError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(ClusterConfigError::Invalid);
    };
    match command {
        "--help" | "help" => {
            println!(
                "usage: backend-worker cluster init --config ABSOLUTE_PATH --bind IP:PORT --namespace HEX --recipe HEX"
            );
            println!("       backend-worker cluster identity show --config ABSOLUTE_PATH");
            println!(
                "       backend-worker cluster trust import --config ABSOLUTE_PATH --data-dir ABSOLUTE_PATH --invite TOKEN --fingerprint HEX"
            );
            println!(
                "       backend-worker cluster trust revoke --config ABSOLUTE_PATH --coordinator ENDPOINT_ID"
            );
            println!(
                "       backend-worker cluster run --config ABSOLUTE_PATH --data-dir ABSOLUTE_PATH"
            );
            println!(
                "       backend-worker cluster embedding install --data-dir ABSOLUTE_PATH --program ABSOLUTE_PATH --model-file ABSOLUTE_PATH --tokenizer-file ABSOLUTE_PATH --dimension N --normalization none|l2 --options-digest HEX [--requirement optional|required] [--arg ARG] [--dependency ABSOLUTE_PATH] [--env KEY=VALUE]"
            );
            println!(
                "       backend-worker cluster embedding show|remove --data-dir ABSOLUTE_PATH"
            );
            println!(
                "       backend-worker cluster pending --config ABSOLUTE_PATH --data-dir ABSOLUTE_PATH"
            );
            println!(
                "       backend-worker cluster abandon --config ABSOLUTE_PATH --data-dir ABSOLUTE_PATH --namespace HEX --work HEX --attempt N --fence HEX --closure HEX"
            );
            println!("compare the full invite fingerprint over a trusted channel before import");
            println!(
                "pending results are retained until owner ACK or an exact audited local abandon"
            );
            Ok(())
        }
        "init" => {
            let options = CliOptions::parse(&args[1..])?;
            options.check_keys(&["--config", "--bind", "--namespace", "--recipe"])?;
            let path = options.path("--config")?;
            if path.exists() {
                return Err(ClusterConfigError::Invalid);
            }
            let bind = options
                .value("--bind")?
                .parse()
                .map_err(|_| ClusterConfigError::Invalid)?;
            let namespace = fixed_hex::<16>(options.value("--namespace")?)?;
            let recipe = fixed_hex::<32>(options.value("--recipe")?)?;
            let config = PersistedClusterConfig::create(bind, namespace, vec![recipe])?;
            config.save_atomic(path)?;
            println!(
                "worker identity fingerprint: {}",
                config.identity_fingerprint()
            );
            println!("transport: direct-only; relays and public discovery disabled");
            println!("scheduler capacity credits: 1000 millicores, 2147483648 memory bytes");
            println!("execution: deny by default; import a scoped invite to enable exact work");
            Ok(())
        }
        "identity" if args.get(1).is_some_and(|arg| arg == "show") => {
            let options = CliOptions::parse(&args[2..])?;
            options.check_keys(&["--config"])?;
            let config = PersistedClusterConfig::load(options.path("--config")?)?;
            println!(
                "worker identity fingerprint: {}",
                config.identity_fingerprint()
            );
            println!("direct endpoint: {}", config.worker.bind_address);
            println!("transport: direct-only; relays and public discovery disabled");
            println!("trusted coordinators: {}", config.worker.coordinators.len());
            println!(
                "scheduler capacity credits: {} millicores, {} memory bytes",
                config.worker.policy.cpu_millicores, config.worker.policy.memory_bytes
            );
            println!(
                "execution grants: {}",
                match &config.worker.policy.execution_policy {
                    ClusterExecutionPolicy::DenyUnconfined => 0,
                    ClusterExecutionPolicy::Allowlisted(grants) => grants.len(),
                }
            );
            Ok(())
        }
        "trust" if args.get(1).is_some_and(|arg| arg == "import") => {
            let options = CliOptions::parse(&args[2..])?;
            options.check_keys(&["--config", "--data-dir", "--invite", "--fingerprint"])?;
            let path = options.path("--config")?;
            let data_dir = options.path("--data-dir")?;
            ensure_private_data_directory(data_dir)?;
            let mut config = PersistedClusterConfig::load(path)?;
            let invite = ScopedClusterInvite::decode_token(options.value("--invite")?)?;
            if invite.fingerprint_hex() != options.value("--fingerprint")? {
                return Err(ClusterConfigError::Fingerprint);
            }
            let now = backend_engine::cluster_transport::now_unix_ms()?;
            let class = worker_execution_class(invite.execution_class());
            let coordinator = invite.coordinator();
            let recipe = invite.recipe();
            let compiler = LocalCompilerHost::production_at(data_dir.join("compiler-runtime"))
                .open()
                .map_err(runtime_error)?;
            let capability = local_compiler_capability_for_profile(&compiler, invite.profile())?;
            config.import_invite(invite, now, capability)?;
            config.save_atomic(path)?;
            println!("scoped coordinator {coordinator} invite imported and persisted");
            println!("recipe: {}", hex(&recipe));
            println!("execution class: {class:?}");
            if class == WorkerExecutionClass::TrustedCoordinatorHostExecution {
                println!(
                    "warning: this grant allows coordinator-controlled tools to run with worker host privileges"
                );
            }
            Ok(())
        }
        "trust" if args.get(1).is_some_and(|arg| arg == "revoke") => {
            let options = CliOptions::parse(&args[2..])?;
            options.check_keys(&["--config", "--coordinator"])?;
            let path = options.path("--config")?;
            let mut config = PersistedClusterConfig::load(path)?;
            let coordinator = endpoint_id(fixed_hex::<32>(options.value("--coordinator")?)?)?;
            config.revoke_coordinator(coordinator)?;
            config.save_atomic(path)?;
            println!("coordinator trust and its execution grants were revoked");
            Ok(())
        }
        "embedding" if args.get(1).is_some_and(|arg| arg == "install") => {
            install_embedding_from_cli(&args[2..])
        }
        "embedding" if args.get(1).is_some_and(|arg| arg == "show") => {
            let options = CliOptions::parse(&args[2..])?;
            options.check_keys(&["--data-dir"])?;
            let data_dir = options.path("--data-dir")?;
            ensure_private_data_directory(data_dir)?;
            match inspect_worker_embedding_runtime(data_dir.join("embedding.config"))
                .map_err(runtime_error)?
            {
                None => println!("embedding runtime: not configured"),
                Some(summary) => {
                    println!(
                        "embedding runtime: configured; activation is checked on worker start"
                    );
                    println!("requirement: {:?}", summary.requirement);
                    println!("recipe: {}", hex(&summary.spec.recipe_identity()));
                    println!("model: {}", hex(&summary.spec.model()));
                    println!("model version: {}", hex(&summary.spec.model_version()));
                    println!("tokenizer: {}", hex(&summary.spec.tokenizer()));
                    println!("executable closure: {}", hex(&summary.spec.executable()));
                    println!("dimension: {}", summary.spec.dimension());
                    println!("normalization: {:?}", summary.spec.normalization());
                    println!("maximum text bytes: {}", summary.spec.maximum_text_bytes());
                    println!(
                        "resident memory credits: {} bytes",
                        summary.limits.resident_credit_bytes
                    );
                    println!(
                        "OS memory limit: {}",
                        summary.limits.memory_bytes.map_or_else(
                            || "not requested".to_owned(),
                            |bytes| format!("{bytes} bytes")
                        )
                    );
                }
            }
            Ok(())
        }
        "embedding" if args.get(1).is_some_and(|arg| arg == "remove") => {
            let options = CliOptions::parse(&args[2..])?;
            options.check_keys(&["--data-dir"])?;
            let data_dir = options.path("--data-dir")?;
            ensure_private_data_directory(data_dir)?;
            remove_worker_embedding_runtime(data_dir.join("embedding.config"))
                .map_err(runtime_error)?;
            println!(
                "embedding runtime config removed; content-addressed artifacts remain in the private data directory"
            );
            Ok(())
        }
        "run" => {
            let options = CliOptions::parse(&args[1..])?;
            options.check_keys(&["--config", "--data-dir"])?;
            let config = PersistedClusterConfig::load(options.path("--config")?)?;
            let data_dir = options.path("--data-dir")?.to_path_buf();
            ensure_private_data_directory(&data_dir)?;
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| ClusterConfigError::Runtime(error.to_string()))?;
            runtime.block_on(run_cluster_worker(config, data_dir))
        }
        "pending" => {
            let options = CliOptions::parse(&args[1..])?;
            options.check_keys(&["--config", "--data-dir"])?;
            let config = PersistedClusterConfig::load(options.path("--config")?)?;
            let data_dir = options.path("--data-dir")?;
            ensure_private_data_directory(data_dir)?;
            let store = open_result_store(data_dir, &config)?;
            let pending =
                inspect_pending_results(&store, &config.worker.policy).map_err(runtime_error)?;
            print_pending_results(&pending);
            Ok(())
        }
        "abandon" => {
            let options = CliOptions::parse(&args[1..])?;
            options.check_keys(&[
                "--config",
                "--data-dir",
                "--namespace",
                "--work",
                "--attempt",
                "--fence",
                "--closure",
            ])?;
            let config = PersistedClusterConfig::load(options.path("--config")?)?;
            let data_dir = options.path("--data-dir")?;
            ensure_private_data_directory(data_dir)?;
            let namespace_id = fixed_hex::<16>(options.value("--namespace")?)?;
            let work_id = fixed_hex::<16>(options.value("--work")?)?;
            let attempt = options
                .value("--attempt")?
                .parse::<u64>()
                .map_err(|_| ClusterConfigError::Invalid)?;
            let fence = fixed_hex::<32>(options.value("--fence")?)?;
            let closure_id = fixed_hex::<32>(options.value("--closure")?)?;
            if attempt == 0 {
                return Err(ClusterConfigError::Invalid);
            }
            let scope = AssignmentScope::new(namespace_id, work_id, attempt, fence)
                .map_err(|_| ClusterConfigError::Invalid)?;
            let store = open_result_store(data_dir, &config)?;
            abandon_pending_result(&store, &config.worker.policy, scope, closure_id)
                .map_err(runtime_error)?;
            println!(
                "exact result transfer abandoned; stored objects remain subject to normal FileStore GC"
            );
            Ok(())
        }
        _ => Err(ClusterConfigError::Invalid),
    }
}

fn install_embedding_from_cli(args: &[String]) -> Result<(), ClusterConfigError> {
    let options = CliOptions::parse(args)?;
    options.check_keys(&[
        "--data-dir",
        "--program",
        "--model-file",
        "--tokenizer-file",
        "--dimension",
        "--normalization",
        "--max-text-bytes",
        "--options-digest",
        "--requirement",
        "--stdout-bytes",
        "--stderr-bytes",
        "--output-bytes",
        "--input-bytes",
        "--wall-time-ms",
        "--workspace-bytes",
        "--process-count",
        "--memory-bytes",
        "--cpu-time-ms",
        "--resident-credit-bytes",
        "--arg",
        "--dependency",
        "--env",
    ])?;
    let data_dir = options.path("--data-dir")?;
    ensure_private_data_directory(data_dir)?;
    let model_bytes = read_embedding_artifact(
        options.path("--model-file")?,
        backend_compile::MAX_EMBEDDING_MODEL_BYTES,
    )?;
    let tokenizer_bytes = read_embedding_artifact(
        options.path("--tokenizer-file")?,
        backend_compile::MAX_EMBEDDING_TOKENIZER_BYTES,
    )?;
    let dimension = NonZeroU16::new(
        options
            .value("--dimension")?
            .parse()
            .map_err(|_| ClusterConfigError::Invalid)?,
    )
    .ok_or(ClusterConfigError::Invalid)?;
    let normalization = match options.value("--normalization")? {
        "none" => backend_compile::EmbeddingNormalization::None,
        "l2" => backend_compile::EmbeddingNormalization::L2,
        _ => return Err(ClusterConfigError::Invalid),
    };
    let maximum_text_bytes = NonZeroU32::new(
        options
            .optional_value("--max-text-bytes")
            .unwrap_or("8192")
            .parse()
            .map_err(|_| ClusterConfigError::Invalid)?,
    )
    .ok_or(ClusterConfigError::Invalid)?;
    let requirement = match options
        .optional_value("--requirement")
        .unwrap_or("optional")
    {
        "optional" => backend_engine::application::EmbeddingRequirement::Optional,
        "required" => backend_engine::application::EmbeddingRequirement::Required,
        _ => return Err(ClusterConfigError::Invalid),
    };
    let arguments = options.values("--arg").map(str::to_owned).collect();
    let dependencies = options.values("--dependency").map(PathBuf::from).collect();
    let environment = options
        .values("--env")
        .map(|entry| {
            let (key, value) = entry.split_once('=').ok_or(ClusterConfigError::Invalid)?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect::<Result<Vec<_>, ClusterConfigError>>()?;

    let model_length = u64::try_from(model_bytes.len()).map_err(|_| ClusterConfigError::Invalid)?;
    let tokenizer_length =
        u64::try_from(tokenizer_bytes.len()).map_err(|_| ClusterConfigError::Invalid)?;
    let memory_bytes = options
        .optional_value("--memory-bytes")
        .map(parse_u64)
        .transpose()?;
    let minimum_credit = model_length
        .checked_add(tokenizer_length)
        .and_then(|value| value.checked_add(16 * 1024 * 1024))
        .and_then(|value| value.checked_add(memory_bytes.unwrap_or(0)))
        .ok_or(ClusterConfigError::Invalid)?;
    let limits = WorkerEmbeddingLimits {
        stdout_bytes: option_u64(&options, "--stdout-bytes", 1024 * 1024)?,
        stderr_bytes: option_u64(&options, "--stderr-bytes", 1024 * 1024)?,
        output_bytes: option_u64(&options, "--output-bytes", 2 * 1024 * 1024)?,
        input_bytes: option_u64(&options, "--input-bytes", 1024 * 1024)?,
        wall_time_ms: option_u64(&options, "--wall-time-ms", 30_000)?,
        workspace_bytes: option_u64(&options, "--workspace-bytes", 512 * 1024 * 1024)?,
        process_count: option_u64(&options, "--process-count", 1)?,
        memory_bytes,
        cpu_time_ms: options
            .optional_value("--cpu-time-ms")
            .map(parse_u64)
            .transpose()?,
        resident_credit_bytes: option_u64(
            &options,
            "--resident-credit-bytes",
            minimum_credit.max(512 * 1024 * 1024),
        )?,
    };
    let spec = install_worker_embedding_runtime(
        data_dir.join("embedding.config"),
        WorkerEmbeddingInstall {
            requirement,
            program: options.path("--program")?.to_path_buf(),
            arguments,
            dependencies,
            environment,
            limits,
            model_bytes,
            tokenizer_bytes,
            dimension,
            normalization,
            maximum_text_bytes,
            options_digest: fixed_hex::<32>(options.value("--options-digest")?)?,
        },
    )
    .map_err(runtime_error)?;
    println!("worker embedding runtime persisted privately");
    println!("recipe: {}", hex(&spec.recipe_identity()));
    println!("model: {}", hex(&spec.model()));
    println!("tokenizer: {}", hex(&spec.tokenizer()));
    println!(
        "dimension: {}; normalization: {:?}; requirement: {:?}",
        spec.dimension(),
        spec.normalization(),
        requirement
    );
    println!("activation runs and verifies the configured process during `cluster run`");
    Ok(())
}

fn read_embedding_artifact(path: &Path, maximum: usize) -> Result<Vec<u8>, ClusterConfigError> {
    let path_metadata = fs::symlink_metadata(path)?;
    if !path_metadata.is_file()
        || path_metadata.file_type().is_symlink()
        || path_metadata.len() == 0
        || path_metadata.len() > maximum as u64
    {
        return Err(ClusterConfigError::Invalid);
    }
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != path_metadata.len() {
        return Err(ClusterConfigError::Invalid);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(
            usize::try_from(metadata.len()).map_err(|_| ClusterConfigError::Invalid)?,
        )
        .map_err(|_| ClusterConfigError::Invalid)?;
    file.by_ref()
        .take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(ClusterConfigError::Invalid);
    }
    Ok(bytes)
}

fn option_u64(
    options: &CliOptions<'_>,
    key: &str,
    default: u64,
) -> Result<u64, ClusterConfigError> {
    options.optional_value(key).map_or(Ok(default), parse_u64)
}

fn parse_u64(value: &str) -> Result<u64, ClusterConfigError> {
    value.parse().map_err(|_| ClusterConfigError::Invalid)
}

async fn run_cluster_worker(
    config: PersistedClusterConfig,
    data_dir: PathBuf,
) -> Result<(), ClusterConfigError> {
    let store = open_result_store(&data_dir, &config)?;
    let input_store = FileStore::open(data_dir.join("input-cas"), store_pack_limit(&config)?)
        .map_err(runtime_error)?;
    let checkpoint_root = data_dir.join("input-checkpoints");
    let snapshot_root = data_dir.join("workspaces");
    let outboard_root = data_dir.join("outboards");
    for path in [&checkpoint_root, &snapshot_root, &outboard_root] {
        ensure_private_data_directory(path)?;
    }
    let embedding_config = data_dir.join("embedding.config");
    let mut runtime_worker_config = config.worker.clone();
    let embedding_reservation = match configured_resident_credit_bytes(&embedding_config) {
        Ok(bytes) => bytes,
        Err(_) => {
            eprintln!(
                "embedding config could not be checked before listener startup; worker capacity is withheld until config validation"
            );
            config.worker.policy.memory_bytes.saturating_sub(1)
        }
    };
    runtime_worker_config.policy.memory_bytes = runtime_worker_config
        .policy
        .memory_bytes
        .saturating_sub(embedding_reservation)
        .max(1);
    let worker = ClusterWorker::bind(runtime_worker_config)
        .await
        .map_err(runtime_error)?;
    println!(
        "worker identity fingerprint: {}",
        hex(worker.identity().as_bytes())
    );
    println!("direct endpoint: {:?}", worker.address());
    println!("transport: direct-only; relays and public discovery disabled");
    println!(
        "execution: exact persisted host-trusted grants only; workspace snapshot is not an OS sandbox"
    );

    // Reconcile ACK-governed durable results before opening the compiler runtime. A retained
    // closure must remain replayable even when this host no longer has the compiler toolchain.
    loop {
        match worker.recover_after_restart(&store).await {
            Ok(pending) if pending.is_empty() => break,
            Ok(pending) => {
                let mut retry = false;
                for result in &pending {
                    match worker
                        .resume_retained_result_until_ack(result, &outboard_root)
                        .await
                    {
                        Ok(_) => {}
                        Err(ClusterWorkerError::Timeout | ClusterWorkerError::Operation(_)) => {
                            retry = true;
                            break;
                        }
                        Err(error) => return Err(runtime_error(error)),
                    }
                }
                if retry {
                    eprintln!(
                        "retained result delivery is offline; preserving durable closure and retrying"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
            Err(ClusterWorkerError::Timeout | ClusterWorkerError::Operation(_)) => {
                eprintln!("cluster recovery is offline; preserving journals and retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => return Err(runtime_error(error)),
        }
    }

    let reaped = reap_orphaned_workspace_snapshots(&snapshot_root).map_err(runtime_error)?;
    if reaped > 0 {
        eprintln!("removed {reaped} abandoned immutable workspace snapshot(s)");
    }
    collect_worker_store_garbage(&input_store, &store, &config.worker.policy)
        .map_err(runtime_error)?;

    let embedding = WorkerEmbeddingProvision::open(&embedding_config).map_err(runtime_error)?;
    match embedding.status() {
        WorkerEmbeddingStatus::NotConfigured => println!("embedding runtime: not configured"),
        WorkerEmbeddingStatus::Available { recipe } => {
            println!("embedding runtime active: recipe={}", hex(&recipe));
        }
        WorkerEmbeddingStatus::Unavailable(reason) => {
            eprintln!(
                "embedding runtime unavailable: {reason:?}; requirement={:?}",
                embedding.requirement()
            );
        }
    }
    let compiler_host = LocalCompilerHost::production_at(data_dir.join("compiler-runtime"));
    let compiler =
        match embedding.provisioning_failure() {
            Some(cause) => compiler_host
                .open_with_embedding_provisioning_failure(cause, embedding.requirement()),
            None => compiler_host
                .open_with_embedding_runtime(embedding.runtime(), embedding.requirement()),
        }
        .map_err(runtime_error)?;
    let compiler = Arc::new(compiler);
    loop {
        match worker
            .run_one_assignment_with_diagnostics(
                Arc::clone(&compiler),
                &input_store,
                &checkpoint_root,
                &snapshot_root,
                &store,
                &outboard_root,
            )
            .await
        {
            Ok((disposition, diagnostics)) => println!(
                "result owner ACK: {disposition:?}; Bao ranges={} bytes={} p50={}ms p95={}ms peak_rss_bytes={}",
                diagnostics.artifact_ranges,
                diagnostics.bao_stream_bytes,
                diagnostics.range_p50_ms,
                diagnostics.range_p95_ms,
                diagnostics
                    .peak_rss_bytes
                    .map_or_else(|| "unavailable".to_owned(), |rss| rss.to_string()),
            ),
            Err(error @ ClusterWorkerError::Timeout)
            | Err(error @ ClusterWorkerError::Cancelled)
            | Err(error @ ClusterWorkerError::OfferDeclined)
            | Err(error @ ClusterWorkerError::InputRejected(_)) => {
                eprintln!("cluster assignment ended: {error}");
            }
            Err(ClusterWorkerError::Operation(_)) => {
                eprintln!(
                    "cluster operation failed; retained transfers will be retried after backoff"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => {
                eprintln!("cluster worker stopped: {error}");
                return Err(runtime_error(error));
            }
        }
        collect_worker_store_garbage(&input_store, &store, &config.worker.policy)
            .map_err(runtime_error)?;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn open_result_store(
    data_dir: &Path,
    config: &PersistedClusterConfig,
) -> Result<FileStore, ClusterConfigError> {
    FileStore::open(data_dir.join("result-cas"), store_pack_limit(config)?).map_err(runtime_error)
}

fn local_compiler_capability_for_profile(
    compiler: &LocalCompilerClient,
    profile: [u8; 2],
) -> Result<LocalCompilerCapability, ClusterConfigError> {
    let deadline = Instant::now().checked_add(Duration::from_secs(6)).ok_or(
        ClusterConfigError::CapabilityUnavailable {
            profile,
            state: None,
        },
    )?;
    loop {
        let capabilities = compiler.capabilities();
        let capability = capabilities
            .as_slice()
            .iter()
            .copied()
            .find(|capability| <[u8; 2]>::from(capability.profile()) == profile)
            .ok_or(ClusterConfigError::CapabilityUnavailable {
                profile,
                state: None,
            })?;
        match capability.state() {
            LocalCompilerCapabilityState::Ready => return Ok(capability),
            LocalCompilerCapabilityState::Probing if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            state => {
                return Err(ClusterConfigError::CapabilityUnavailable {
                    profile,
                    state: Some(state),
                });
            }
        }
    }
}

fn store_pack_limit(config: &PersistedClusterConfig) -> Result<usize, ClusterConfigError> {
    let bound = config
        .worker
        .policy
        .max_input_bytes
        .max(config.worker.policy.max_output_bytes)
        .checked_add(64 * 1024 * 1024)
        .ok_or(ClusterConfigError::Invalid)?;
    usize::try_from(bound).map_err(|_| ClusterConfigError::Invalid)
}

fn ensure_private_data_directory(path: &Path) -> Result<(), ClusterConfigError> {
    #[cfg(windows)]
    validate_windows_local_drive_path(path)?;
    if !path.is_absolute() {
        return Err(ClusterConfigError::Invalid);
    }
    #[cfg(windows)]
    {
        validate_windows_path_chain(path)?;
    }
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ClusterConfigError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(CONFIG_DIR_MODE);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(windows)]
    {
        if backend_platform::win32::security::is_endpoint_metadata(&metadata) {
            return Err(ClusterConfigError::Invalid);
        }
        backend_platform::win32::security::restrict_to_current_user(path)?;
        let restricted = fs::symlink_metadata(path)?;
        if !restricted.is_dir()
            || restricted.file_type().is_symlink()
            || backend_platform::win32::security::is_endpoint_metadata(&restricted)
        {
            return Err(ClusterConfigError::Invalid);
        }
    }
    #[cfg(not(any(unix, windows)))]
    return Err(ClusterConfigError::PermissionsUnavailable);
    Ok(())
}

fn print_pending_results(pending: &[PendingResultSummary]) {
    if pending.is_empty() {
        println!("no retained result closures are awaiting owner ACK");
        return;
    }
    for item in pending {
        println!(
            "namespace={} work={} attempt={} fence={} coordinator={} input_closure={} manifest={} result_closure={} objects={} bytes={}",
            hex(&item.scope.namespace_id),
            hex(&item.scope.work_id),
            item.scope.attempt,
            hex(&item.scope.fence),
            hex(item.coordinator.as_bytes()),
            hex(&item.input_closure_id),
            hex(&item.input_manifest_object_id),
            hex(&item.result_closure_id),
            item.object_count,
            item.payload_bytes,
        );
    }
}

fn runtime_error(error: impl std::fmt::Debug) -> ClusterConfigError {
    ClusterConfigError::Runtime(format!("{error:?}"))
}

struct CliOptions<'a> {
    entries: Vec<(&'a str, &'a str)>,
}

impl<'a> CliOptions<'a> {
    fn parse(args: &'a [String]) -> Result<Self, ClusterConfigError> {
        let mut entries = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let key = args[index].as_str();
            let value = args.get(index + 1).ok_or(ClusterConfigError::Invalid)?;
            let repeatable = matches!(key, "--arg" | "--dependency" | "--env");
            if !key.starts_with("--")
                || (!repeatable && entries.iter().any(|(prior, _)| *prior == key))
            {
                return Err(ClusterConfigError::Invalid);
            }
            entries.push((key, value.as_str()));
            index += 2;
        }
        Ok(Self { entries })
    }

    fn value(&self, key: &str) -> Result<&'a str, ClusterConfigError> {
        self.entries
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(*value))
            .ok_or(ClusterConfigError::Invalid)
    }

    fn path(&self, key: &str) -> Result<&'a Path, ClusterConfigError> {
        let path = Path::new(self.value(key)?);
        if !path.is_absolute() {
            return Err(ClusterConfigError::Invalid);
        }
        Ok(path)
    }

    fn optional_value(&self, key: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find_map(|(name, value)| (*name == key).then_some(*value))
    }

    fn values<'b>(&'b self, key: &'b str) -> impl Iterator<Item = &'a str> + 'b {
        self.entries
            .iter()
            .filter_map(move |(name, value)| (*name == key).then_some(*value))
    }

    fn check_keys(&self, allowed: &[&str]) -> Result<(), ClusterConfigError> {
        if self
            .entries
            .iter()
            .any(|(key, _)| !allowed.iter().any(|expected| key == expected))
        {
            return Err(ClusterConfigError::Invalid);
        }
        Ok(())
    }
}

fn encode_config(config: &PersistedClusterConfig) -> Result<Vec<u8>, ClusterConfigError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(CONFIG_MAGIC);
    bytes.extend_from_slice(&config.worker.identity.to_bytes());
    put_string(&mut bytes, &config.worker.bind_address.to_string())?;
    bytes.extend_from_slice(&config.worker.policy.namespace_id);
    put_count(
        &mut bytes,
        config.worker.policy.accepted_recipes.len(),
        MAX_RECIPES,
    )?;
    for recipe in &config.worker.policy.accepted_recipes {
        bytes.extend_from_slice(recipe);
    }
    bytes.extend_from_slice(&config.worker.policy.max_output_bytes.to_be_bytes());
    bytes.extend_from_slice(&config.worker.policy.cpu_millicores.to_be_bytes());
    bytes.extend_from_slice(&config.worker.policy.memory_bytes.to_be_bytes());
    put_u32(&mut bytes, config.worker.policy.max_input_objects)?;
    bytes.extend_from_slice(&config.worker.policy.max_input_bytes.to_be_bytes());
    put_u32(&mut bytes, config.worker.policy.max_capabilities)?;
    bytes.extend_from_slice(&config.worker.policy.max_grant_pages.to_be_bytes());
    let timeout = u64::try_from(config.worker.policy.io_timeout.as_millis())
        .map_err(|_| ClusterConfigError::Invalid)?;
    bytes.extend_from_slice(&timeout.to_be_bytes());
    match &config.worker.policy.execution_policy {
        ClusterExecutionPolicy::DenyUnconfined => bytes.push(0),
        ClusterExecutionPolicy::Allowlisted(grants) => {
            bytes.push(1);
            put_count(&mut bytes, grants.len(), MAX_GRANTS)?;
            for grant in grants.iter() {
                bytes.extend_from_slice(grant.coordinator.as_bytes());
                bytes.extend_from_slice(&grant.namespace_id);
                bytes.extend_from_slice(&grant.recipe);
                bytes.extend_from_slice(&grant.profile);
                bytes.push(grant.stage);
                bytes.extend_from_slice(grant.toolchain.as_ref());
                bytes.extend_from_slice(&grant.environment);
                bytes.extend_from_slice(&grant.target_platform);
                bytes.extend_from_slice(&grant.local_authority_fingerprint);
                bytes.push(match grant.class {
                    WorkerExecutionClass::PureInProcessParser => 1,
                    WorkerExecutionClass::TrustedCoordinatorHostExecution => 2,
                });
            }
        }
    }
    put_count(
        &mut bytes,
        config.worker.coordinators.len(),
        MAX_COORDINATORS,
    )?;
    for coordinator in &config.worker.coordinators {
        bytes.extend_from_slice(coordinator.identity.as_bytes());
        let addresses = coordinator.address.ip_addrs().collect::<Vec<_>>();
        put_count(&mut bytes, addresses.len(), MAX_ADDRESSES)?;
        for address in addresses {
            put_string(&mut bytes, &address.to_string())?;
        }
    }
    put_count(&mut bytes, config.consumed_invites.len(), MAX_USED_INVITES)?;
    for fingerprint in &config.consumed_invites {
        bytes.extend_from_slice(fingerprint);
    }
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ClusterConfigError::Invalid);
    }
    Ok(bytes)
}

fn decode_config(bytes: &[u8]) -> Result<PersistedClusterConfig, ClusterConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ClusterConfigError::Invalid);
    }
    let mut reader = Reader::new(bytes);
    if reader.take(8)? != CONFIG_MAGIC {
        return Err(ClusterConfigError::Invalid);
    }
    let identity = SecretKey::from_bytes(&reader.array()?);
    let bind_address = reader
        .string(MAX_ADDRESS_TEXT)?
        .parse()
        .map_err(|_| ClusterConfigError::Invalid)?;
    let namespace_id = reader.array()?;
    let recipe_count = reader.count(MAX_RECIPES)?;
    let mut accepted_recipes = Vec::with_capacity(recipe_count);
    for _ in 0..recipe_count {
        accepted_recipes.push(reader.array()?);
    }
    let max_output_bytes = reader.u64()?;
    let cpu_millicores = reader.u32()?;
    let memory_bytes = reader.u64()?;
    let max_input_objects = reader.u32()? as usize;
    let max_input_bytes = reader.u64()?;
    let max_capabilities = reader.u32()? as usize;
    let max_grant_pages = reader.u32()?;
    let io_timeout = Duration::from_millis(reader.u64()?);
    let execution_policy = match reader.u8()? {
        0 => ClusterExecutionPolicy::DenyUnconfined,
        1 => {
            let grant_count = reader.count(MAX_GRANTS)?;
            let mut grants = Vec::with_capacity(grant_count);
            for _ in 0..grant_count {
                let coordinator = endpoint_id(reader.array()?)?;
                let namespace_id = reader.array()?;
                let recipe = reader.array()?;
                let profile = reader.array()?;
                let stage = reader.u8()?;
                let toolchain = ContentId::<ToolchainDomain>::try_from(reader.array()?)
                    .map_err(|_| ClusterConfigError::Invalid)?;
                let environment = reader.array()?;
                let target_platform = reader.array()?;
                let local_authority_fingerprint = reader.array()?;
                let class = match reader.u8()? {
                    1 => WorkerExecutionClass::PureInProcessParser,
                    2 => WorkerExecutionClass::TrustedCoordinatorHostExecution,
                    _ => return Err(ClusterConfigError::Invalid),
                };
                grants.push(WorkerExecutionGrant {
                    coordinator,
                    namespace_id,
                    recipe,
                    profile,
                    stage,
                    toolchain,
                    environment,
                    target_platform,
                    local_authority_fingerprint,
                    class,
                });
            }
            ClusterExecutionPolicy::Allowlisted(grants.into_boxed_slice())
        }
        _ => return Err(ClusterConfigError::Invalid),
    };
    let coordinator_count = reader.count(MAX_COORDINATORS)?;
    let mut coordinators = Vec::with_capacity(coordinator_count);
    for _ in 0..coordinator_count {
        let identity = endpoint_id(reader.array()?)?;
        let address_count = reader.count(MAX_ADDRESSES)?;
        if address_count == 0 {
            return Err(ClusterConfigError::Invalid);
        }
        let mut address = EndpointAddr::new(identity);
        for _ in 0..address_count {
            let ip = reader
                .string(MAX_ADDRESS_TEXT)?
                .parse()
                .map_err(|_| ClusterConfigError::Invalid)?;
            address = address.with_ip_addr(ip);
        }
        coordinators.push(ClusterCoordinator { identity, address });
    }
    let consumed_count = reader.count(MAX_USED_INVITES)?;
    let mut consumed_invites = BTreeSet::new();
    for _ in 0..consumed_count {
        if !consumed_invites.insert(reader.array()?) {
            return Err(ClusterConfigError::Invalid);
        }
    }
    reader.finish()?;
    Ok(PersistedClusterConfig {
        worker: ClusterWorkerConfig {
            bind_address,
            identity,
            coordinators,
            policy: ClusterWorkerPolicy {
                namespace_id,
                accepted_recipes,
                execution_policy,
                max_output_bytes,
                cpu_millicores,
                memory_bytes,
                max_input_objects,
                max_input_bytes,
                max_capabilities,
                max_grant_pages,
                io_timeout,
            },
        },
        consumed_invites,
    })
}

fn read_private_file(path: &Path) -> Result<Vec<u8>, ClusterConfigError> {
    validate_private_parent(path)?;
    let mut file = backend_platform::durable::open_private_read(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ClusterConfigError::Invalid);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ClusterConfigError::Invalid);
    }
    Ok(bytes)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), ClusterConfigError> {
    validate_private_parent(path)?;
    backend_platform::durable::write_private_atomic(path, bytes)?;
    Ok(())
}

fn validate_private_parent(path: &Path) -> Result<(), ClusterConfigError> {
    #[cfg(windows)]
    validate_windows_local_drive_path(path)?;
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(ClusterConfigError::Invalid);
    }
    let parent = path.parent().ok_or(ClusterConfigError::Invalid)?;
    let existed = parent.exists();
    #[cfg(windows)]
    {
        validate_windows_path_chain(parent)?;
    }
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ClusterConfigError::Invalid);
        }
        if existed {
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(ClusterConfigError::Invalid);
            }
        } else {
            fs::set_permissions(parent, fs::Permissions::from_mode(CONFIG_DIR_MODE))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = existed;
        validate_windows_path_chain(parent)?;
        ensure_private_data_directory(parent)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = existed;
        Err(ClusterConfigError::PermissionsUnavailable)
    }
}

#[cfg(windows)]
fn validate_windows_local_drive_path(path: &Path) -> Result<(), ClusterConfigError> {
    let value = path
        .to_str()
        .ok_or(ClusterConfigError::WindowsLocalDriveRequired)?;
    let bytes = value.as_bytes();
    let has_drive_root = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if !path.is_absolute() || !has_drive_root {
        return Err(ClusterConfigError::WindowsLocalDriveRequired);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_windows_path_chain(path: &Path) -> Result<(), ClusterConfigError> {
    for component in path.ancestors() {
        match fs::symlink_metadata(component) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || backend_platform::win32::security::is_endpoint_metadata(&metadata) =>
            {
                return Err(ClusterConfigError::Invalid);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ClusterConfigError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ClusterConfigError::Invalid)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ClusterConfigError::Invalid)?;
        self.cursor = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ClusterConfigError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ClusterConfigError::Invalid)
    }

    fn u8(&mut self) -> Result<u8, ClusterConfigError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ClusterConfigError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, ClusterConfigError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn count(&mut self, max: usize) -> Result<usize, ClusterConfigError> {
        let count = usize::from(u16::from_be_bytes(self.array()?));
        if count > max {
            return Err(ClusterConfigError::Invalid);
        }
        Ok(count)
    }

    fn string(&mut self, max: usize) -> Result<&'a str, ClusterConfigError> {
        let length = usize::from(u16::from_be_bytes(self.array()?));
        if length > max {
            return Err(ClusterConfigError::Invalid);
        }
        std::str::from_utf8(self.take(length)?).map_err(|_| ClusterConfigError::Invalid)
    }

    fn finish(self) -> Result<(), ClusterConfigError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ClusterConfigError::Invalid)
        }
    }
}

fn put_count(bytes: &mut Vec<u8>, count: usize, max: usize) -> Result<(), ClusterConfigError> {
    if count > max {
        return Err(ClusterConfigError::Invalid);
    }
    let count = u16::try_from(count).map_err(|_| ClusterConfigError::Invalid)?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn put_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), ClusterConfigError> {
    let value = u32::try_from(value).map_err(|_| ClusterConfigError::Invalid)?;
    bytes.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), ClusterConfigError> {
    if value.len() > MAX_ADDRESS_TEXT {
        return Err(ClusterConfigError::Invalid);
    }
    let length = u16::try_from(value.len()).map_err(|_| ClusterConfigError::Invalid)?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn endpoint_id(bytes: [u8; 32]) -> Result<EndpointId, ClusterConfigError> {
    EndpointId::from_bytes(&bytes).map_err(|_| ClusterConfigError::Invalid)
}

fn worker_execution_class(class: ClusterExecutionClass) -> WorkerExecutionClass {
    match class {
        ClusterExecutionClass::PureInProcessParser => WorkerExecutionClass::PureInProcessParser,
        ClusterExecutionClass::TrustedCoordinatorHostExecution => {
            WorkerExecutionClass::TrustedCoordinatorHostExecution
        }
    }
}

fn fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], ClusterConfigError> {
    let bytes = unhex(value)?;
    bytes.try_into().map_err(|_| ClusterConfigError::Invalid)
}

fn unhex(value: &str) -> Result<Vec<u8>, ClusterConfigError> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ClusterConfigError::Invalid);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(ClusterConfigError::Invalid)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(ClusterConfigError::Invalid)?;
        bytes.push((high * 16 + low) as u8);
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn private_dir() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("backend-worker-cluster-{stamp}"));
        fs::create_dir(&dir).expect("create temporary config directory");
        #[cfg(unix)]
        fs::set_permissions(&dir, fs::Permissions::from_mode(CONFIG_DIR_MODE))
            .expect("restrict temporary config directory");
        #[cfg(windows)]
        backend_platform::win32::security::restrict_to_current_user(&dir)
            .expect("restrict temporary config directory");
        dir
    }

    #[test]
    fn persisted_identity_cold_reopens_with_deny_by_default() {
        let dir = private_dir();
        let path = dir.join("worker.identity");
        let namespace = [9; 16];
        let recipe = [7; 32];
        let config = PersistedClusterConfig::create(
            "127.0.0.1:0".parse().expect("loopback socket"),
            namespace,
            vec![recipe],
        )
        .expect("valid initial config");
        let fingerprint = config.identity_fingerprint();
        config.save_atomic(&path).expect("atomic config write");
        let reopened = PersistedClusterConfig::load(&path).expect("cold config reopen");
        assert_eq!(reopened.identity_fingerprint(), fingerprint);
        assert_eq!(reopened.worker.policy.namespace_id, namespace);
        assert_eq!(
            reopened.worker.policy.execution_policy,
            ClusterExecutionPolicy::DenyUnconfined
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path)
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o777,
            CONFIG_FILE_MODE
        );
        fs::remove_dir_all(dir).expect("remove temporary config directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_paths_reject_unc_device_and_relative_paths() {
        for value in [
            r"\\server\share\worker.identity",
            r"\\?\C:\worker\identity",
            r"C:worker\identity",
            r"relative\worker.identity",
        ] {
            assert!(matches!(
                validate_windows_local_drive_path(Path::new(value)),
                Err(ClusterConfigError::WindowsLocalDriveRequired)
            ));
        }
        assert!(validate_windows_local_drive_path(Path::new(r"C:\worker\identity")).is_ok());
    }

    #[test]
    fn exact_invite_import_is_persisted_one_time_and_revocable() {
        let dir = private_dir();
        let path = dir.join("worker.identity");
        let namespace_id = [3; 16];
        let recipe = [4; 32];
        let mut config = PersistedClusterConfig::create(
            "127.0.0.1:0".parse().expect("loopback socket"),
            namespace_id,
            vec![recipe],
        )
        .expect("valid initial config");
        let coordinator_key = SecretKey::generate();
        let invite = ScopedClusterInvite::new(
            coordinator_key.public(),
            "127.0.0.1:4411".parse().expect("coordinator socket"),
            namespace_id,
            recipe,
            [1, 2],
            1,
            toolchain_claim([5; 32]),
            [6; 32],
            [8; 32],
            50_000,
            ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .expect("valid shared invite");
        let token = invite.encode_token().expect("canonical invite token");
        let decoded = ScopedClusterInvite::decode_token(&token).expect("decode invite token");
        let binding = local_binding_for(&decoded);
        config
            .import_invite_with_binding(decoded.clone(), 10_000, binding)
            .expect("explicit invite import");
        assert!(
            config
                .import_invite_with_binding(decoded, 10_000, binding)
                .is_err()
        );
        config
            .save_atomic(&path)
            .expect("persist coordinator allowlist");
        let mut reopened = PersistedClusterConfig::load(&path).expect("cold trust reopen");
        let ClusterExecutionPolicy::Allowlisted(grants) = &reopened.worker.policy.execution_policy
        else {
            panic!("imported trust grant is retained");
        };
        assert_eq!(grants.len(), 1);
        assert_eq!(
            grants[0].class,
            WorkerExecutionClass::TrustedCoordinatorHostExecution
        );
        assert_eq!(
            grants[0].toolchain,
            ContentId::<ToolchainDomain>::from_digest([5; 32])
        );
        assert_eq!(grants[0].local_authority_fingerprint, [9; 32]);
        reopened
            .revoke_coordinator(coordinator_key.public())
            .expect("explicit coordinator revocation");
        assert!(matches!(
            reopened.worker.policy.execution_policy,
            ClusterExecutionPolicy::DenyUnconfined
        ));
        fs::remove_dir_all(dir).expect("remove temporary config directory");
    }

    #[test]
    fn unverified_pure_recipe_invite_is_rejected_before_persistence() {
        let mut config = PersistedClusterConfig::create(
            "127.0.0.1:0".parse().expect("loopback socket"),
            [3; 16],
            vec![[4; 32]],
        )
        .expect("valid initial config");
        let invite = ScopedClusterInvite::new(
            SecretKey::generate().public(),
            "127.0.0.1:4411".parse().expect("coordinator socket"),
            [3; 16],
            [4; 32],
            [1, 2],
            1,
            toolchain_claim([5; 32]),
            [6; 32],
            [8; 32],
            50_000,
            ClusterExecutionClass::PureInProcessParser,
        )
        .expect("valid shared invite");
        assert!(matches!(
            config.import_invite_with_binding(invite.clone(), 10_000, local_binding_for(&invite),),
            Err(ClusterConfigError::PureExecutionUnavailable)
        ));
        assert!(config.worker.coordinators.is_empty());
        assert_eq!(
            config.worker.policy.execution_policy,
            ClusterExecutionPolicy::DenyUnconfined
        );
    }

    fn local_binding_for(invite: &ScopedClusterInvite) -> LocalCompilerBinding {
        LocalCompilerBinding {
            profile: invite.profile(),
            toolchain: ContentId::<ToolchainDomain>::try_from(invite.toolchain())
                .expect("invite toolchain uses the registered typed domain"),
            environment: invite.environment(),
            target_platform: invite.target_platform(),
            local_authority_fingerprint: [9; 32],
        }
    }

    fn toolchain_claim(raw_digest: [u8; 32]) -> [u8; 32] {
        *ContentId::<ToolchainDomain>::from_digest(raw_digest).as_ref()
    }

    #[test]
    fn invite_toolchain_requires_the_canonical_toolchain_domain_id() {
        use backend_version::CompileRecipeDomain;

        let digest = [0x2a; 32];
        let raw_digest = digest;
        let canonical = ContentId::<ToolchainDomain>::from_digest(digest);
        assert_ne!(canonical.as_ref(), &raw_digest);
        let decoded = ContentId::<ToolchainDomain>::try_from(*canonical.as_ref())
            .expect("decode canonical typed toolchain ID");
        assert_eq!(decoded, canonical);

        let wrong_domain = *ContentId::<CompileRecipeDomain>::from_digest(digest).as_ref();
        assert!(ContentId::<ToolchainDomain>::try_from(wrong_domain).is_err());
        let invite = ScopedClusterInvite::new(
            SecretKey::generate().public(),
            "127.0.0.1:4411".parse().expect("coordinator socket"),
            [3; 16],
            [4; 32],
            [1, 2],
            1,
            wrong_domain,
            [6; 32],
            [8; 32],
            50_000,
            ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .expect("wire invite validates nonzero claim before typed admission");
        let binding = LocalCompilerBinding {
            profile: invite.profile(),
            toolchain: canonical,
            environment: invite.environment(),
            target_platform: invite.target_platform(),
            local_authority_fingerprint: [9; 32],
        };
        assert_eq!(
            binding.mismatched_invite_field(&invite),
            Some("canonical ToolchainDomain identity")
        );
        let wrong_toolchain_binding = LocalCompilerBinding {
            toolchain: ContentId::<ToolchainDomain>::from_digest([0x2b; 32]),
            ..binding
        };
        assert_eq!(
            wrong_toolchain_binding.mismatched_invite_field(&invite),
            Some("canonical ToolchainDomain identity")
        );
        let valid_invite = ScopedClusterInvite::new(
            SecretKey::generate().public(),
            "127.0.0.1:4411".parse().expect("coordinator socket"),
            [3; 16],
            [4; 32],
            [1, 2],
            1,
            toolchain_claim(digest),
            [6; 32],
            [8; 32],
            50_000,
            ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .expect("valid typed toolchain claim");
        assert_eq!(binding.mismatched_invite_field(&valid_invite), None);
        assert_eq!(
            wrong_toolchain_binding.mismatched_invite_field(&valid_invite),
            Some("canonical ToolchainDomain identity")
        );
    }
}
