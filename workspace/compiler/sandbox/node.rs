//! Stable per-node identity (DAEMON-PLAN §2.5).
//!
//! Replaces `process::id()` in cgroup / scratch / temp names with a stable,
//! injected identity: `nudox-{node}-{jobkey8}-{seq}`. Owned by the
//! `ForgeRuntime` and passed down, never read from process globals.

use std::sync::atomic::{AtomicU64, Ordering};

/// A stable identity for a forge node (config or hostname+uuid).
///
/// Two `ForgeRuntime`s in one process carry distinct `NodeId`s so their scratch
/// / cgroup names never collide — the multi-tenant smoke test relies on this.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId(String);

impl NodeId {
    /// Construct from an explicit name.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Derive from the host name plus a monotonic per-process counter so two
    /// runtimes assembled in one process are distinct even without config.
    pub fn from_host() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let host = hostname().unwrap_or_else(|| "node".to_string());
        Self(format!("{host}-{seq}"))
    }

    /// Borrow the identity string.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// A collision-resistant scratch / cgroup name prefix for one job.
    pub fn scratch_prefix(&self, job8: &str) -> String {
        format!("nudox-{}-{}", self.0, job8)
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// Best-effort host name (no ambient policy, just a label). `None` when unknown.
fn hostname() -> Option<String> {
    // Avoid a `std::env::var` policy read; use the uname/hostname syscall via a
    // tiny read of `/proc` or the libc call. Fall back to None.
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most buf.len() bytes and NUL-terminates.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end])
                && !s.is_empty()
            {
                return Some(s.to_string());
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_from_host_ids() {
        let a = NodeId::from_host();
        let b = NodeId::from_host();
        assert_ne!(
            a, b,
            "two runtimes in one process must get distinct node ids"
        );
    }

    #[test]
    fn scratch_prefix_includes_node_and_job() {
        let n = NodeId::new("nodeX");
        let p = n.scratch_prefix("deadbeef");
        assert!(p.contains("nodeX"));
        assert!(p.contains("deadbeef"));
    }
}
