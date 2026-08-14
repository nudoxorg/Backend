//! Per-platform directory resolution.
//!
//! One home for the `HOME`/`XDG_*`/`LOCALAPPDATA`/`APPDATA` dance, so the
//! account cache ([`crate::mcp::account::cache::state_dir`]) and the package
//! cache ([`crate::packages::acquire::default_cache_dir`]) agree on where things go
//! instead of each re-deriving it — a duplication that silently broke once
//! Windows (which has neither `HOME` nor `XDG_STATE_HOME`) was in scope.
//!
//! Both functions return `None` when no usable per-user data root exists;
//! each caller decides what `None` means — "run without a cache" for the
//! account state, "fall back to the temp directory" for the package cache.

use std::path::PathBuf;

/// A non-empty environment variable value, if one is set.
fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The platform's per-user application *state* directory for `app`.
///
/// "State" is data that must persist across runs but is neither a user
/// document nor a cache we can lose freely — the account authorisation cache
/// and the usage ledger. macOS uses `~/Library/Application Support`, Windows
/// `%LOCALAPPDATA%`, and Linux the XDG state directory (`$XDG_STATE_HOME`,
/// defaulting to `~/.local/state`).
pub(crate) fn app_state_dir(app: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = env_dir("HOME")?;
        Some(home.join("Library").join("Application Support").join(app))
    }
    #[cfg(target_os = "windows")]
    {
        // `%LOCALAPPDATA%` is the per-user, non-roaming data root. `%APPDATA%`
        // (roaming) is the fallback for environments where the former is unset;
        // Windows has no `HOME`/`XDG_STATE_HOME`.
        let root = env_dir("LOCALAPPDATA").or_else(|| env_dir("APPDATA"))?;
        Some(root.join(app))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = env_dir("XDG_STATE_HOME") {
            return Some(xdg.join(app));
        }
        let home = env_dir("HOME")?;
        Some(home.join(".local").join("state").join(app))
    }
}

/// The platform's per-user application *cache* directory for `app`.
///
/// Cache is freely deletable — fetched package trees. Windows uses
/// `%LOCALAPPDATA%` (`%APPDATA%` fallback); everything else honours
/// `$XDG_CACHE_HOME`, defaulting to `~/.cache`.
pub(crate) fn app_cache_dir(app: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let root = env_dir("LOCALAPPDATA").or_else(|| env_dir("APPDATA"))?;
        Some(root.join(app))
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(xdg) = env_dir("XDG_CACHE_HOME") {
            return Some(xdg.join(app));
        }
        let home = env_dir("HOME")?;
        Some(home.join(".cache").join(app))
    }
}
