//! Backend selection and implementations.

mod linux;
mod macos;
mod nix_derivation;
mod passthrough;
pub(crate) mod supervisor;

pub use linux::{run_direct_hardened, LinuxBwrap};
pub use macos::MacSeatbelt;
pub use nix_derivation::NixDerivation;
pub use passthrough::Passthrough;

use crate::error::SandboxError;
use crate::spec::{Output, Spec};

/// What a backend can enforce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
	/// Namespaces / seatbelt profile available.
	pub isolation: bool,
	/// Network can be forced off.
	pub network_off: bool,
	/// Landlock or equivalent FS scoping.
	pub fs_scope: bool,
	/// seccomp denylist.
	pub seccomp: bool,
	/// cgroup resource control.
	pub cgroups: bool,
	/// Suitable as production security boundary of record.
	pub production_grade: bool,
}

/// Execution backend.
pub trait Backend: Send + Sync {
	/// Short name for logs/metrics.
	fn name(&self) -> &'static str;

	/// Probe static capabilities of this backend on the host.
	fn capabilities(&self) -> Capabilities;

	/// Run `spec` to completion (or resource kill).
	fn run(&self, spec: Spec) -> Result<Output, SandboxError>;
}

/// Owned selected backend (type-erased).
pub struct Selected {
	inner: Box<dyn Backend>,
}

impl Selected {
	/// Borrow the backend.
	pub fn as_ref(&self) -> &dyn Backend {
		self.inner.as_ref()
	}
}

/// Auto-select the strongest available backend.
///
/// Order:
/// - `NUDOX_SANDBOX=nix` → [`NixDerivation`] when `nix` works
/// - Linux: LinuxBwrap → Passthrough
/// - macOS: MacSeatbelt only if `NUDOX_SANDBOX=seatbelt` (accident-prevention);
///   default is Passthrough for speed (design §12). Prod security is Linux.
pub fn select() -> Selected {
	if nix_derivation::prefer_nix_derivation() {
		let nix = NixDerivation::new();
		if nix.probe_available() {
			tracing::info!(backend = nix.name(), "sandbox backend selected");
			return Selected {
				inner: Box::new(nix),
			};
		}
		tracing::warn!("NUDOX_SANDBOX=nix but nix CLI unavailable; falling through");
	}

	#[cfg(target_os = "linux")]
	{
		let bwrap = LinuxBwrap::new();
		if bwrap.probe_available() {
			tracing::info!(backend = bwrap.name(), "sandbox backend selected");
			return Selected {
				inner: Box::new(bwrap),
			};
		}
		tracing::warn!("bwrap unavailable; falling back");
	}

	#[cfg(target_os = "macos")]
	{
		let want_seatbelt = matches!(
			std::env::var("NUDOX_SANDBOX").as_deref(),
			Ok("seatbelt") | Ok("mac-seatbelt") | Ok("1")
		);
		if want_seatbelt {
			let seatbelt = MacSeatbelt::new();
			if seatbelt.probe_available() {
				tracing::info!(backend = seatbelt.name(), "sandbox backend selected (dev)");
				return Selected {
					inner: Box::new(seatbelt),
				};
			}
			tracing::warn!("NUDOX_SANDBOX=seatbelt but sandbox-exec missing");
		}
	}

	let pass = Passthrough::new();
	tracing::info!(
		backend = pass.name(),
		"sandbox backend selected (dev/unsupported)"
	);
	Selected {
		inner: Box::new(pass),
	}
}
