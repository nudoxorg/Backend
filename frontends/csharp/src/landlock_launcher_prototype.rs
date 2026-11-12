//! Experimental dedicated launcher. Not an authenticated production boundary.
#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
#[path = "legacy/landlock_candidate.rs"]
mod landlock_candidate;

#[cfg(target_os = "linux")]
mod linux {
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File},
        io::{Read, Seek, SeekFrom},
        os::unix::process::CommandExt,
        path::{Component, Path, PathBuf},
        process::Command,
    };

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Grant {
        path: PathBuf,
        sha256: String,
        execute: bool,
    }

    // These are explicitly requested, volatile kernel-resource observations;
    // they never become hashes or members of the SDK/native execution closure.
    #[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
    enum KernelResourceObservation {
        ProcessMemoryMap,
        SystemMemoryInfo,
        ProcessStatus,
        ProcessMountInfo,
        SystemCpuStat,
        CpuPossible,
        CpuOnline,
        RandomDevice,
    }

    impl KernelResourceObservation {
        fn path(self) -> &'static str {
            match self {
                Self::ProcessMemoryMap => "/proc/self/maps",
                Self::SystemMemoryInfo => "/proc/meminfo",
                Self::ProcessStatus => "/proc/self/stat",
                Self::ProcessMountInfo => "/proc/self/mountinfo",
                Self::SystemCpuStat => "/proc/stat",
                Self::CpuPossible => "/sys/devices/system/cpu/possible",
                Self::CpuOnline => "/sys/devices/system/cpu/online",
                Self::RandomDevice => "/dev/urandom",
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Policy {
        version: u32,
        nonce: String,
        program: PathBuf,
        cwd: PathBuf,
        args: Vec<String>,
        grants: Vec<Grant>,
        enumerate: Vec<PathBuf>,
        #[serde(default)]
        kernel_observations: Vec<KernelResourceObservation>,
    }

    fn absolute(path: &Path) -> bool {
        path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    }

    // Opening /proc/self/fd holds a directory descriptor while enumerating.
    // First collect candidate numbers, drop the iterator, then probe each;
    // any descriptor still open except stdin/stdout/stderr is a hard failure.
    // This is an audit, not close_range, and needs a Linux integration test.
    fn audit_fds() -> Result<(), ()> {
        let mut candidates = Vec::new();
        for entry in fs::read_dir("/proc/self/fd").map_err(|_| ())? {
            let name = entry.map_err(|_| ())?.file_name();
            let number: u32 = name.to_str().ok_or(())?.parse().map_err(|_| ())?;
            if number > 2 {
                candidates.push(number);
            }
            if candidates.len() > 8192 {
                return Err(());
            }
        }
        for number in candidates {
            let path = format!("/proc/self/fd/{number}");
            match fs::symlink_metadata(path) {
                Ok(_) => return Err(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(()),
            }
        }
        Ok(())
    }

    fn run() -> Result<(), ()> {
        let mut args = std::env::args_os().skip(1);
        let expected_nonce = args.next().ok_or(())?.into_string().map_err(|_| ())?;
        let manifest = PathBuf::from(args.next().ok_or(())?);
        if args.next().is_some()
            || expected_nonce.len() != 64
            || !expected_nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || !absolute(&manifest)
        {
            return Err(());
        }
        // Reject inherited descriptor authority before opening grants.
        audit_fds()?;
        let content =
            backend_platform::durable::read_regular_bounded_stable(&manifest, 1024 * 1024)
                .map_err(|_| ())?;
        let policy: Policy = serde_json::from_slice(&content).map_err(|_| ())?;
        if policy.version != 1
            || policy.nonce != expected_nonce
            || !absolute(&policy.program)
            || !absolute(&policy.cwd)
            || policy.grants.len() > 8192
            || policy.enumerate.len() > 8192
            || policy.args.len() > 4096
            || policy.args.iter().any(|arg| arg.len() > 4096)
        {
            return Err(());
        }
        let mut reads = Vec::new();
        let mut executables = Vec::new();
        let mut saw_program = false;
        for grant in &policy.grants {
            if !absolute(&grant.path) || grant.sha256.len() != 64 {
                return Err(());
            }
            // Refuse a final symlink; the stable source reader also checks a
            // held revision. A separate immutable custody proof is required.
            let content = backend_platform::durable::read_regular_bounded_stable(
                &grant.path,
                64 * 1024 * 1024,
            )
            .map_err(|_| ())?;
            if format!("{:x}", Sha256::digest(&content)) != grant.sha256 {
                return Err(());
            }
            let mut file = File::open(&grant.path).map_err(|_| ())?;
            let mut reread = Vec::new();
            file.by_ref()
                .take(64 * 1024 * 1024 + 1)
                .read_to_end(&mut reread)
                .map_err(|_| ())?;
            if reread != content {
                return Err(());
            }
            file.seek(SeekFrom::Start(0)).map_err(|_| ())?;
            if grant.execute {
                saw_program |= grant.path == policy.program;
                executables.push(file);
            } else {
                reads.push(file);
            }
        }
        if !saw_program {
            return Err(());
        }
        let mut directories = Vec::new();
        for path in &policy.enumerate {
            if !absolute(path) {
                return Err(());
            }
            directories.push(File::open(path).map_err(|_| ())?);
        }
        // This grant permits listing descriptor names only; no file reads are
        // authorized by it. The final audit runs after all grant FDs close.
        directories.push(File::open("/proc/self/fd").map_err(|_| ())?);
        if policy.kernel_observations.len() > 8 {
            return Err(());
        }
        let mut observations = Vec::new();
        for (index, item) in policy.kernel_observations.iter().enumerate() {
            if policy.kernel_observations[..index].contains(item) {
                return Err(());
            }
            observations.push(File::open(item.path()).map_err(|_| ())?);
        }
        let rules =
            super::landlock_candidate::prepare(&reads, &executables, &directories, &observations)?;
        drop(observations);
        drop(reads);
        drop(executables);
        drop(directories);
        super::landlock_candidate::enforce(rules)?;
        audit_fds()?;
        // No shell, no ambient environment, and exec replaces the dedicated
        // single-thread launcher; the copied host becomes /proc/self/exe.
        let error = Command::new(&policy.program)
            .args(&policy.args)
            .current_dir(&policy.cwd)
            .env_clear()
            .exec();
        let _ = error;
        Err(())
    }

    pub(super) fn main() {
        if run().is_err() {
            std::process::exit(125);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
    #[cfg(not(target_os = "linux"))]
    std::process::exit(125);
}
