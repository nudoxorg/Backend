//! Bounded hashing of an already selected ambient-ignore path set.
//!
//! This does not select the paths or prove the environment. Callers must bind
//! the sorted unique path list to the exact discovery policy and environment,
//! and must hold any reservations for those path owners during this call.
use backend_engine::workspace::retirement_budget::RetirementBudget;
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const FILE_LIMIT: u64 = 1024 * 1024;
const TOTAL_LIMIT: u64 = 16 * 1024 * 1024;
const PATH_LIMIT: usize = 4096;

/// Computes the canonical `AmbientIgnoreInputs::canonical_bytes()` hash
/// without retaining file contents or constructing the canonical byte vector.
/// Paths must be the exact sorted, deduplicated path set selected by discovery.
/// A detected change, unsupported path, failed reservation or I/O fails closed.
/// A caller must separately establish the lifetime and coherence of the
/// filesystem view; metadata comparisons cannot exclude every possible race.
#[cfg(unix)]
pub(super) fn hash_selected_paths(
    paths: &[&Path],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<[u8; 32], &'static str> {
    hash_selected_paths_impl(
        paths,
        budget,
        allocation_bookkeeping_bytes,
        cancelled,
        false,
    )
}

#[cfg(unix)]
fn hash_selected_paths_impl(
    paths: &[&Path],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
    require_absent: bool,
) -> Result<[u8; 32], &'static str> {
    use std::os::unix::fs::MetadataExt;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.ambient-ignore-inputs.v1\0");
    hash.update(
        &(u64::try_from(paths.len()).map_err(|_| "ambient path count overflow")?).to_be_bytes(),
    );
    let mut total = 0u64;
    let mut prior: Option<&Path> = None;
    let mut buffer = [0u8; 8192];
    for path in paths {
        if cancelled.load(Ordering::Acquire) {
            return Err("ambient capture cancelled");
        }
        if prior.is_some_and(|p| p >= *path) {
            return Err("ambient paths not strictly ordered");
        }
        prior = Some(path);
        let name = path.as_os_str().as_encoded_bytes();
        if name.len() > PATH_LIMIT || name.contains(&0) {
            return Err("ambient path unsupported");
        }
        // Unix path conversion can materialize a NUL-terminated copy. Reserve
        // this before *each* std filesystem call and retain it through all
        // calls using the path. The file contents use only the stack buffer.
        let path_bytes = name
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(allocation_bookkeeping_bytes))
            .ok_or("ambient path overflow")?;
        let _path_reservation = budget
            .reserve(path_bytes)
            .map_err(|_| "ambient path budget")?;
        hash.update(
            &(u64::try_from(name.len()).map_err(|_| "ambient path overflow")?).to_be_bytes(),
        );
        hash.update(name);
        let before = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                hash.update(&[0]);
                continue;
            }
            Err(_) => return Err("ambient metadata unavailable"),
        };
        if require_absent {
            return Err("ambient ignore input is present");
        }
        if !before.is_file() {
            hash.update(&[0]);
            continue;
        }
        let length = before.len();
        if length > FILE_LIMIT {
            return Err("ambient file too large");
        }
        total = total.checked_add(length).ok_or("ambient byte overflow")?;
        if total > TOTAL_LIMIT {
            return Err("ambient inputs too large");
        }
        let mut file = File::open(path).map_err(|_| "ambient file unavailable")?;
        let opened = file
            .metadata()
            .map_err(|_| "ambient file metadata unavailable")?;
        let same = |a: &std::fs::Metadata, b: &std::fs::Metadata| {
            (
                a.dev(),
                a.ino(),
                a.len(),
                a.mtime(),
                a.mtime_nsec(),
                a.ctime(),
                a.ctime_nsec(),
            ) == (
                b.dev(),
                b.ino(),
                b.len(),
                b.mtime(),
                b.mtime_nsec(),
                b.ctime(),
                b.ctime_nsec(),
            )
        };
        if !opened.is_file() || !same(&before, &opened) {
            return Err("ambient file changed");
        }
        hash.update(&[1]);
        hash.update(&length.to_be_bytes());
        let mut remaining = length;
        while remaining > 0 {
            if cancelled.load(Ordering::Acquire) {
                return Err("ambient capture cancelled");
            }
            let count = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| "ambient length overflow")?;
            let read = file
                .read(&mut buffer[..count])
                .map_err(|_| "ambient read failed")?;
            if read == 0 {
                return Err("ambient file shortened");
            }
            hash.update(&buffer[..read]);
            remaining -= read as u64;
        }
        if file
            .read(&mut buffer[..1])
            .map_err(|_| "ambient read failed")?
            != 0
        {
            return Err("ambient file grew");
        }
        let after = file
            .metadata()
            .map_err(|_| "ambient file metadata unavailable")?;
        let named_after = std::fs::metadata(path).map_err(|_| "ambient file moved")?;
        if !same(&opened, &after) || !same(&opened, &named_after) {
            return Err("ambient file changed");
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("ambient capture cancelled");
    }
    Ok(*hash.finalize().as_bytes())
}

/// Borrowed values from an already admitted, immutable environment snapshot.
/// This type does not prove that the ordinary discovery walk used the snapshot;
/// a caller must establish that binding separately before using the result as
/// retirement evidence. A missing HOME is unsupported because `home_dir` may
/// consult platform account services in that case.
#[cfg(unix)]
pub(super) struct NoConfigEnvironment<'a> {
    pub home: &'a std::ffi::OsStr,
    pub xdg_config_home: Option<&'a std::ffi::OsStr>,
    pub git_config_global: Option<&'a std::ffi::OsStr>,
    pub git_config_system: Option<&'a std::ffi::OsStr>,
}

/// Resolve the pinned ignore crate's default global excludes path when every
/// config it attempts is absent or proven empty through a held descriptor.
/// Nonempty configs and ambiguous I/O errors decline. Returned path ownership is
/// charged to `budget`; snapshot ownership must already be charged by the caller.
/// This is only a path computation, not evidence that discovery used it.
#[cfg(unix)]
pub(super) fn no_config_default_excludes(
    env: &NoConfigEnvironment<'_>,
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
) -> Result<backend_version::retirement_budget::Metered<std::path::PathBuf>, &'static str> {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    fn validate(path: &Path) -> Result<(), &'static str> {
        let bytes = path.as_os_str().as_encoded_bytes();
        if !path.is_absolute() || bytes.len() > PATH_LIMIT || bytes.contains(&0) {
            return Err("unsupported config path");
        }
        Ok(())
    }
    fn absent(path: &Path, budget: &RetirementBudget, overhead: usize) -> Result<(), &'static str> {
        validate(path)?;
        super::bounded_empty_inventory::empty_config_state(path, budget, overhead).map(|_| ())
    }
    fn joined(
        base: &Path,
        suffix: &str,
        budget: &RetirementBudget,
        overhead: usize,
    ) -> Result<backend_version::retirement_budget::Metered<PathBuf>, &'static str> {
        validate(base)?;
        // PathBuf::join first copies the base and then may grow its buffer.
        // Cover both allocations concurrently, including geometric growth.
        let max_len = base
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(suffix.len()))
            .ok_or("config path overflow")?;
        if max_len > PATH_LIMIT {
            return Err("config path too long");
        }
        let charge = max_len
            .checked_mul(3)
            .and_then(|n| n.checked_add(overhead.checked_mul(2)?))
            .ok_or("config path overflow")?;
        let reservation = budget.reserve(charge).map_err(|_| "config path budget")?;
        Ok(reservation.meter(base.join(suffix)))
    }
    fn nonempty(value: Option<&OsStr>) -> Option<&Path> {
        value.filter(|v| !v.is_empty()).map(Path::new)
    }

    let home = Path::new(env.home);
    validate(home)?;
    if let Some(path) = nonempty(env.git_config_global) {
        absent(path, budget, allocation_bookkeeping_bytes)?;
    }
    let home_config = joined(home, ".gitconfig", budget, allocation_bookkeeping_bytes)?;
    absent(home_config.get(), budget, allocation_bookkeeping_bytes)?;
    drop(home_config);
    let xdg_default;
    let xdg = if let Some(path) = nonempty(env.xdg_config_home) {
        validate(path)?;
        path
    } else {
        xdg_default = joined(home, ".config", budget, allocation_bookkeeping_bytes)?;
        xdg_default.get().as_path()
    };
    let xdg_config = joined(xdg, "git/config", budget, allocation_bookkeeping_bytes)?;
    absent(xdg_config.get(), budget, allocation_bookkeeping_bytes)?;
    drop(xdg_config);
    let system = nonempty(env.git_config_system).unwrap_or_else(|| Path::new("/etc/gitconfig"));
    absent(system, budget, allocation_bookkeeping_bytes)?;
    joined(xdg, "git/ignore", budget, allocation_bookkeeping_bytes)
}

#[cfg(target_os = "linux")]
struct StartupSnapshot {
    home: std::ffi::OsString,
    xdg: Option<std::ffi::OsString>,
    global: Option<std::ffi::OsString>,
    system: Option<std::ffi::OsString>,
    configs: Vec<ConfigWitness>,
}

#[cfg(target_os = "linux")]
struct ConfigWitness {
    path: std::path::PathBuf,
    revision: Option<super::bounded_empty_inventory::EmptyConfigState>,
}

#[cfg(target_os = "linux")]
static STARTUP_SNAPSHOT: std::sync::OnceLock<StartupSnapshot> = std::sync::OnceLock::new();
#[cfg(target_os = "linux")]
static STARTUP_BOUND: AtomicBool = AtomicBool::new(false);

/// Capture only Git's named path variables at owner startup, before discovery.
/// The cap applies to retained values: `var_os` itself allocates before the
/// value length can be inspected and therefore transient startup allocation is
/// not bounded by this cap. No retirement code initializes this snapshot.
#[cfg(target_os = "linux")]
pub(super) fn install_startup_snapshot() {
    let mut snapshot = StartupSnapshot {
        home: match std::env::var_os("HOME") {
            Some(value) => value,
            None => return,
        },
        xdg: std::env::var_os("XDG_CONFIG_HOME"),
        global: std::env::var_os("GIT_CONFIG_GLOBAL"),
        system: std::env::var_os("GIT_CONFIG_SYSTEM"),
        configs: Vec::new(),
    };
    if [
        &snapshot.home,
        snapshot.xdg.as_ref().unwrap_or(&snapshot.home),
        snapshot.global.as_ref().unwrap_or(&snapshot.home),
        snapshot.system.as_ref().unwrap_or(&snapshot.home),
    ]
    .iter()
    .any(|value| value.as_encoded_bytes().len() > PATH_LIMIT || value.capacity() > PATH_LIMIT)
    {
        return;
    }
    let env = snapshot.borrowed();
    let Ok(budget) = RetirementBudget::new(usize::MAX) else {
        return;
    };
    let Ok(path) = no_config_default_excludes(&env, &budget, 64) else {
        return;
    };
    match std::fs::metadata(path.get()) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) => {}
        _ => return,
    }
    // This clone belongs to the admitted owner snapshot, not a retirement.
    let policy_path = path.get().clone();
    if policy_path.capacity() > PATH_LIMIT {
        return;
    }
    let configs = snapshot.config_paths();
    if configs.capacity() > 4 || configs.iter().any(|path| path.capacity() > PATH_LIMIT * 2) {
        return;
    }
    let mut witnessed = Vec::with_capacity(configs.len());
    if witnessed.capacity() > 4 {
        return;
    }
    for config in configs {
        let Ok(revision) = super::bounded_empty_inventory::empty_config_state(&config, &budget, 64)
        else {
            return;
        };
        witnessed.push(ConfigWitness {
            path: config,
            revision,
        });
    }
    snapshot.configs = witnessed;
    if STARTUP_SNAPSHOT.set(snapshot).is_ok() {
        if backend_library::install_startup_absent_global_excludes(
            policy_path,
            config_snapshot_current,
        )
        .is_ok()
        {
            STARTUP_BOUND.store(true, Ordering::Release);
        }
    }
}

#[cfg(target_os = "linux")]
fn validate_config_snapshot(
    budget: &RetirementBudget,
    bookkeeping: usize,
) -> Result<(), &'static str> {
    let snapshot = STARTUP_SNAPSHOT
        .get()
        .ok_or("startup config snapshot unavailable")?;
    for witness in &snapshot.configs {
        let current =
            super::bounded_empty_inventory::empty_config_state(&witness.path, budget, bookkeeping)?;
        if current != witness.revision {
            return Err("startup Git config changed");
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn config_snapshot_current() -> bool {
    let Ok(budget) = RetirementBudget::new(usize::MAX) else {
        return false;
    };
    validate_config_snapshot(&budget, 64).is_ok()
}

#[cfg(target_os = "linux")]
impl StartupSnapshot {
    fn config_paths(&self) -> Vec<std::path::PathBuf> {
        let home = Path::new(&self.home);
        let xdg = self
            .xdg
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let system = self
            .system
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/etc/gitconfig".into());
        let mut paths = Vec::with_capacity(4);
        if let Some(global) = self.global.as_deref().filter(|value| !value.is_empty()) {
            paths.push(global.into());
        }
        paths.push(home.join(".gitconfig"));
        paths.push(xdg.join("git/config"));
        paths.push(system);
        paths
    }

    fn borrowed(&self) -> NoConfigEnvironment<'_> {
        NoConfigEnvironment {
            home: &self.home,
            xdg_config_home: self.xdg.as_deref(),
            git_config_global: self.global.as_deref(),
            git_config_system: self.system.as_deref(),
        }
    }
}

/// Borrow the admitted startup values; every path copy created during a
/// retirement must be separately reserved against its admission budget.
#[cfg(target_os = "linux")]
pub(super) fn startup_snapshot() -> Option<NoConfigEnvironment<'static>> {
    if !STARTUP_BOUND.load(Ordering::Acquire) {
        return None;
    }
    STARTUP_SNAPSHOT.get().map(StartupSnapshot::borrowed)
}

/// Construct precisely the path set used by `ambient_ignore_inputs`, using the
/// admitted startup snapshot rather than consulting the mutable environment.
/// The caller must establish the policy/snapshot binding and filesystem-view
/// coherence and compare this digest to the original discovery witness.
#[cfg(target_os = "linux")]
pub(super) fn hash_startup_policy_paths(
    root: &Path,
    directories: &[std::path::PathBuf],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<[u8; 32], &'static str> {
    hash_startup_policy_paths_impl(
        root,
        directories,
        budget,
        allocation_bookkeeping_bytes,
        cancelled,
        false,
    )
}

/// Strict subset for a fresh empty scan: all ambient ignore rules must be
/// absent, so the normal walker cannot filter the proved tiny tree.
#[cfg(target_os = "linux")]
pub(super) fn hash_absent_startup_policy_paths(
    root: &Path,
    directories: &[std::path::PathBuf],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<[u8; 32], &'static str> {
    hash_startup_policy_paths_impl(
        root,
        directories,
        budget,
        allocation_bookkeeping_bytes,
        cancelled,
        true,
    )
}

#[cfg(target_os = "linux")]
fn hash_startup_policy_paths_impl(
    root: &Path,
    directories: &[std::path::PathBuf],
    budget: &RetirementBudget,
    allocation_bookkeeping_bytes: usize,
    cancelled: &AtomicBool,
    require_absent: bool,
) -> Result<[u8; 32], &'static str> {
    use backend_version::retirement_budget::Metered;
    use std::path::PathBuf;
    let snapshot = startup_snapshot().ok_or("startup snapshot unavailable")?;
    validate_config_snapshot(budget, allocation_bookkeeping_bytes)?;
    let global = no_config_default_excludes(&snapshot, budget, allocation_bookkeeping_bytes)?;
    let count = root
        .ancestors()
        .count()
        .checked_mul(4)
        .and_then(|n| n.checked_add(directories.len()))
        .and_then(|n| n.checked_add(1))
        .ok_or("ambient path count overflow")?;
    let bytes = count
        .checked_mul(std::mem::size_of::<Metered<PathBuf>>())
        .and_then(|n| n.checked_add(allocation_bookkeeping_bytes))
        .ok_or("ambient path container overflow")?;
    let _paths_charge = budget
        .reserve(bytes)
        .map_err(|_| "ambient path container budget")?;
    let mut paths = Vec::with_capacity(count);
    fn push_joined(
        paths: &mut Vec<Metered<PathBuf>>,
        base: &Path,
        suffix: &Path,
        budget: &RetirementBudget,
        overhead: usize,
    ) -> Result<(), &'static str> {
        let max_len = base
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_add(suffix.as_os_str().as_encoded_bytes().len()))
            .ok_or("ambient path overflow")?;
        if max_len > PATH_LIMIT {
            return Err("ambient path too long");
        }
        let charge = max_len
            .checked_mul(3)
            .and_then(|n| n.checked_add(overhead.checked_mul(2)?))
            .ok_or("ambient path overflow")?;
        let reservation = budget.reserve(charge).map_err(|_| "ambient path budget")?;
        paths.push(reservation.meter(base.join(suffix)));
        Ok(())
    }
    for ancestor in root.ancestors() {
        for name in [".ignore", ".gitignore", ".nudoxignore", ".git/info/exclude"] {
            push_joined(
                &mut paths,
                ancestor,
                Path::new(name),
                budget,
                allocation_bookkeeping_bytes,
            )?;
        }
    }
    for directory in directories {
        // Join the two components under separately held reservations.
        let mut intermediate = Vec::new();
        let header = std::mem::size_of::<Metered<PathBuf>>()
            .checked_add(allocation_bookkeeping_bytes)
            .ok_or("ambient intermediate overflow")?;
        let _intermediate_charge = budget
            .reserve(header)
            .map_err(|_| "ambient intermediate budget")?;
        intermediate.reserve_exact(1);
        push_joined(
            &mut intermediate,
            root,
            directory,
            budget,
            allocation_bookkeeping_bytes,
        )?;
        push_joined(
            &mut paths,
            intermediate[0].get(),
            Path::new(".git/info/exclude"),
            budget,
            allocation_bookkeeping_bytes,
        )?;
    }
    paths.push(global);
    paths.sort_unstable_by(|a, b| a.get().cmp(b.get()));
    paths.dedup_by(|a, b| a.get() == b.get());
    let refs_bytes = paths
        .len()
        .checked_mul(std::mem::size_of::<&Path>())
        .and_then(|n| n.checked_add(allocation_bookkeeping_bytes))
        .ok_or("ambient path references overflow")?;
    let _refs_charge = budget
        .reserve(refs_bytes)
        .map_err(|_| "ambient path references budget")?;
    let refs: Vec<&Path> = paths.iter().map(|p| p.get().as_path()).collect();
    hash_selected_paths_impl(
        &refs,
        budget,
        allocation_bookkeeping_bytes,
        cancelled,
        require_absent,
    )
}
