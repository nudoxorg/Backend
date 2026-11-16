//! Optional delegated cgroup-v2 containment for processes that create new
//! process groups. No cgroup is created without an explicitly delegated root.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(0);
const ROOT_ENV: &str = "KEEL_CGROUP_ROOT";
const REQUIRE_ENV: &str = "KEEL_REQUIRE_CGROUP";
const HELPER_ROOT_ENV: &str = "KEEL_EXEC_CGROUP_ROOT";
const HELPER: &str = "__exec-cgroup";

/// One owned process-tree scope. The fallback is explicitly process-group
/// containment and cannot contain a descendant which creates a new session.
pub(crate) struct Scope {
    path: Option<PathBuf>,
    root: Option<PathBuf>,
}

impl Scope {
    /// Create a child scope under an explicitly delegated cgroup-v2 directory.
    /// A supplied but unusable root is an error, never a fallback.
    pub(crate) fn new() -> Result<Self, String> {
        let root = std::env::var_os(ROOT_ENV);
        let required = std::env::var_os(REQUIRE_ENV).as_deref() == Some(std::ffi::OsStr::new("1"));
        let Some(root) = root else {
            return if required {
                Err(format!("{REQUIRE_ENV}=1 requires a delegated {ROOT_ENV}"))
            } else {
                Ok(Self {
                    path: None,
                    root: None,
                })
            };
        };
        #[cfg(not(target_os = "linux"))]
        {
            let _ = root;
            Err(format!("{ROOT_ENV} requires Linux cgroup v2"))
        }
        #[cfg(target_os = "linux")]
        {
            let root = validate_root(Path::new(&root))?;
            let serial = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| format!("clock before Unix epoch: {error}"))?
                .as_nanos();
            let path = root.join(format!("keel-{}-{time}-{serial}", std::process::id()));
            std::fs::create_dir(&path)
                .map_err(|error| format!("create delegated cgroup {}: {error}", path.display()))?;
            if !path.join("cgroup.procs").is_file()
                || !path.join("cgroup.events").is_file()
                || !path.join("cgroup.kill").is_file()
            {
                let _ = std::fs::remove_dir(&path);
                return Err(format!(
                    "{} lacks required cgroup-v2 controls",
                    path.display()
                ));
            }
            Ok(Self {
                path: Some(path),
                root: Some(root),
            })
        }
    }

    /// Exact containment mode for the run report.
    pub(crate) fn mode(&self) -> &'static str {
        if self.path.is_some() {
            "cgroup-v2"
        } else {
            "process-group"
        }
    }

    /// The owned cgroup, for evidence. The caller must not place unrelated
    /// processes in this directory.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Wrap a prepared command so the helper enters the cgroup before exec.
    /// The original command's explicit environment and cwd are preserved.
    pub(crate) fn command(&self, original: &Command) -> Result<Command, String> {
        let Some(path) = self.path.as_ref() else {
            return Ok(copy_command(original));
        };
        let executable =
            std::env::current_exe().map_err(|error| format!("locate cgroup helper: {error}"))?;
        let mut wrapped = Command::new(executable);
        wrapped
            .arg(HELPER)
            .arg(path)
            .arg("--")
            .arg(original.get_program());
        wrapped.args(original.get_args());
        if let Some(cwd) = original.get_current_dir() {
            wrapped.current_dir(cwd);
        }
        wrapped.env_clear();
        for (key, value) in original.get_envs() {
            if let Some(value) = value {
                wrapped.env(key, value);
            }
        }
        let root = self.root.as_ref().ok_or("cgroup root missing")?;
        wrapped.env(HELPER_ROOT_ENV, root);
        Ok(wrapped)
    }

    /// Kill every remaining member, wait for the kernel to report an empty
    /// subtree, then remove only the child created by this scope. Idempotent.
    pub(crate) fn stop(&mut self) -> Result<(), String> {
        let Some(path) = self.path.as_ref() else {
            return Ok(());
        };
        #[cfg(target_os = "linux")]
        {
            std::fs::write(path.join("cgroup.kill"), b"1")
                .map_err(|error| format!("kill cgroup {}: {error}", path.display()))?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let events = std::fs::read_to_string(path.join("cgroup.events"))
                    .map_err(|error| format!("read cgroup events {}: {error}", path.display()))?;
                if events
                    .lines()
                    .any(|line| line.split_whitespace().collect::<Vec<_>>() == ["populated", "0"])
                {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "cgroup {} remained populated after kill",
                        path.display()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::fs::remove_dir(path)
                .map_err(|error| format!("remove cgroup {}: {error}", path.display()))?;
            self.path = None;
            self.root = None;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Err("cgroup scope is unavailable on this platform".into())
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn copy_command(original: &Command) -> Command {
    let mut command = Command::new(original.get_program());
    command.args(original.get_args());
    if let Some(cwd) = original.get_current_dir() {
        command.current_dir(cwd);
    }
    command.env_clear();
    for (key, value) in original.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    command
}

#[cfg(target_os = "linux")]
fn unescape_mount(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
fn validate_root(root: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err(format!("{ROOT_ENV} must be absolute"));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|error| format!("invalid cgroup root {}: {error}", root.display()))?;
    if !root.is_dir()
        || !root.join("cgroup.controllers").is_file()
        || !root.join("cgroup.procs").is_file()
    {
        return Err(format!("{} is not a cgroup-v2 directory", root.display()));
    }
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("read cgroup mount information: {error}"))?;
    let mounted = mounts.lines().any(|line| {
        let Some((before, after)) = line.split_once(" - ") else {
            return false;
        };
        if after.split_whitespace().next() != Some("cgroup2") {
            return false;
        }
        let Some(mount) = before.split_whitespace().nth(4) else {
            return false;
        };
        std::fs::canonicalize(unescape_mount(mount))
            .is_ok_and(|mount| root != mount && root.starts_with(mount))
    });
    if !mounted {
        return Err(format!(
            "{} is not on a visible cgroup-v2 mount",
            root.display()
        ));
    }
    Ok(root)
}

/// Handle the private trampoline before normal argument parsing. The helper
/// validates that its target is a direct child of the delegated root, joins
/// it, and replaces itself with the actual product executable.
pub(crate) fn exec_helper() -> Option<ExitCode> {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER)) {
        return None;
    }
    let result = exec_helper_inner(args.collect());
    if let Err(error) = result {
        eprintln!("keel cgroup helper: {error}");
    }
    Some(ExitCode::from(125))
}

#[cfg(target_os = "linux")]
fn exec_helper_inner(args: Vec<OsString>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    if args.len() < 3 || args[1] != "--" {
        return Err("invalid helper arguments".into());
    }
    let root = std::env::var_os(HELPER_ROOT_ENV).ok_or("missing delegated root")?;
    let root = validate_root(Path::new(&root))?;
    let path = std::fs::canonicalize(Path::new(&args[0]))
        .map_err(|error| format!("invalid child cgroup: {error}"))?;
    if path.parent() != Some(root.as_path())
        || !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with("keel-"))
    {
        return Err("child cgroup is outside the delegated root".into());
    }
    std::fs::write(path.join("cgroup.procs"), std::process::id().to_string())
        .map_err(|error| format!("join cgroup {}: {error}", path.display()))?;
    let mut command = Command::new(&args[2]);
    command.args(&args[3..]);
    command.env_remove(HELPER_ROOT_ENV);
    Err(format!("exec {:?}: {}", args[2], command.exec()))
}

#[cfg(not(target_os = "linux"))]
fn exec_helper_inner(_args: Vec<OsString>) -> Result<(), String> {
    Err("cgroup helper requires Linux".into())
}
