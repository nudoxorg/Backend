//! Filesystem identity and the short local endpoint derived from it.
//!
//! Workspace ownership is a kernel lock on an inode, while the endpoint is a
//! path. Every spelling of one directory is reduced to the same identity
//! before it is hashed, so one checkout always derives one socket.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use super::{RUNTIME_DIR_ENV, RuntimeError};

/// Domain separator so an endpoint digest can never be confused with another
/// blake3 use of the same path bytes.
const ENDPOINT_DOMAIN: &[u8] = b"backend-v2-local-endpoint\0";
/// Conservative budget for a derived endpoint. The platform `sun_path` limit
/// is 104 bytes on macOS and 108 on Linux; staying under this keeps the
/// preferred runtime directory from producing an unbindable socket.
const MAX_DERIVED_ENDPOINT_BYTES: usize = 100;
/// Total capacity of `sockaddr_un.sun_path`, in bytes, on the strictest
/// platform this crate targets.
///
/// macOS declares `sun_path` as `char[104]` in `<sys/un.h>`; Linux declares it
/// as `char[108]` in `unix(7)`. A workspace's daemon and every client that
/// dials it must agree on one endpoint regardless of which of the two built
/// it, so the smaller of the two capacities is the one this crate enforces
/// everywhere `sockaddr_un` is unix-specific, not per compiled target.
#[cfg(target_os = "linux")]
const SUN_PATH_CAPACITY: usize = 108;
#[cfg(all(unix, not(target_os = "linux")))]
const SUN_PATH_CAPACITY: usize = 104;
/// Bytes available to the endpoint path itself once the NUL terminator that
/// `bind(2)`/`connect(2)` require inside `sun_path` is reserved.
#[cfg(unix)]
pub(super) const MAX_ENDPOINT_PATH_BYTES: usize = SUN_PATH_CAPACITY - 1;
#[cfg(windows)]
pub(super) const MAX_ENDPOINT_PATH_BYTES: usize = 100;

/// Reduces one path to the filesystem identity the workspace lock uses.
///
/// `canonicalize` is authoritative whenever the directory exists: it resolves
/// symlinks, `.` and `..` segments, trailing separators, and the macOS
/// `/tmp` to `/private/tmp` indirection in one step.
///
/// A workspace that has not been created yet has no inode to resolve. Rather
/// than create it — discovery must stay free of side effects, because every
/// surface calls it before it knows whether it will own anything — the longest
/// existing ancestor is canonicalized and the remaining components are
/// appended after lexical reduction. The value therefore already agrees with
/// the canonical form for the part of the path that exists, and converges on
/// it completely as soon as the workspace directory is created.
pub(super) fn normalize_identity(path: &Path) -> PathBuf {
    let absolute = lexically_absolute(path);
    let mut existing = absolute.as_path();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            let mut identity = normalize_verbatim_prefix(&resolved);
            for component in suffix.iter().rev() {
                identity.push(component);
            }
            return identity;
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return absolute;
        };
        suffix.push(name.to_os_string());
        existing = parent;
    }
}

/// Absolutizes and lexically reduces a path without touching the filesystem.
fn lexically_absolute(path: &Path) -> PathBuf {
    let path = normalize_verbatim_prefix(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Removes Windows' verbatim path marker at the process boundary.
///
/// `canonicalize` deliberately returns `\\?\\` paths on Windows. Those paths
/// are valid for Win32 calls but leak an implementation spelling into package
/// coordinates, MCP instructions, and continuation context. The marker does
/// not identify a different file, so stripping it before lexical identity
/// reduction keeps endpoint ownership stable while giving every surface one
/// display spelling. UNC verbatim paths become ordinary UNC paths.
pub(super) fn normalize_verbatim_prefix(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_os_str().to_string_lossy();
        // Compare bytes so an unrelated non-ASCII path prefix cannot panic
        // on a slice that splits a UTF-8 code point.
        if text
            .as_bytes()
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(br"\\?\unc\"))
        {
            return PathBuf::from(format!(r"\\{}", &text[8..]));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            // Only a verbatim drive path has a normal Win32 spelling. Keep
            // device namespaces such as `GLOBALROOT` and `Volume{...}`
            // verbatim: dropping their prefix would turn an absolute device
            // path into a relative path and could change which object is
            // addressed.
            let drive = local.as_bytes().get(1).copied() == Some(b':');
            if drive {
                return PathBuf::from(local);
            }
        }
    }
    path.to_path_buf()
}

/// Returns the canonical, user-facing spelling of a project or workspace
/// path. This resolves the existing prefix and strips Windows' `\\?\\` marker
/// without changing the filesystem identity used for ownership.
#[must_use]
pub fn normalize_surface_path(path: impl AsRef<Path>) -> PathBuf {
    normalize_identity(path.as_ref())
}

/// Returns the checkout that owns the conventional `.backend/v2` state path.
///
/// A caller can intentionally choose a state directory elsewhere, so this
/// check is limited to the layout this crate itself derives. That catches a
/// stale MCP/CLI configuration pointing at another checkout while preserving
/// explicit shared state locations for hosts that own their own identity
/// policy.
pub(super) fn checkout_for_workspace(workspace: &Path) -> Option<PathBuf> {
    let version = workspace.file_name()?.to_string_lossy();
    let backend = workspace.parent()?.file_name()?.to_string_lossy();
    if !version.eq_ignore_ascii_case("v2") || !backend.eq_ignore_ascii_case(".backend") {
        return None;
    }
    workspace.parent()?.parent().map(normalize_identity)
}

/// Compares two normalized paths using the host filesystem's identity rules.
/// Windows paths are case-insensitive even when callers spell a drive, UNC
/// share, or directory component differently; Unix paths remain byte-exact.
#[cfg(windows)]
pub(super) fn same_path_identity(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
pub(super) fn same_path_identity(left: &Path, right: &Path) -> bool {
    left == right
}

/// Returns the effective user identity folded into every derived endpoint.
///
/// `/tmp` is shared, so two users indexing the same absolute workspace path
/// would otherwise contend for one 0600 socket that only one of them can open.
#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    #[cfg(windows)]
    {
        // Windows has no numeric effective UID. Fold the authenticated user
        // SID into the endpoint identity so two users sharing a workspace
        // root still derive different owner-protected endpoints.
        if let Ok(user) = backend_platform::win32::identity::current_user() {
            let digest = blake3::hash(user.as_bytes());
            let bytes = digest.as_bytes();
            return u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
    }
    0
}

/// Selects the directory that holds derived endpoints.
///
/// `XDG_RUNTIME_DIR` is preferred because it is per-user, mode `0700`, and
/// cleaned by the session rather than by a `/tmp` sweeper. It is used only
/// when it is absolute and leaves the endpoint inside the platform `sun_path`
/// budget; otherwise `/tmp` remains the answer.
///
/// `TMPDIR` is deliberately *not* consulted. On macOS it names a per-session
/// directory, so two surfaces of the same user can see different values — the
/// exact divergence this derivation exists to remove.
#[cfg(unix)]
fn socket_directory(file_name: &str) -> PathBuf {
    let fallback = PathBuf::from("/tmp");
    let Some(preferred) = std::env::var_os(RUNTIME_DIR_ENV).map(PathBuf::from) else {
        return fallback;
    };
    if preferred.is_absolute()
        && preferred.join(file_name).as_os_str().len() <= MAX_DERIVED_ENDPOINT_BYTES
    {
        preferred
    } else {
        fallback
    }
}

#[cfg(not(unix))]
fn socket_directory(_file_name: &str) -> PathBuf {
    std::env::temp_dir()
}

/// Derives the short Unix endpoint for a workspace's data directory using
/// this crate's hashed, short-prefix scheme — the same one
/// [`WorkspacePaths::discover`] falls back to when no endpoint is supplied
/// *and* `BACKEND_LOCALD_ENDPOINT` is unset.
///
/// Unlike `discover`, this never consults the environment. A caller that
/// needs a workspace-specific endpoint regardless of an ambient
/// `BACKEND_LOCALD_ENDPOINT` left over from another session on a shared
/// machine — a test deriving a fresh, unique, always-short socket per
/// fixture is the motivating case — should call this directly instead of
/// duplicating the hashing scheme or risking a collision with whatever that
/// variable happens to name.
#[must_use]
pub fn derive_endpoint(data: &Path) -> PathBuf {
    default_endpoint(data)
}

/// Derives the endpoint for a workspace from its filesystem identity.
///
/// The digest covers a domain separator, the effective user, and the
/// normalized workspace path, so the same directory always yields the same
/// socket however it was spelled, and two users never collide.
pub(super) fn default_endpoint(data: &Path) -> PathBuf {
    let identity = normalize_identity(data);
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENDPOINT_DOMAIN);
    hasher.update(&effective_uid().to_be_bytes());
    hasher.update(&[0]);
    hasher.update(identity.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut short = String::with_capacity(24);
    for byte in &digest.as_bytes()[..12] {
        use std::fmt::Write as _;
        let _ = write!(short, "{byte:02x}");
    }
    let file_name = format!("backend-v2-{short}.sock");
    socket_directory(&file_name).join(file_name)
}

/// Rejects an endpoint that could never be bound or connected to.
///
/// `UnixListener`/`UnixStream` copy the path into a fixed-size `sun_path`
/// buffer at the syscall boundary, so an over-long path fails there with a
/// raw `ENAMETOOLONG` `io::Error` that gives a caller no way to tell "this
/// workspace's identity is unreachable" from an ordinary transient I/O fault.
/// Catching it here, against an honestly derived platform limit, turns that
/// into a typed, actionable error before a socket is ever touched.
#[cfg(any(unix, windows))]
pub(super) fn validate_endpoint_length(endpoint: &Path) -> Result<(), RuntimeError> {
    let bytes = endpoint.as_os_str().len();
    if bytes > MAX_ENDPOINT_PATH_BYTES {
        return Err(RuntimeError::EndpointTooLong {
            endpoint: endpoint.to_path_buf(),
            bytes,
            limit: MAX_ENDPOINT_PATH_BYTES,
        });
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(super) const fn validate_endpoint_length(_endpoint: &Path) -> Result<(), RuntimeError> {
    Ok(())
}
