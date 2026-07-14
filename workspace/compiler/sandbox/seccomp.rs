//! Seccomp-bpf denylist (Docker-profile style) for LPE-prone syscalls.
//!
//! Denylist, not allowlist: toolchains touch 200+ syscalls; allowlists are
//! too brittle.
//!
//! ## Where to install
//!
//! - **bwrap**: never install on the bwrap process itself — that breaks
//!   `unshare`/`mount` setup. Pass the compiled cBPF via `bwrap --seccomp FD`
//!   so the filter attaches **after** namespace construction (research finding).

use std::collections::BTreeMap;
use std::convert::TryInto;

use seccompiler::{apply_filter, BpfProgram, SeccompAction, SeccompFilter, TargetArch};

use crate::error::SandboxError;

/// Install the LPE denylist on the calling thread (direct-spawn path only).
pub fn apply_denylist() -> Result<(), SandboxError> {
	let prog = denylist_program()?;
	apply_filter(&prog).map_err(|e| SandboxError::Backend(format!("seccomp apply: {e}")))
}

/// Compile the denylist to a loadable BPF program.
pub fn denylist_program() -> Result<BpfProgram, SandboxError> {
	let arch = host_arch()?;
	// Empty Vec = match syscall with any args → match_action (EPERM).
	// mismatch_action = Allow (default-allow denylist).
	let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();
	for nr in denied_syscalls() {
		rules.insert(nr, Vec::new());
	}

	let filter = SeccompFilter::new(
		rules,
		SeccompAction::Allow,
		SeccompAction::Errno(libc::EPERM as u32),
		arch,
	)
	.map_err(|e| SandboxError::Backend(format!("seccomp filter: {e}")))?;

	filter
		.try_into()
		.map_err(|e| SandboxError::Backend(format!("seccomp bpf: {e}")))
}

/// Raw cBPF bytes for `bwrap --seccomp FD` (contiguous `sock_filter` records).
pub fn denylist_bpf_bytes() -> Result<Vec<u8>, SandboxError> {
	let prog = denylist_program()?;
	Ok(bpf_to_bytes(&prog))
}

/// Serialize a [`BpfProgram`] for bubblewrap / `SECCOMP_SET_MODE_FILTER`.
pub fn bpf_to_bytes(prog: &BpfProgram) -> Vec<u8> {
	// sock_filter is 8 bytes, #[repr(C)]: code:u16, jt:u8, jf:u8, k:u32
	let mut out = Vec::with_capacity(prog.len() * 8);
	for insn in prog {
		out.extend_from_slice(&insn.code.to_ne_bytes());
		out.push(insn.jt);
		out.push(insn.jf);
		out.extend_from_slice(&insn.k.to_ne_bytes());
	}
	out
}

fn host_arch() -> Result<TargetArch, SandboxError> {
	match std::env::consts::ARCH {
		"x86_64" => Ok(TargetArch::x86_64),
		"aarch64" => Ok(TargetArch::aarch64),
		other => Err(SandboxError::Backend(format!(
			"seccomp: unsupported arch {other}"
		))),
	}
}

/// Syscalls blocked for LPE / breakout mitigation (design §4).
/// Prefer `libc::SYS_*` so numbers match the execution ABI.
fn denied_syscalls() -> Vec<i64> {
	let mut nrs = vec![
		libc::SYS_bpf,
		libc::SYS_mount,
		libc::SYS_umount2,
		libc::SYS_ptrace,
		libc::SYS_process_vm_readv,
		libc::SYS_process_vm_writev,
		libc::SYS_kexec_load,
		libc::SYS_init_module,
		libc::SYS_finit_module,
		libc::SYS_delete_module,
		libc::SYS_open_by_handle_at,
		libc::SYS_name_to_handle_at,
		libc::SYS_unshare,
		libc::SYS_setns,
		libc::SYS_perf_event_open,
		libc::SYS_userfaultfd,
		libc::SYS_reboot,
		libc::SYS_swapon,
		libc::SYS_swapoff,
		libc::SYS_pivot_root,
	];

	// io_uring + new mount API — shared numbers on x86_64 and aarch64.
	// Prefer libc when present; fall back to the generic table.
	#[cfg(target_os = "linux")]
	{
		nrs.push(libc::SYS_io_uring_setup);
		nrs.push(libc::SYS_io_uring_enter);
		nrs.push(libc::SYS_io_uring_register);
		// New mount API (may be missing on older libc)
		nrs.extend_from_slice(&[
			428, // open_tree
			429, // move_mount
			430, // fsopen
			431, // fsconfig
			432, // fsmount
			433, // fspick
			442, // mount_setattr
		]);
	}

	// kexec_file_load — arch-specific; libc may lack the constant.
	#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
	nrs.push(320);
	#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
	nrs.push(294);

	nrs
}
