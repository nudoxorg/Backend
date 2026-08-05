//! Real smolvm backend: [`SmolvmRuntime`] implementing [`crate::vm::VmRuntime`]
//! over the vendored smolvm crate (SMOLVM-PLAN §3, P0).
//!
//! # API layer choice
//!
//! We use `smolvm::agent::AgentManager` (not the raw `vm::VmBackend`/`VmHandle`
//! or `EmbeddedRuntime`) because:
//!
//! - `VmBackend::create` forks internally and **blocks** until the VM exits — it
//!   cannot be used for the boot-then-exec-then-kill flow we need.
//! - `EmbeddedRuntime` is backed by a `SmolvmDb` SQLite registry, which is overkill
//!   for ephemeral one-shot job VMs with no persistence requirement.
//! - `AgentManager` starts the VM as an independent subprocess (via `smolvm _boot-vm`),
//!   exposes a vsock socket we can `AgentClient::connect` to, and provides `vm_exec`
//!   for direct-to-rootfs execution — exactly the one-shot cage lifecycle we need.
//!
//! # RootfsStore bootstrap
//!
//! [`RootfsStore`] reads the `NUDOX_GUEST_ROOTFS` environment variable, which must
//! point to a directory with this layout:
//!
//! ```text
//! $NUDOX_GUEST_ROOTFS/
//!   base/                  ← default rootfs (Alpine + smolvm-agent + producer-worker)
//!   sha256:<hex>/          ← per-image rootfs keyed by OCI image digest
//!   …
//! ```
//!
//! An unresolvable image (directory missing) is a typed
//! [`VmError::RootfsUnavailable`] — never a fallback.
//!
//! # Facade → real translation
//!
//! | Facade field | Real translation | Gaps |
//! |---|---|---|
//! | `VmConfig.memory_mib` (NonZeroU64 MiB) | `VmResources.memory_mib` (u32); overflow → `VmError::InvalidConfig` | — |
//! | `VmConfig.cpus` (NonZeroU32) | `VmResources.cpus` (u8); overflow → `VmError::InvalidConfig` | — |
//! | `VmConfig.image` | `RootfsStore::resolve(Some(&digest))` → `sha256:<hex>/` subdir; `None` → `base/` | missing dir → `VmError::RootfsUnavailable` |
//! | `NetworkPolicy::None` | `VmResources { network: false, allowed_cidrs: None }` | — |
//! | `NetworkPolicy::Egress(EgressPolicy { allowed_cidrs, dns: DnsPolicy::AllowAll })` | `VmResources { network: true, allowed_cidrs: None }` | — |
//! | `NetworkPolicy::Egress(EgressPolicy { allowed_cidrs, dns: DnsPolicy::Allowlist(_) })` | **`VmError::Unsupported`** — smolvm's DNS filter is host-process-based (`dns_filter.rs`), not configurable from `VmConfig`/`VmResources`; the allowlist cannot be enforced through this API surface | documented gap V-DNS-1 |
//! | `NetworkPolicy::Egress(EgressPolicy { allowed_cidrs, dns: DnsPolicy::Deny })` | `VmResources { network: true, allowed_cidrs: Some(cidrs) }` | DNS implicitly off (no name resolves without a DNS server) |
//! | `VirtiofsMount { host_path, read_only, … }` | `smolvm::data::storage::HostMount { source: host_path, target: "/mnt/<tag>", read_only }` | tag ≠ guest path; guest producer must be configured to use `/mnt/<tag>` paths |
//! | `ScratchOverlay::Ephemeral` | ephemeral disk (temp-dir-backed `OverlayDisk`); destroyed on drop | — |
//! | `VsockPort { listen: true, socket_path, … }` | `PublishedSocketConfig { direction: Mount, guest_path: IR_STREAM_GUEST_SOCKET, host_path: Some(socket_path) }` → `LaunchFeatures.published_sockets` → `start_with_full_config`; smolvm assigns vsock port `PUBLISH_SOCKET_BASE + i` (≥ 6100) | — |
//! | `VsockPort { listen: false, … }` | **`VmError::Unsupported`** — `listen: false` means the *guest* listens and the host connects in; that is the `Expose` direction, which is the opposite of the IR-stream pattern (host-listener). No use case for this exists in the current design. | documented gap V-VSOCK-EXPOSE-1 |
//! | `RunSpec.rlimits` | prepended `sh -c 'ulimit …; exec …'` wrapper | smolvm-protocol has no rlimit field; wrapper is argv-safe (ulimit built-in) |
//! | `RunSpec.timeout` | `AgentClient::vm_exec(…, timeout: Some(…))` | — |
//! | `GoldenId` / `fork_golden` | **real CoW fork (V-GOLD-1 CLOSED)** — a warm *forkable* golden VM (memfd-backed guest RAM + control socket) is registered per image digest; `fork_golden` calls `smolvm::agent::fork::prepare_fork` to freeze+snapshot the golden and CoW-clone its disks, then boots the ephemeral job clone from the golden's in-memory snapshot (`LaunchFeatures.snapshot_dir`) — the ~250 ms restore, not a cold boot | — |
//! | `checkpoint` | boots a fresh forkable golden (memfd RAM + control socket) from `base/` and registers it in the DB; the returned [`GoldenId`] is the golden's VM name | — |
//!
//! # Golden fork (V-GOLD-1 — realized)
//!
//! The smolvm crate at the pinned rev exposes the **whole** live-fork stack
//! publicly, not only through `EmbeddedRuntime`:
//!
//! - [`smolvm::agent::LaunchFeatures`] carries `forkable` (memfd-back guest RAM
//!   so it is copy-on-write cloneable), `control_socket` (pause/resume/FORK),
//!   and `snapshot_dir` (boot as a clone, restoring from a golden's snapshot).
//! - [`smolvm::agent::fork::prepare_fork`] freezes a running forkable golden,
//!   snapshots its memfd RAM + device state, CoW-clones its qcow2 disks
//!   (`O(metadata)` regardless of golden size), inserts the clone's DB record,
//!   and returns the `snapshot_dir` to boot the clone from.
//! - [`smolvm::agent::fork::rejuvenate_clone`] /
//!   [`smolvm::agent::fork::fail_closed_on_rejuvenation`] re-mint the clone's
//!   per-machine on-disk identity (machine-id, SSH host keys, hostname, RNG)
//!   fail-closed, so a clone never impersonates the golden across tenants.
//!
//! We drive those primitives directly (mirroring `smolvm::embedded::control::fork_vm`
//! but keeping our own [`SmolvmHandle`] type and one-VM-per-job teardown) rather
//! than adopting `EmbeddedRuntime` wholesale. The `SmolvmDb` registry the module
//! doc originally called "overkill for ephemeral job VMs" is *intrinsic* to fork:
//! `prepare_fork` records the clone and its golden→clone CoW-disk dependency in
//! the DB. The warm golden is long-lived (one per toolchain image digest, kept
//! frozen as the shared base); every job's clone is still ephemeral — it is
//! killed **and DB-removed** at end of job (see [`SmolvmHandle::kill`]).
//!
//! ## Residual upstream limitation
//!
//! Fork rejuvenation scrubs only *on-disk* identity. In-RAM golden secrets are
//! CoW-inherited by every clone — intrinsic to fork-from-warm and a golden-image
//! packaging constraint upstream, not fixable here (see
//! [`smolvm::agent::fork::rejuvenate_clone`] docs). Cage goldens are toolchain
//! base images that mint no per-instance boot secrets in RAM, so this is benign.
//! Live fork is Linux/macOS only (`clone_fork_disks`); on other targets
//! `prepare_fork` returns an error that we fold to [`VmError::Unsupported`].
//!
//! # Published-socket bridge — direction semantics
//!
//! smolvm's `SocketDirection` has two values (`src/config.rs:23`,
//! `crates/smolvm-protocol/src/publish_socket.rs`):
//!
//! - **`Expose`** (guest→host): the guest process already listens on `guest_path`;
//!   libkrun creates a Unix listener on the host side (`host_listens() == true`);
//!   host clients connect to it and are proxied to the guest listener via vsock.
//!   Pattern: Docker bridge.
//!
//! - **`Mount`** (host→guest): the host process already listens on `host_path`;
//!   libkrun does **not** create a host listener (`host_listens() == false`) — it
//!   only dials the host socket on behalf of vsock connections arriving from the
//!   guest. The guest agent creates a Unix listener at `guest_path`; guest clients
//!   connect there and are relayed to the host socket via vsock.
//!   Pattern: SSH-agent bridge. **This is the IR-stream direction.**
//!
//! For IR streaming: the host already listens on `StreamPort.socket_path` before
//! the VM boots; the in-guest producer connects to
//! [`IR_STREAM_GUEST_SOCKET`](crate::vm::IR_STREAM_GUEST_SOCKET)
//! (`/run/nudox/ir-stream.sock`). That guest path is the `Mount` direction's
//! `guest_path`; `socket_path` is the `host_path`.
//!
//! Evidence: `src/agent/launcher.rs:1141-1204`, `src/agent/launcher.rs:1173`
//! (`spec.direction.host_listens()`), `crates/smolvm-agent/src/publish_socket.rs:101-150`
//! (`serve_mount` creates the guest-side listener).
//!
//! # smolvm binary requirement
//!
//! `AgentManager::start_via_subprocess` spawns `smolvm _boot-vm` — the smolvm CLI
//! binary must be on `PATH` (or pointed to by smolvm's own `SMOLVM_PATH` env var).
//! Missing binary → `VmError::Launch` with a clear message.

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use smolvm::agent::fork::{
    control_socket_path, fail_closed_on_rejuvenation, prepare_fork, rejuvenate_clone,
};
use smolvm::agent::{AgentManager, HostMount, LaunchFeatures, VmResources, vm_data_dir};
use smolvm::config::{PublishedSocketConfig, SocketDirection, VmRecord};
use smolvm::db::SmolvmDb;
use smolvm::storage::{OverlayDisk, StorageDisk};

use crate::spec::{Output, ProcessEnd};
use crate::toolchains::images::ImageDigest;
use crate::vm::{
    DnsPolicy, EgressPolicy, GoldenId, IR_STREAM_GUEST_SOCKET, NetworkPolicy, RunSpec, VmConfig,
    VmError, VmHandle, VmRuntime, VsockPort,
};

// ─── RootfsStore ────────────────────────────────────────────────────────────

/// Maps toolchain-image digests to pre-built rootfs directories.
///
/// # Bootstrap
///
/// Read from the `NUDOX_GUEST_ROOTFS` env var at construction time via
/// [`RootfsStore::from_env`]. The env var must point at a directory where:
///
/// - `base/` is the default rootfs (Alpine + smolvm-agent + producer-worker)
/// - `sha256:<64 lowercase hex>/` is the per-image rootfs for a given OCI digest
///
/// # Errors
///
/// [`RootfsStore::from_env`] returns [`VmError::InvalidConfig`] when
/// `NUDOX_GUEST_ROOTFS` is unset or empty. [`RootfsStore::resolve`] returns
/// [`VmError::RootfsUnavailable`] when the digest-keyed subdirectory is absent —
/// never falls back silently.
#[derive(Debug, Clone)]
pub struct RootfsStore {
    root: PathBuf,
}

impl RootfsStore {
    /// Bootstrap from `NUDOX_GUEST_ROOTFS`.
    ///
    /// Returns [`VmError::InvalidConfig`] when the variable is unset or the
    /// directory does not exist.
    pub fn from_env() -> Result<Self, VmError> {
        let raw = std::env::var_os("NUDOX_GUEST_ROOTFS").ok_or_else(|| VmError::InvalidConfig {
            reason: "NUDOX_GUEST_ROOTFS is not set; point it at a directory \
				         containing base/ and sha256:<hex>/ subdirectories"
                .into(),
        })?;
        let root = PathBuf::from(raw);
        if !root.is_dir() {
            return Err(VmError::InvalidConfig {
                reason: format!("NUDOX_GUEST_ROOTFS={} is not a directory", root.display()),
            });
        }
        Ok(Self { root })
    }

    /// Resolve the rootfs path for `digest`, or the `base/` fallback when
    /// `digest` is `None`.
    ///
    /// Returns [`VmError::RootfsUnavailable`] when the directory does not exist.
    pub fn resolve(&self, digest: Option<&ImageDigest>) -> Result<PathBuf, VmError> {
        let candidate = match digest {
            Some(d) => self.root.join(d.to_string()),
            None => self.root.join("base"),
        };
        if candidate.is_dir() {
            Ok(candidate)
        } else {
            Err(VmError::RootfsUnavailable {
                digest: candidate.to_string_lossy().into_owned(),
            })
        }
    }
}

// ─── Translation helpers ─────────────────────────────────────────────────────

/// Translate our [`NetworkPolicy`] into the smolvm [`VmResources`] network fields.
///
/// `DnsPolicy::Allowlist` cannot be enforced through the `VmResources` API
/// (the smolvm DNS filter runs as a host-side listener wired by the agent
/// launcher, not configurable per-VM through this interface) — it returns
/// [`VmError::Unsupported`] rather than silently dropping the restriction.
fn translate_network(net: &NetworkPolicy, resources: &mut VmResources) -> Result<(), VmError> {
    match net {
        NetworkPolicy::None => {
            resources.network = false;
            resources.allowed_cidrs = None;
            resources.dns = None;
        }
        NetworkPolicy::Egress(EgressPolicy { allowed_cidrs, dns }) => {
            resources.network = true;
            match dns {
                DnsPolicy::Deny => {
                    // No DNS nameserver → effectively deny DNS. CIDRs still enforced.
                    resources.dns = None;
                    if !allowed_cidrs.is_empty() {
                        resources.allowed_cidrs =
                            Some(allowed_cidrs.iter().map(|c| c.to_string()).collect());
                    } else {
                        resources.allowed_cidrs = None;
                    }
                }
                DnsPolicy::AllowAll => {
                    resources.dns = None;
                    if !allowed_cidrs.is_empty() {
                        resources.allowed_cidrs =
                            Some(allowed_cidrs.iter().map(|c| c.to_string()).collect());
                    } else {
                        resources.allowed_cidrs = None;
                    }
                }
                DnsPolicy::Allowlist(_names) => {
                    // smolvm's DNS filter (`dns_filter.rs`) runs as a host-side
                    // process wired by the agent launcher, not configurable from
                    // VmResources. Name-level DNS filtering cannot be enforced at
                    // this API surface — V-DNS-1.
                    return Err(VmError::Unsupported {
                        reason: "DnsPolicy::Allowlist cannot be enforced through \
						         AgentManager/VmResources — smolvm's DNS filter is a \
						         host-process listener wired by the launcher, not a \
						         per-VM VmResources field. Use DnsPolicy::Deny for \
						         strict isolation or DnsPolicy::AllowAll for open DNS. \
						         (gap V-DNS-1)"
                            .into(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Translate virtiofs mounts to smolvm `HostMount` list.
///
/// The guest path is `/mnt/<tag>` — producers must use these paths. The
/// `read_only` flag is preserved 1:1.
fn translate_mounts(cfg: &VmConfig) -> Result<Vec<HostMount>, VmError> {
    let mut mounts = Vec::with_capacity(cfg.mounts.len());
    for m in &cfg.mounts {
        mounts.push(HostMount::new(
            &m.host_path,
            format!("/mnt/{}", m.tag.as_str()),
            m.read_only,
        )?);
    }
    Ok(mounts)
}

/// Translate facade `VsockPort` entries into smolvm `PublishedSocketConfig`s.
///
/// # Direction mapping
///
/// `VsockPort { listen: true }` means the **host** listens and the guest
/// producer connects — this is smolvm's `Mount` direction:
///
/// - `host_path` = `socket_path` (the host-side Unix socket already listening)
/// - `guest_path` = [`IR_STREAM_GUEST_SOCKET`] (`/run/nudox/ir-stream.sock`)
/// - `direction` = `SocketDirection::Mount`
///
/// smolvm's guest agent will create a Unix listener at `guest_path`; the guest
/// producer connects there, and the agent relays each connection through the
/// vsock port that libkrun bridges to `host_path`.
///
/// `VsockPort { listen: false }` is the *Expose* pattern (guest listens, host
/// connects in) — that direction has no current use case in the cage design and
/// is rejected with `VmError::Unsupported` (gap V-VSOCK-EXPOSE-1) rather than
/// silently dropped.
///
/// smolvm auto-assigns the vsock port number (`PUBLISH_SOCKET_BASE + i`,
/// starting at 6100); the `port` field of the facade `VsockPort` is advisory
/// for documentation purposes only on this backend.
pub fn translate_vsock_ports(ports: &[VsockPort]) -> Result<Vec<PublishedSocketConfig>, VmError> {
    let mut out = Vec::with_capacity(ports.len());
    for p in ports {
        if !p.listen {
            // listen: false = guest-listens / host-connects-in pattern (Expose).
            // No current cage use case: the IR-stream host is always the listener.
            // Reject rather than silently drop — no-silent-degrade rule.
            return Err(VmError::Unsupported {
                reason: format!(
                    "VsockPort {{ port: {}, listen: false }} requests the 'Expose' \
					 direction (guest listens, host connects in) which has no current \
					 use in the cage design. Only host-listening (listen: true) vsock \
					 ports are expressible as 'Mount' socket bridges on this backend. \
					 (gap V-VSOCK-EXPOSE-1)",
                    p.port
                ),
            });
        }
        out.push(PublishedSocketConfig {
            direction: SocketDirection::Mount,
            guest_path: IR_STREAM_GUEST_SOCKET.to_owned(),
            host_path: Some(p.socket_path.to_string_lossy().into_owned()),
        });
    }
    Ok(out)
}

/// Build the `argv` for a `RunSpec`, prepending a `sh -c 'ulimit …; exec …'`
/// wrapper that applies in-guest rlimits before exec.
///
/// The ulimit shell built-in is always available inside the Alpine guest image and
/// does not change the semantics of argv0 visible to the executed program
/// (exec replaces the shell). This is the only channel available for rlimits
/// since smolvm-protocol has no rlimit field.
fn build_command_with_rlimits(spec: &RunSpec) -> Vec<String> {
    let rl = &spec.rlimits;
    let ulimit_prefix = format!(
        "ulimit -t {cpu} -u {pids} -n {nofile} -f {fsize} -v {mem}",
        cpu = rl.cpu_secs,
        pids = rl.pids,
        nofile = rl.nofile,
        fsize = rl.fsize_bytes.get() / 512, // ulimit -f is in 512-byte blocks
        mem = rl.mem_bytes.get() / 1024,    // ulimit -v is in KiB
    );
    let inner_argv: Vec<String> = std::iter::once(spec.command.to_string_lossy().into_owned())
        .chain(spec.args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    let exec_str = shell_quote_argv(&inner_argv);
    let script = format!("{ulimit_prefix}; exec {exec_str}");
    vec!["sh".into(), "-c".into(), script]
}

/// Minimally quote a single argument for inclusion in a `sh -c` string.
///
/// We wrap each argument in single quotes and escape embedded single quotes via
/// `'\''`. This is argv-safe for all argument values.
fn shell_quote_one(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Join pre-quoted arguments into a single exec string.
fn shell_quote_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shell_quote_one(a))
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── SmolvmHandle ────────────────────────────────────────────────────────────

/// A live smolvm VM handle: holds the `AgentManager` (which owns the VM
/// subprocess) and a connected `AgentClient` (vsock exec channel).
pub struct SmolvmHandle {
    manager: AgentManager,
    client: smolvm::agent::AgentClient,
    /// Temp directories holding ephemeral disks (cold-boot / `launch` path);
    /// dropped (deleted) with the handle. `None` for a fork clone, whose disks
    /// are DB-managed CoW overlays under `vm_data_dir(name)` and are torn down by
    /// [`SmolvmHandle::teardown`] instead.
    _scratch: Option<SmolvmScratch>,
    /// When this handle is a fork clone, its `(db, clone_name)`: on `kill` the
    /// clone VM is stopped **and removed** from the machine registry + its data
    /// dir deleted (one-VM-per-job ephemeral teardown). `None` for a `launch`ed
    /// one-shot VM (no DB record to clean).
    clone: Option<(SmolvmDb, String)>,
}

/// Ephemeral disk directories for one job VM.
struct SmolvmScratch {
    _dir: tempfile::TempDir,
}

impl std::fmt::Debug for SmolvmHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmolvmHandle").finish_non_exhaustive()
    }
}

impl VmHandle for SmolvmHandle {
    fn exec(&mut self, spec: &RunSpec) -> Result<Output, VmError> {
        let t0 = Instant::now();
        let command = build_command_with_rlimits(spec);
        let env: Vec<(String, String)> = spec.env.clone();
        let workdir = spec.cwd.as_ref().map(|p| p.to_string_lossy().into_owned());
        let timeout = Some(spec.timeout);

        let (exit_code, stdout, stderr) = self
            .client
            .vm_exec(command, env, workdir, timeout, None)
            .map_err(|e| VmError::Exec {
                reason: e.to_string(),
            })?;

        let wall = t0.elapsed();
        let end = ProcessEnd::Exited(std::process::ExitStatus::from_raw(exit_code * 256));
        Ok(Output {
            stdout,
            stderr,
            end,
            wall,
            peak_mem: None,
        })
    }

    fn kill(&mut self) {
        // Best-effort: shut down gracefully then kill. The scratch temp-dir is
        // cleaned up on drop regardless.
        let _ = self.manager.stop();
        // One-VM-per-job: a fork clone is a DB-registered named machine with
        // CoW-overlay disks under vm_data_dir(name). Forking is the START
        // optimization; the clone still dies at end of job — so remove its
        // registry record and delete its data dir (disks) after stopping it.
        if let Some((db, name)) = self.clone.take() {
            let _ = db.remove_vm(&name);
            let _ = std::fs::remove_dir_all(vm_data_dir(&name));
        }
    }
}

// ─── SmolvmRuntime ───────────────────────────────────────────────────────────

/// [`VmRuntime`] implementation backed by the real smolvm crate.
///
/// Construct via [`SmolvmRuntime::new`]; inject into [`crate::smolvm::SmolvmCage`]
/// via [`crate::smolvm::SmolvmCage::with_runtime`].
///
/// Each [`VmRuntime::launch`] call creates a fresh ephemeral VM:
///
/// 1. Resolves the rootfs path from [`RootfsStore`].
/// 2. Creates ephemeral storage + overlay disks in a temp directory.
/// 3. Starts the VM via `AgentManager` (which spawns `smolvm _boot-vm`).
/// 4. Connects an `AgentClient` over the vsock socket.
/// 5. Returns a [`SmolvmHandle`] that execs and kills via the agent protocol.
///
/// [`VmRuntime::fork_golden`] performs a **real CoW fork** from a warm forkable
/// golden VM registered per image digest (V-GOLD-1 closed) — see the module docs
/// and [`SmolvmRuntime::prepare_golden`].
///
/// # Golden vs. `launch` rootfs
///
/// [`VmRuntime::launch`] boots a one-shot VM from an *explicit* rootfs resolved
/// by [`RootfsStore`] (image-digest → subdir), owning ephemeral temp-dir disks.
/// The golden/fork path instead uses smolvm's DB-managed **named** VMs: a golden
/// is a long-lived `AgentManager::for_vm` machine whose disks live under
/// `vm_data_dir(name)` (booted from smolvm's default agent rootfs), because
/// `prepare_fork` freezes it by name and CoW-clones those disks. The toolchain is
/// provisioned *into* the golden (a warm, prepared machine) rather than selected
/// by a rootfs subdir — the idiomatic smolvm golden model.
#[derive(Debug)]
pub struct SmolvmRuntime {
    store: RootfsStore,
    /// Monotonic salt so each fork clone gets a unique VM name within a process.
    fork_seq: AtomicU64,
}

impl SmolvmRuntime {
    /// Construct with an explicit rootfs store.
    ///
    /// Infallible and side-effect-free: the smolvm machine registry (`SmolvmDb`,
    /// needed only by the golden-fork path) is opened lazily by
    /// [`SmolvmRuntime::db`], so a `launch`-only runtime never touches it.
    pub fn new(store: RootfsStore) -> Self {
        Self {
            store,
            fork_seq: AtomicU64::new(0),
        }
    }

    /// Open the smolvm machine registry (lazy; the golden-fork path only).
    ///
    /// `SmolvmDb::open` opens its connection lazily on first use, so this is cheap
    /// and safe to call per golden operation.
    fn db(&self) -> Result<SmolvmDb, VmError> {
        SmolvmDb::open().map_err(|e| VmError::Launch {
            reason: format!("open smolvm machine registry (SmolvmDb): {e}"),
        })
    }

    /// Launch one ephemeral VM from a resolved rootfs path.
    fn launch_vm(&self, rootfs: PathBuf, cfg: &VmConfig) -> Result<SmolvmHandle, VmError> {
        // 1. Translate resource knobs.
        let mut resources =
            VmResources {
                cpus: cfg
                    .cpus
                    .get()
                    .try_into()
                    .map_err(|_| VmError::InvalidConfig {
                        reason: format!(
                            "vCPU count {} overflows u8 (smolvm VmResources limit)",
                            cfg.cpus
                        ),
                    })?,
                memory_mib: cfg.memory_mib.get().try_into().map_err(|_| {
                    VmError::InvalidConfig {
                        reason: format!(
                            "memory_mib {} overflows u32 (smolvm VmResources limit)",
                            cfg.memory_mib
                        ),
                    }
                })?,
                ..VmResources::default()
            };
        translate_network(&cfg.network, &mut resources)?;

        // 2. Ephemeral disks in a temp directory.
        let tmp = tempfile::TempDir::new().map_err(VmError::Io)?;
        let storage_disk = StorageDisk::open_or_create_at(&tmp.path().join("storage.raw"), 1)
            .map_err(|e| VmError::Launch {
                reason: format!("storage disk: {e}"),
            })?;
        let overlay_disk = OverlayDisk::open_or_create_at(&tmp.path().join("overlay.raw"), 1)
            .map_err(|e| VmError::Launch {
                reason: format!("overlay disk: {e}"),
            })?;

        // 3. Build HostMount list.
        let mounts = translate_mounts(cfg)?;

        // 4. Translate vsock ports → published-socket bridges.
        //    VsockPort { listen: true } → Mount direction (host-listener, guest-connects).
        //    VsockPort { listen: false } → rejected (Expose direction, no current use case).
        let published_sockets = translate_vsock_ports(&cfg.vsock_ports)?;
        let features = LaunchFeatures {
            published_sockets,
            ..LaunchFeatures::default()
        };

        // 5. Create AgentManager and start.
        let manager =
            AgentManager::new(rootfs, storage_disk, overlay_disk).map_err(|e| VmError::Launch {
                reason: format!("AgentManager::new: {e}"),
            })?;

        manager
            .start_with_full_config(mounts, Vec::new(), resources, features)
            .map_err(|e| VmError::Launch {
                reason: format!("AgentManager::start_with_full_config: {e}"),
            })?;

        // 6. Connect agent client.
        let client = manager.connect().map_err(|e| VmError::Launch {
            reason: format!("agent connect: {e}"),
        })?;

        Ok(SmolvmHandle {
            manager,
            client,
            _scratch: Some(SmolvmScratch { _dir: tmp }),
            clone: None,
        })
    }

    /// Prepare (or return) a warm **forkable golden** for `image` and register it
    /// in the [`GoldenPool`] keyed by digest, returning its [`GoldenId`].
    ///
    /// A golden is a long-lived, DB-registered named machine started `forkable`
    /// (its guest RAM is memfd-backed → copy-on-write cloneable) with a control
    /// socket for the `FORK` command. Callers park the returned id in the pool;
    /// [`VmRuntime::fork_golden`] then CoW-forks an ephemeral clone from it per
    /// job (~250 ms restore) instead of cold-booting.
    ///
    /// The golden VM name is derived deterministically from the image digest, so
    /// re-preparing the same image is idempotent: if a golden by that name is
    /// already running forkable (its control socket answers `STATUS`), this
    /// returns the existing id without booting a second one.
    ///
    /// The golden stays frozen as the shared base once it has clones; it must not
    /// be relaunched writable while clones exist (smolvm enforces this — a
    /// re-`start` of a frozen fork base is refused).
    pub fn prepare_golden(&self, image: &ImageDigest) -> Result<GoldenId, VmError> {
        let name = golden_vm_name(image);

        // Idempotent: if the golden already runs forkable, reuse it. We probe the
        // control socket (not the vsock agent) because a golden that has already
        // been forked is frozen — its agent won't ping, but its control socket
        // still answers STATUS and it can be forked again.
        let ctl = control_socket_path(&name);
        if ctl.exists() {
            return Ok(GoldenId::new(name));
        }

        let db = self.db()?;

        // Register the golden's record (default toolchain-base sizes) if absent, so
        // `prepare_fork` can read it and the fork machinery can track clones.
        if db
            .get_vm(&name)
            .map_err(golden_err("read golden record"))?
            .is_none()
        {
            let record = VmRecord::new(
                name.clone(),
                /* cpus */ 1,
                /* mem MiB */ 512,
                /* mounts */ Vec::new(),
                /* ports */ Vec::new(),
                /* network */ false,
            );
            db.insert_vm(&name, &record)
                .map_err(golden_err("register golden record"))?;
        }

        // Boot it forkable: memfd-backed RAM + control socket. Named disks live
        // under vm_data_dir(name) via `for_vm`, which is exactly where
        // `prepare_fork` looks to CoW-clone them.
        let manager = AgentManager::for_vm(&name).map_err(golden_err("golden AgentManager"))?;
        let features = LaunchFeatures {
            forkable: true,
            control_socket: Some(ctl),
            ..LaunchFeatures::default()
        };
        let resources = VmResources {
            cpus: 1,
            memory_mib: 512,
            ..VmResources::default()
        };
        manager
            .start_with_full_config(Vec::new(), Vec::new(), resources, features)
            .map_err(golden_err("start forkable golden"))?;

        Ok(GoldenId::new(name))
    }

    /// CoW-fork an ephemeral clone from a warm forkable golden and return a live
    /// handle. Mirrors `smolvm::embedded::control::fork_vm` but keeps our
    /// [`SmolvmHandle`] type + one-VM-per-job teardown.
    fn fork_clone(&self, golden: &GoldenId) -> Result<SmolvmHandle, VmError> {
        let golden_name = golden.as_str();
        let clone_name = self.next_clone_name(golden_name);
        let db = self.db()?;

        // 1. Freeze the golden, snapshot its memfd RAM + device state, CoW-clone
        //    its disks, and register the clone's DB record. Returns the snapshot
        //    dir to boot the clone from. On non-Linux/macOS targets, or if the
        //    golden isn't running forkable, this is the typed error surface.
        let prep = prepare_fork(
            &db,
            golden_name,
            &clone_name,
            &[],
            /* clone_forkable */ false,
        )
        .map_err(|e| match e {
            // Live fork unsupported on this platform → surface as Unsupported.
            e if e.to_string().contains("not supported") => VmError::Unsupported {
                reason: format!("golden fork: {e}"),
            },
            e => VmError::Launch {
                reason: format!("prepare_fork golden '{golden_name}': {e}"),
            },
        })?;

        // 2. Boot the clone from the golden's in-memory snapshot (~250 ms restore)
        //    instead of cold-booting. Its CoW-overlay disks are already in place
        //    under vm_data_dir(clone_name); `for_vm` opens them as-is.
        let manager = AgentManager::for_vm(&clone_name).map_err(|e| {
            let _ = db.remove_vm(&clone_name);
            VmError::Launch {
                reason: format!("clone AgentManager '{clone_name}': {e}"),
            }
        })?;
        let features = LaunchFeatures {
            snapshot_dir: Some(prep.snapshot_dir.clone()),
            ..LaunchFeatures::default()
        };
        let boot = manager.start_with_full_config(
            prep.clone_record.host_mounts(),
            prep.clone_record.port_mappings(),
            prep.clone_record.vm_resources(),
            features,
        );
        if let Err(e) = boot {
            // prepare_fork already registered the clone; roll it back.
            let _ = db.remove_vm(&clone_name);
            let _ = std::fs::remove_dir_all(vm_data_dir(&clone_name));
            return Err(VmError::Launch {
                reason: format!("boot clone '{clone_name}' from snapshot: {e}"),
            });
        }

        // 3. Fresh on-disk identity (hostname, machine-id, SSH host keys, RNG),
        //    FAIL-CLOSED: a clone that could not be rejuvenated must never be
        //    vended (it would share the golden's per-machine secrets across
        //    tenants) — tear it down and turn the failure into a fork failure.
        fail_closed_on_rejuvenation(rejuvenate_clone(&clone_name), || {
            let _ = manager.stop();
            let _ = db.remove_vm(&clone_name);
            let _ = std::fs::remove_dir_all(vm_data_dir(&clone_name));
        })
        .map_err(|e| VmError::Launch {
            reason: format!("rejuvenate clone '{clone_name}': {e}"),
        })?;

        // 4. Connect the exec channel.
        let client = manager.connect().map_err(|e| {
            let _ = manager.stop();
            let _ = db.remove_vm(&clone_name);
            let _ = std::fs::remove_dir_all(vm_data_dir(&clone_name));
            VmError::Launch {
                reason: format!("agent connect (clone '{clone_name}'): {e}"),
            }
        })?;

        Ok(SmolvmHandle {
            manager,
            client,
            _scratch: None,
            clone: Some((db, clone_name)),
        })
    }

    /// A process-unique, DNS-safe clone name derived from the golden name and a
    /// monotonic counter (validated by smolvm as alphanumeric + dashes).
    fn next_clone_name(&self, golden_name: &str) -> String {
        let n = self.fork_seq.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        format!("{golden_name}-c{pid}-{n}")
    }
}

/// Deterministic, DNS/name-safe golden VM name for an image digest.
///
/// `ImageDigest::to_string()` is `sha256:<hex>`; smolvm's `validate_vm_name`
/// rejects `:`, so we key on the hex only (collision-free per digest).
pub(crate) fn golden_vm_name(image: &ImageDigest) -> String {
    let s = image.to_string();
    let hex = s.strip_prefix("sha256:").unwrap_or(&s);
    // Keep it short and unambiguous; the full hex keeps distinct digests distinct.
    format!("nudox-golden-{hex}")
}

/// Fold a smolvm error into a [`VmError::Launch`] with `ctx` prefixed.
fn golden_err(ctx: &'static str) -> impl Fn(smolvm::Error) -> VmError {
    move |e| VmError::Launch {
        reason: format!("{ctx}: {e}"),
    }
}

impl VmRuntime for SmolvmRuntime {
    type Handle = SmolvmHandle;

    fn launch(&self, cfg: &VmConfig) -> Result<Self::Handle, VmError> {
        let rootfs = self.store.resolve(cfg.image.as_ref())?;
        self.launch_vm(rootfs, cfg)
    }

    /// CoW-fork a warm VM from a golden checkpoint (~250 ms restore) — V-GOLD-1.
    ///
    /// `golden` names a forkable golden previously parked by
    /// [`SmolvmRuntime::prepare_golden`]. This freezes+snapshots it and boots an
    /// ephemeral clone from that snapshot (`LaunchFeatures.snapshot_dir`) via
    /// `smolvm::agent::fork::prepare_fork` — a real copy-on-write fork, not a cold
    /// boot. The clone still dies at end of job (see [`SmolvmHandle::kill`]).
    fn fork_golden(&self, golden: &GoldenId) -> Result<Self::Handle, VmError> {
        self.fork_clone(golden)
    }

    /// Checkpoint a prepared VM into a golden image.
    ///
    /// The passed one-shot `handle` was **not** booted forkable (`launch` owns
    /// ephemeral temp-dir disks and cannot be memfd-snapshotted after the fact),
    /// so we cannot promote it in place. Instead we tear it down and boot a fresh
    /// forkable golden from the default toolchain base, returning its
    /// [`GoldenId`]. Callers that want a specific toolchain image should prefer
    /// [`SmolvmRuntime::prepare_golden`], which keys the golden by digest.
    fn checkpoint(&self, mut handle: Self::Handle) -> Result<GoldenId, VmError> {
        handle.kill();
        // `ImageDigest::from_bytes` of all-zeros is the "base toolchain" golden.
        self.prepare_golden(&ImageDigest::from_bytes([0u8; 32]))
    }
}

// ─── VmError extension ───────────────────────────────────────────────────────

// We need to add RootfsUnavailable to VmError. Since VmError lives in vm.rs and
// has #[non_exhaustive], we can't add variants there without editing vm.rs.
// We extend VmError in vm.rs instead (see the vm.rs edit below).
// This conversion bridges smolvm's data::error::Error to HostMount errors.
impl From<smolvm::data::error::Error> for VmError {
    fn from(e: smolvm::data::error::Error) -> Self {
        VmError::InvalidConfig {
            reason: format!("host mount validation: {e}"),
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::vm::{
        Cidr, DnsName, EgressPolicy, GuestRlimits, MountTag, NetworkPolicy, VirtiofsMount,
        VmConfig, VmError, VsockPort,
    };

    // ── Network translation ──────────────────────────────────────────────────

    #[test]
    fn net_none_translates_to_network_off() {
        let mut r = VmResources::default();
        translate_network(&NetworkPolicy::None, &mut r).expect("no error");
        assert!(!r.network);
        assert!(r.allowed_cidrs.is_none());
        assert!(r.dns.is_none());
    }

    #[test]
    fn net_egress_allow_all_dns_translates() {
        let mut r = VmResources::default();
        let cidr: Cidr = "10.0.0.0/8".parse().unwrap();
        let net = NetworkPolicy::Egress(EgressPolicy {
            allowed_cidrs: vec![cidr],
            dns: DnsPolicy::AllowAll,
        });
        translate_network(&net, &mut r).expect("allow all is fine");
        assert!(r.network);
        let cidrs = r.allowed_cidrs.expect("cidrs present");
        assert_eq!(cidrs, vec!["10.0.0.0/8"]);
    }

    #[test]
    fn net_egress_deny_dns_translates() {
        let mut r = VmResources::default();
        let net = NetworkPolicy::Egress(EgressPolicy {
            allowed_cidrs: vec![],
            dns: DnsPolicy::Deny,
        });
        translate_network(&net, &mut r).expect("deny dns is fine");
        assert!(r.network);
        // No CIDRs → None
        assert!(r.allowed_cidrs.is_none());
    }

    #[test]
    fn net_egress_allowlist_dns_returns_unsupported() {
        let mut r = VmResources::default();
        let dns_name = DnsName::new("crates.io").unwrap();
        let net = NetworkPolicy::Egress(EgressPolicy {
            allowed_cidrs: vec![],
            dns: DnsPolicy::Allowlist(vec![dns_name]),
        });
        let err = translate_network(&net, &mut r).expect_err("should fail");
        assert!(matches!(err, VmError::Unsupported { .. }));
    }

    // ── Memory overflow ──────────────────────────────────────────────────────

    #[test]
    fn memory_mib_overflow_u32_returns_invalid_config() {
        // NonZeroU64 value that exceeds u32::MAX
        let too_big = NonZeroU64::new(u64::from(u32::MAX) + 1).unwrap();
        let cfg = VmConfig::builder().memory_mib(too_big).build();
        let store = make_temp_store();
        let rt = SmolvmRuntime::new(store);
        let err = rt.launch(&cfg).expect_err("should fail on overflow");
        assert!(
            matches!(err, VmError::InvalidConfig { .. }),
            "expected InvalidConfig, got {err:?}"
        );
    }

    // ── Mount translation ────────────────────────────────────────────────────

    #[test]
    fn mount_ro_preserved_in_translation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = VmConfig::builder()
            .mount(VirtiofsMount {
                tag: MountTag::new("ro0").unwrap(),
                host_path: tmp.path().to_path_buf(),
                read_only: true,
                dax: true,
            })
            .build();
        let mounts = translate_mounts(&cfg).expect("valid mount");
        assert_eq!(mounts.len(), 1);
        assert!(mounts[0].read_only, "RO flag must be preserved 1:1");
        assert_eq!(mounts[0].target, PathBuf::from("/mnt/ro0"));
    }

    #[test]
    fn mount_rw_preserved_in_translation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = VmConfig::builder()
            .mount(VirtiofsMount {
                tag: MountTag::new("rw0").unwrap(),
                host_path: tmp.path().to_path_buf(),
                read_only: false,
                dax: false,
            })
            .build();
        let mounts = translate_mounts(&cfg).expect("valid mount");
        assert_eq!(mounts.len(), 1);
        assert!(!mounts[0].read_only, "RW flag must be preserved");
    }

    // ── Vsock → PublishedSocketConfig translation ────────────────────────────

    /// `VsockPort { listen: true }` (host-listener) maps to `Mount` direction
    /// with `IR_STREAM_GUEST_SOCKET` as guest_path and `socket_path` as host_path.
    #[test]
    fn vsock_listen_true_maps_to_mount_direction() {
        use crate::vm::IR_STREAM_GUEST_SOCKET;
        use smolvm::config::SocketDirection;

        let port = VsockPort {
            port: 4100, // advisory only on this backend; smolvm assigns the real port
            socket_path: PathBuf::from("/run/jobs/42/ir-stream.sock"),
            listen: true,
        };
        let result = translate_vsock_ports(&[port]).expect("listen:true must succeed");
        assert_eq!(result.len(), 1);
        let cfg = &result[0];
        assert_eq!(
            cfg.direction,
            SocketDirection::Mount,
            "host-listener = Mount direction (guest connects in)"
        );
        assert_eq!(
            cfg.guest_path, IR_STREAM_GUEST_SOCKET,
            "guest path must be the canonical IR-stream socket"
        );
        assert_eq!(
            cfg.host_path.as_deref(),
            Some("/run/jobs/42/ir-stream.sock"),
            "host_path must be the facade socket_path"
        );
    }

    /// Multiple `listen: true` ports translate to a Vec with one entry per port,
    /// each with the canonical guest_path (the assignment of distinct vsock port
    /// numbers is smolvm's responsibility at boot time).
    #[test]
    fn multiple_vsock_ports_each_translate_independently() {
        use crate::vm::IR_STREAM_GUEST_SOCKET;

        let ports = vec![
            VsockPort {
                port: 4100,
                socket_path: PathBuf::from("/run/jobs/1/ir.sock"),
                listen: true,
            },
            VsockPort {
                port: 4101,
                socket_path: PathBuf::from("/run/jobs/2/ir.sock"),
                listen: true,
            },
        ];
        let result = translate_vsock_ports(&ports).expect("all listen:true");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].host_path.as_deref(), Some("/run/jobs/1/ir.sock"));
        assert_eq!(result[1].host_path.as_deref(), Some("/run/jobs/2/ir.sock"));
        assert!(
            result
                .iter()
                .all(|c| c.guest_path == IR_STREAM_GUEST_SOCKET),
            "all entries must use the canonical guest path"
        );
    }

    /// `VsockPort { listen: false }` (Expose direction — guest listens, host
    /// connects) is rejected with `VmError::Unsupported` (gap V-VSOCK-EXPOSE-1).
    /// No current cage use case exists for this direction.
    #[test]
    fn vsock_listen_false_is_rejected_as_unsupported() {
        let port = VsockPort {
            port: 9000,
            socket_path: PathBuf::from("/tmp/host-connects.sock"),
            listen: false,
        };
        let err = translate_vsock_ports(&[port]).expect_err("Expose direction must be rejected");
        assert!(
            matches!(err, VmError::Unsupported { .. }),
            "expected Unsupported (V-VSOCK-EXPOSE-1), got {err:?}"
        );
    }

    /// Empty port list translates to an empty PublishedSocketConfig vec.
    #[test]
    fn empty_vsock_ports_translates_to_empty_vec() {
        let result = translate_vsock_ports(&[]).expect("empty is always ok");
        assert!(result.is_empty());
    }

    // ── Rlimit wrapper ───────────────────────────────────────────────────────

    #[test]
    fn rlimit_wrapper_produces_sh_c_ulimit_exec() {
        let spec = RunSpec {
            command: PathBuf::from("/usr/bin/producer"),
            args: vec!["--lower".into(), "my arg with spaces".into()],
            env: vec![],
            cwd: None,
            timeout: std::time::Duration::from_secs(60),
            rlimits: GuestRlimits {
                cpu_secs: NonZeroU64::new(900).unwrap(),
                pids: NonZeroU32::new(64).unwrap(),
                nofile: NonZeroU64::new(1024).unwrap(),
                fsize_bytes: NonZeroU64::new(512 * 1024).unwrap(),
                mem_bytes: NonZeroU64::new(6 * 1024 * 1024 * 1024).unwrap(),
            },
        };
        let cmd = build_command_with_rlimits(&spec);
        assert_eq!(cmd[0], "sh");
        assert_eq!(cmd[1], "-c");
        let script = &cmd[2];
        assert!(script.contains("ulimit"), "must include ulimit prefix");
        assert!(script.contains("exec"), "must exec into the real command");
        assert!(script.contains("/usr/bin/producer"), "must include command");
        assert!(script.contains("my arg with spaces"), "must include args");
    }

    // ── RootfsStore ──────────────────────────────────────────────────────────

    #[test]
    fn rootfs_store_from_env_missing_returns_invalid_config() {
        // Temporarily ensure the var is unset (save/restore).
        let saved = std::env::var_os("NUDOX_GUEST_ROOTFS");
        // SAFETY: single-threaded test process; no other thread reads this var concurrently.
        unsafe { std::env::remove_var("NUDOX_GUEST_ROOTFS") };
        let err = RootfsStore::from_env().expect_err("must fail when unset");
        assert!(matches!(err, VmError::InvalidConfig { .. }));
        if let Some(v) = saved {
            // SAFETY: same rationale as above.
            unsafe { std::env::set_var("NUDOX_GUEST_ROOTFS", v) };
        }
    }

    #[test]
    fn rootfs_store_resolves_base_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().join("base");
        std::fs::create_dir_all(&base).unwrap();

        let store = RootfsStore {
            root: tmp.path().to_path_buf(),
        };
        let resolved = store.resolve(None).expect("base must resolve");
        assert_eq!(resolved, base);
    }

    #[test]
    fn rootfs_store_missing_image_returns_rootfs_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = RootfsStore {
            root: tmp.path().to_path_buf(),
        };
        let digest = ImageDigest::from_bytes([0xAB; 32]);
        let err = store.resolve(Some(&digest)).expect_err("must fail");
        assert!(matches!(err, VmError::RootfsUnavailable { .. }));
    }

    // ── VmConfig.image rootfs selection ─────────────────────────────────────

    /// `VmConfig.image = None` → `base/` subdir is used.
    #[test]
    fn launch_resolves_base_rootfs_when_image_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("base")).unwrap();
        let store = RootfsStore {
            root: tmp.path().to_path_buf(),
        };
        // store.resolve(None) must succeed → picks base/
        let path = store.resolve(None).expect("base must exist");
        assert!(path.ends_with("base"));
    }

    /// `VmConfig.image = Some(digest)` and the digest subdir exists → resolves.
    #[test]
    fn launch_resolves_image_digest_subdir_when_present() {
        let tmp = tempfile::TempDir::new().unwrap();
        let digest = ImageDigest::from_bytes([0xCC; 32]);
        let subdir = tmp.path().join(digest.to_string());
        std::fs::create_dir_all(&subdir).unwrap();
        let store = RootfsStore {
            root: tmp.path().to_path_buf(),
        };
        let path = store.resolve(Some(&digest)).expect("image dir must exist");
        assert_eq!(path, subdir);
    }

    /// `VmConfig.image = Some(digest)` and the digest subdir is absent →
    /// `VmError::RootfsUnavailable` (never falls back to base/).
    #[test]
    fn launch_returns_rootfs_unavailable_for_missing_digest_subdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        // base/ exists so only the image-path failure is tested.
        std::fs::create_dir_all(tmp.path().join("base")).unwrap();
        let store = RootfsStore {
            root: tmp.path().to_path_buf(),
        };
        let digest = ImageDigest::from_bytes([0xDD; 32]);
        let err = store
            .resolve(Some(&digest))
            .expect_err("missing dir must fail");
        assert!(
            matches!(err, VmError::RootfsUnavailable { .. }),
            "expected RootfsUnavailable, got {err:?}"
        );
    }

    /// `SmolvmRuntime::launch` with `VmConfig.image = Some(missing_digest)` must
    /// return `VmError::RootfsUnavailable` (exercises the full launch path).
    #[test]
    fn runtime_launch_with_missing_image_returns_rootfs_unavailable() {
        let store = make_temp_store(); // has base/ but no image dirs
        let rt = SmolvmRuntime::new(store);
        let digest = ImageDigest::from_bytes([0xEE; 32]);
        let cfg = VmConfig::builder().image(digest).build();
        let err = rt.launch(&cfg).expect_err("missing image must fail");
        assert!(
            matches!(err, VmError::RootfsUnavailable { .. }),
            "expected RootfsUnavailable, got {err:?}"
        );
    }

    // ── Smoke test (real VM boot, requires libkrun) ───────────────────────────

    /// Attempt a real VM launch against a temp rootfs; asserts either success or a
    /// typed libkrun-unavailable error. Requires the smolvm binary on PATH and a
    /// pre-built guest rootfs pointed to by `NUDOX_GUEST_ROOTFS`.
    #[test]
    #[ignore = "requires libkrun binaries + NUDOX_GUEST_ROOTFS + smolvm binary on PATH"]
    fn smoke_real_launch_succeeds_or_typed_unavailable() {
        let store = RootfsStore::from_env().expect("NUDOX_GUEST_ROOTFS must be set");
        let rt = SmolvmRuntime::new(store);
        let cfg = VmConfig::builder()
            .cpus(NonZeroU32::new(1).unwrap())
            .memory_mib(NonZeroU64::new(512).unwrap())
            .build();

        match rt.launch(&cfg) {
            Ok(mut handle) => {
                // VM booted: run a trivial command
                let spec = RunSpec {
                    command: PathBuf::from("/bin/true"),
                    args: vec![],
                    env: vec![],
                    cwd: None,
                    timeout: Duration::from_secs(10),
                    rlimits: GuestRlimits {
                        cpu_secs: NonZeroU64::new(10).unwrap(),
                        pids: NonZeroU32::new(16).unwrap(),
                        nofile: NonZeroU64::new(64).unwrap(),
                        fsize_bytes: NonZeroU64::new(512 * 1024).unwrap(),
                        mem_bytes: NonZeroU64::new(256 * 1024 * 1024).unwrap(),
                    },
                };
                let out = handle.exec(&spec).expect("exec should succeed");
                handle.kill();
                assert!(out.success(), "trivial /bin/true must exit 0");
            }
            Err(VmError::Launch { reason }) if reason.contains("libkrun") => {
                // libkrun not installed — acceptable on CI without the binary deps
                eprintln!("libkrun unavailable (expected on CI): {reason}");
            }
            Err(VmError::Launch { reason })
                if reason.contains("smolvm") || reason.contains("not found") =>
            {
                eprintln!("smolvm binary not on PATH (expected on CI): {reason}");
            }
            Err(e) => panic!("unexpected launch error: {e:?}"),
        }
    }

    // ── Golden fork (V-GOLD-1) ────────────────────────────────────────────────

    /// The golden VM name is deterministic per image digest and DNS/name-safe
    /// (no `sha256:` prefix, which smolvm's `validate_vm_name` rejects).
    #[test]
    fn golden_vm_name_is_deterministic_and_name_safe() {
        let a = ImageDigest::from_bytes([0xAB; 32]);
        let b = ImageDigest::from_bytes([0xCD; 32]);
        assert_eq!(golden_vm_name(&a), golden_vm_name(&a), "deterministic");
        assert_ne!(
            golden_vm_name(&a),
            golden_vm_name(&b),
            "distinct per digest"
        );
        let name = golden_vm_name(&a);
        assert!(
            !name.contains(':'),
            "must not carry the sha256: prefix: {name}"
        );
        assert!(name.starts_with("nudox-golden-"));
        assert!(
            name.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "name must be alphanumeric/dash/underscore: {name}"
        );
    }

    /// Each fork clone gets a process-unique name so concurrent jobs forking the
    /// same golden never collide on the clone's data dir / DB record.
    #[test]
    fn clone_names_are_unique_per_fork() {
        let rt = SmolvmRuntime::new(make_temp_store());
        let golden = "nudox-golden-abcd";
        let n1 = rt.next_clone_name(golden);
        let n2 = rt.next_clone_name(golden);
        assert_ne!(n1, n2, "monotonic counter must make clone names unique");
        assert!(
            n1.starts_with(golden),
            "clone name derives from golden: {n1}"
        );
        assert!(
            n1.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "clone name must be name-safe: {n1}"
        );
    }

    /// A fork against a golden that isn't running forkable (no live control
    /// socket) fails with a typed error rather than silently cold-booting — the
    /// no-silent-degrade rule. Requires the smolvm machine registry to be
    /// openable; skips cleanly if it is not (e.g. sandboxed CI with no HOME).
    #[test]
    fn fork_golden_without_running_golden_is_typed_error() {
        let rt = SmolvmRuntime::new(make_temp_store());
        // A golden id that was never prepared / has no live control socket.
        let golden = GoldenId::new("nudox-golden-deadbeefdeadbeefdeadbeefdeadbeef");
        match rt.fork_golden(&golden) {
            Ok(_) => panic!("forking a non-running golden must not succeed"),
            // Registry unopenable in this environment → acceptable skip.
            Err(VmError::Launch { reason }) if reason.contains("SmolvmDb") => {
                eprintln!("smolvm registry unavailable (expected in some CI): {reason}");
            }
            // prepare_fork rejects a golden with no live/forkable control socket.
            Err(VmError::Launch { .. }) | Err(VmError::Unsupported { .. }) => {}
            Err(e) => panic!("unexpected fork error: {e:?}"),
        }
    }

    // ── Helper ───────────────────────────────────────────────────────────────

    fn make_temp_store() -> RootfsStore {
        let tmp = tempfile::TempDir::new().unwrap();
        // Create base/ so rootfs resolution succeeds and tests reach the
        // resource-translation layer they intend to exercise.
        std::fs::create_dir_all(tmp.path().join("base")).unwrap();
        // Keep temp dir alive by leaking it (test-only).
        let path = tmp.keep();
        RootfsStore { root: path }
    }
}
