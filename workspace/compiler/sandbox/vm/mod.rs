//! Strongly-typed microVM facade mirroring smolvm's configuration shapes.
//!
//! This module is the compile target for the cage layer while the real
//! smolvm backend is not yet vendored: the types here mirror the upstream
//! shapes at the pinned commit (`smol-machines/smolvm @ 56bb13b`) —
//! `VmConfig::builder`, `VsockPort { port, socket_path, listen }`,
//! `NetworkPolicy::{None, Egress}`, virtiofs mounts with per-mount RO/RW and
//! DAX, and qcow2 scratch overlays. When the `smolvm-backend` feature grows a
//! real implementation, it implements [`VmRuntime`] / [`VmHandle`] over these
//! same types; nothing above this module changes (SMOLVM-PLAN §3.1).
//!
//! [`FakeVmRuntime`] is an always-available recording runtime used by the
//! budget→`VmConfig` projection tests in [`crate::cage::smolvm`].

use std::ffi::OsString;
use std::net::Ipv4Addr;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::spec::Output;

pub mod fake;

pub use fake::{FakeVmHandle, FakeVmRuntime};

// ─── IR stream vsock ─────────────────────────────────────────────────────────

/// In-guest path that producers connect to for IR streaming.
///
/// On the smolvm backend this is the guest-side endpoint of a `Mount`-direction
/// `PublishedSocketConfig` bridge: smolvm's guest agent creates a Unix listener
/// here and relays each producer connection through the vsock port that libkrun
/// bridges to the host-side `StreamPort.socket_path`.
///
/// The host end is `StreamPort.socket_path`.  smolvm assigns the vsock port
/// automatically (`PUBLISH_SOCKET_BASE + i`, starting at 6100) — that is not
/// ours to choose.  The `ir-stream` crate's `IR_STREAM_VSOCK_PORT` constant
/// (4100) is advisory for raw-vsock transports and is **not** used on this
/// backend.
pub const IR_STREAM_GUEST_SOCKET: &str = "/run/nudox/ir-stream.sock";

/// A host-side Unix socket bridged to one guest vsock port for IR streaming
/// (SMOLVM-PLAN §6 / SV-8).
///
/// The host listens on the socket (`VsockPort { listen: true }`); the guest
/// producer connects and emits `ir-stream` postcard frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPort {
    /// Guest-side vsock port number for the ir-stream channel.
    pub port: u32,
    /// Host-side Unix socket path the host listens on.
    pub socket_path: PathBuf,
}

// ─── Mounts ─────────────────────────────────────────────────────────────────

/// Validated virtiofs mount tag (the name the guest mounts by).
///
/// Tags are short identifiers: non-empty, at most 36 bytes, ASCII
/// alphanumeric plus `-` / `_`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MountTag(String);

impl MountTag {
    /// Validate and construct a mount tag.
    pub fn new(tag: impl Into<String>) -> Result<Self, VmError> {
        let tag = tag.into();
        let ok = !tag.is_empty()
            && tag.len() <= 36
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok {
            Ok(Self(tag))
        } else {
            Err(VmError::InvalidConfig {
                reason: format!("invalid mount tag `{tag}`"),
            })
        }
    }

    /// Borrow the tag string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MountTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One virtiofs host-directory mount into the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtiofsMount {
    /// Guest-visible mount tag (one tag per root).
    pub tag: MountTag,
    /// Host directory to expose.
    pub host_path: PathBuf,
    /// Whether the guest sees the mount read-only.
    pub read_only: bool,
    /// Whether a DAX window is mapped (fast repeated reads of big trees).
    pub dax: bool,
}

/// Lifetime of the guest's writable scratch disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OverlayMode {
    /// qcow2 CoW overlay destroyed when the VM exits (the sealed default —
    /// nothing the job writes survives it).
    #[default]
    Ephemeral,
    /// Overlay persisted at the given host path (golden-image preparation).
    Persistent(PathBuf),
}

/// The guest's writable scratch surface: a qcow2 overlay, not a host bind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScratchOverlay {
    /// Ephemeral (auto-destroyed) or persistent overlay.
    pub mode: OverlayMode,
}

// ─── Network ────────────────────────────────────────────────────────────────

/// An IPv4 CIDR block (address + prefix length), e.g. `10.0.0.0/8`.
///
/// Host bits below the prefix must be zero; parsing and construction reject
/// anything else so an allowlist entry always names a whole block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cidr {
    addr: Ipv4Addr,
    prefix: u8,
}

impl Cidr {
    /// The whole IPv4 space (`0.0.0.0/0`) — the permissive acquiring default.
    pub const ANY_V4: Self = Self {
        addr: Ipv4Addr::UNSPECIFIED,
        prefix: 0,
    };

    /// Construct from an address and prefix length, validating that `prefix`
    /// is at most 32 and that no host bit is set.
    pub fn new(addr: Ipv4Addr, prefix: u8) -> Result<Self, VmError> {
        if prefix > 32 {
            return Err(VmError::InvalidConfig {
                reason: format!("cidr prefix {prefix} exceeds 32"),
            });
        }
        let bits = u32::from(addr);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(prefix))
        };
        if bits & !mask != 0 {
            return Err(VmError::InvalidConfig {
                reason: format!("cidr {addr}/{prefix} has non-zero host bits"),
            });
        }
        Ok(Self { addr, prefix })
    }

    /// The network address.
    pub fn addr(&self) -> Ipv4Addr {
        self.addr
    }

    /// The prefix length.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }
}

impl std::str::FromStr for Cidr {
    type Err = VmError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = s.split_once('/').ok_or_else(|| VmError::InvalidConfig {
            reason: format!("cidr `{s}` missing `/prefix`"),
        })?;
        let addr: Ipv4Addr = addr.parse().map_err(|e| VmError::InvalidConfig {
            reason: format!("cidr `{s}`: {e}"),
        })?;
        let prefix: u8 = prefix.parse().map_err(|e| VmError::InvalidConfig {
            reason: format!("cidr `{s}`: {e}"),
        })?;
        Self::new(addr, prefix)
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// A DNS name allowed through the host-side DNS filter.
///
/// Validated: non-empty, at most 253 bytes, dot-separated labels of ASCII
/// alphanumerics / hyphens; a single leading `*.` wildcard label is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DnsName(String);

impl DnsName {
    /// Validate and construct a DNS name.
    pub fn new(name: impl Into<String>) -> Result<Self, VmError> {
        let name = name.into();
        let body = name.strip_prefix("*.").unwrap_or(&name);
        let ok = !body.is_empty()
            && name.len() <= 253
            && body.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            });
        if ok {
            Ok(Self(name))
        } else {
            Err(VmError::InvalidConfig {
                reason: format!("invalid dns name `{name}`"),
            })
        }
    }

    /// Borrow the name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DnsName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host-side DNS filter policy for an egress-enabled VM.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DnsPolicy {
    /// No DNS resolution at all.
    #[default]
    Deny,
    /// Resolve any name (permissive acquiring default).
    AllowAll,
    /// Resolve only the listed names.
    Allowlist(Vec<DnsName>),
}

/// Egress configuration for a network-enabled VM.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EgressPolicy {
    /// CIDR blocks outbound connections may target.
    pub allowed_cidrs: Vec<Cidr>,
    /// DNS filter applied host-side.
    pub dns: DnsPolicy,
}

/// Guest network posture — mirrors smolvm's `NetworkPolicy`.
///
/// `None` means the machine is constructed without a NIC or TSI: the network
/// is *absent*, not filtered. Sealed budgets can only ever project to `None`
/// (see [`crate::cage::smolvm`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// No network device exists; vsock only.
    #[default]
    None,
    /// Outbound-only egress under an allowlist + DNS filter.
    Egress(EgressPolicy),
}

// ─── Vsock / config ─────────────────────────────────────────────────────────

/// A guest vsock port bridged to a host-side Unix socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockPort {
    /// Guest-side vsock port number.
    pub port: u32,
    /// Host-side Unix socket path.
    pub socket_path: PathBuf,
    /// Whether the host listens (guest connects) rather than the reverse.
    pub listen: bool,
}

/// Opaque kernel / agent knobs.
///
/// The pinned smolvm chooses the libkrunfw kernel and the `smolvm-agent`
/// init; we deliberately do not model those knobs until the backend is
/// vendored — the default is always correct for the cage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GuestKnobs {
    _private: (),
}

/// Full machine configuration for one microVM.
///
/// Build via [`VmConfig::builder`]. Fields are public for inspection
/// (projection tests read them back out of [`FakeVmRuntime`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// Virtual CPU count.
    pub cpus: NonZeroU32,
    /// Guest memory ceiling in MiB (virtio balloon reclaims idle pages).
    pub memory_mib: NonZeroU64,
    /// virtiofs host mounts.
    pub mounts: Vec<VirtiofsMount>,
    /// Writable scratch overlay.
    pub scratch: ScratchOverlay,
    /// Network posture.
    pub network: NetworkPolicy,
    /// Extra vsock ports (IR streaming, control).
    pub vsock_ports: Vec<VsockPort>,
    /// Opaque kernel/agent knobs.
    pub knobs: GuestKnobs,
    /// OCI toolchain image digest selecting the rootfs to boot.
    ///
    /// `Some(digest)` → the `sha256:<hex>/` subdirectory under
    /// `NUDOX_GUEST_ROOTFS` (absent directory → `VmError::RootfsUnavailable`).
    /// `None` → the `base/` subdirectory (default Alpine + smolvm-agent).
    ///
    /// `project_vm_config_with_stream` leaves this as `None`; callers that want
    /// a language-specific toolchain image must set it via
    /// [`VmConfigBuilder::image`] after projection.
    pub image: Option<crate::toolchains::images::ImageDigest>,
}

impl VmConfig {
    /// Start a builder with 1 vCPU / 512 MiB and everything else empty/off.
    pub fn builder() -> VmConfigBuilder {
        VmConfigBuilder::default()
    }
}

/// Builder for [`VmConfig`].
#[derive(Debug, Clone)]
pub struct VmConfigBuilder {
    cpus: NonZeroU32,
    memory_mib: NonZeroU64,
    mounts: Vec<VirtiofsMount>,
    scratch: ScratchOverlay,
    network: NetworkPolicy,
    vsock_ports: Vec<VsockPort>,
    image: Option<crate::toolchains::images::ImageDigest>,
}

impl Default for VmConfigBuilder {
    fn default() -> Self {
        Self {
            cpus: NonZeroU32::MIN,
            memory_mib: NonZeroU64::new(512).expect("non-zero"),
            mounts: Vec::new(),
            scratch: ScratchOverlay::default(),
            network: NetworkPolicy::None,
            vsock_ports: Vec::new(),
            image: None,
        }
    }
}

impl VmConfigBuilder {
    /// Set the vCPU count.
    pub fn cpus(mut self, cpus: NonZeroU32) -> Self {
        self.cpus = cpus;
        self
    }

    /// Set the memory ceiling in MiB.
    pub fn memory_mib(mut self, memory_mib: NonZeroU64) -> Self {
        self.memory_mib = memory_mib;
        self
    }

    /// Add one virtiofs mount.
    pub fn mount(mut self, mount: VirtiofsMount) -> Self {
        self.mounts.push(mount);
        self
    }

    /// Set the scratch overlay.
    pub fn scratch(mut self, scratch: ScratchOverlay) -> Self {
        self.scratch = scratch;
        self
    }

    /// Set the network posture.
    pub fn network(mut self, network: NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    /// Add a vsock port.
    pub fn vsock_port(mut self, port: VsockPort) -> Self {
        self.vsock_ports.push(port);
        self
    }

    /// Set the OCI toolchain image digest for rootfs selection.
    ///
    /// `Some(digest)` → the `sha256:<hex>/` subdirectory under
    /// `NUDOX_GUEST_ROOTFS`.  `None` (the default) → `base/`.
    pub fn image(mut self, digest: crate::toolchains::images::ImageDigest) -> Self {
        self.image = Some(digest);
        self
    }

    /// Finish the configuration.
    pub fn build(self) -> VmConfig {
        VmConfig {
            cpus: self.cpus,
            memory_mib: self.memory_mib,
            mounts: self.mounts,
            scratch: self.scratch,
            network: self.network,
            vsock_ports: self.vsock_ports,
            knobs: GuestKnobs::default(),
            image: self.image,
        }
    }
}

// ─── Exec ───────────────────────────────────────────────────────────────────

/// Rlimits the in-guest agent applies before exec (belt-and-braces under the
/// VM's own memory ceiling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestRlimits {
    /// CPU seconds (`RLIMIT_CPU`).
    pub cpu_secs: NonZeroU64,
    /// Process budget inside the guest.
    pub pids: NonZeroU32,
    /// Open file descriptor cap.
    pub nofile: NonZeroU64,
    /// Single-file size cap (`RLIMIT_FSIZE`).
    pub fsize_bytes: NonZeroU64,
    /// In-guest memory rlimit — the VM's `memory_mib` is the real ceiling.
    pub mem_bytes: NonZeroU64,
}

/// One in-guest exec: the agent runs exactly this, with exactly this env.
///
/// `env` is the **only** environment the guest process sees — the host
/// environment never leaks across the vsock boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    /// Guest path of the program to exec.
    pub command: PathBuf,
    /// Arguments (not including argv0).
    pub args: Vec<OsString>,
    /// The complete guest environment (allowlist, verbatim).
    pub env: Vec<(String, String)>,
    /// Working directory inside the guest.
    pub cwd: Option<PathBuf>,
    /// Wall-clock budget for the exec; the host kills the VM past it.
    pub timeout: Duration,
    /// Rlimits the agent applies before exec.
    pub rlimits: GuestRlimits,
}

// ─── Runtime abstraction ────────────────────────────────────────────────────

/// Opaque identity of a golden (checkpointed, warm) VM image.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GoldenId(String);

impl GoldenId {
    /// Construct from an opaque backend-issued identifier.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GoldenId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A live (or fake) microVM the cage can exec into and kill.
pub trait VmHandle {
    /// Run one command inside the guest and capture its output.
    fn exec(&mut self, spec: &RunSpec) -> Result<Output, VmError>;

    /// Tear the machine down immediately (cancellation / end of job). The
    /// ephemeral overlay is destroyed with it. Idempotent.
    fn kill(&mut self);
}

/// Launches microVMs from configs or golden checkpoints.
///
/// The real implementation embeds smolvm's `EmbeddedRuntime` behind the
/// `smolvm-backend` feature; [`FakeVmRuntime`] records instead of booting.
pub trait VmRuntime: Send + Sync {
    /// The handle type produced by this runtime.
    type Handle: VmHandle;

    /// Cold-boot a VM from `cfg`.
    fn launch(&self, cfg: &VmConfig) -> Result<Self::Handle, VmError>;

    /// CoW-fork a warm VM from a golden checkpoint (~250 ms restore).
    fn fork_golden(&self, golden: &GoldenId) -> Result<Self::Handle, VmError>;

    /// Checkpoint a prepared VM into a golden image, consuming the handle.
    fn checkpoint(&self, handle: Self::Handle) -> Result<GoldenId, VmError>;
}

/// Failures from the VM runtime layer.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VmError {
    /// A config value failed validation before launch.
    #[error("invalid vm config: {reason}")]
    InvalidConfig {
        /// What was wrong.
        reason: String,
    },

    /// The hypervisor refused to boot the machine.
    #[error("vm launch failed: {reason}")]
    Launch {
        /// Backend detail.
        reason: String,
    },

    /// The in-guest exec failed at the agent/protocol level.
    #[error("vm exec failed: {reason}")]
    Exec {
        /// Backend detail.
        reason: String,
    },

    /// The requested golden checkpoint does not exist.
    #[error("golden image missing: {id}")]
    GoldenMissing {
        /// The missing golden id.
        id: String,
    },

    /// This host / build cannot provide the requested operation.
    #[error("vm unsupported: {reason}")]
    Unsupported {
        /// Why the operation is unavailable.
        reason: String,
    },

    /// The requested rootfs directory is absent or not yet materialised.
    ///
    /// `digest` is a human-readable path or digest string to aid diagnosis;
    /// it is not necessarily a valid [`crate::toolchains::images::ImageDigest`].
    #[error("rootfs unavailable: {digest}")]
    RootfsUnavailable {
        /// The missing path or digest.
        digest: String,
    },

    /// I/O talking to the VMM or its sockets.
    #[error("vm I/O error")]
    Io(#[from] std::io::Error),
}

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_parse_display_roundtrip() {
        let c: Cidr = "10.0.0.0/8".parse().expect("valid cidr");
        assert_eq!(c.addr(), Ipv4Addr::new(10, 0, 0, 0));
        assert_eq!(c.prefix(), 8);
        assert_eq!(c.to_string(), "10.0.0.0/8");
    }

    #[test]
    fn cidr_rejects_bad_prefix_and_host_bits() {
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("10.0.0.1/8".parse::<Cidr>().is_err(), "host bits set");
        assert!("10.0.0.0".parse::<Cidr>().is_err(), "missing prefix");
        assert!("banana/8".parse::<Cidr>().is_err());
    }

    #[test]
    fn cidr_any_is_valid_and_zero() {
        assert_eq!(Cidr::ANY_V4.to_string(), "0.0.0.0/0");
        assert_eq!(
            Cidr::new(Ipv4Addr::UNSPECIFIED, 0).expect("any"),
            Cidr::ANY_V4
        );
    }

    #[test]
    fn dns_name_validation() {
        assert!(DnsName::new("crates.io").is_ok());
        assert!(DnsName::new("*.github.com").is_ok());
        assert!(DnsName::new("").is_err());
        assert!(DnsName::new("*.").is_err());
        assert!(DnsName::new("-bad.example").is_err());
        assert!(DnsName::new("sp ace.example").is_err());
    }

    #[test]
    fn mount_tag_validation() {
        assert!(MountTag::new("ro0").is_ok());
        assert!(MountTag::new("with-dash_and_underscore").is_ok());
        assert!(MountTag::new("").is_err());
        assert!(MountTag::new("has space").is_err());
    }

    #[test]
    fn builder_defaults_are_closed() {
        let cfg = VmConfig::builder().build();
        assert_eq!(cfg.network, NetworkPolicy::None, "network off by default");
        assert!(cfg.mounts.is_empty());
        assert_eq!(cfg.scratch.mode, OverlayMode::Ephemeral);
    }
}
