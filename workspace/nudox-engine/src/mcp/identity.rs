//! The on-disk record that makes the MCP endpoint's port and token survive a
//! restart — see `tests/mcp/endpoint_stability.rs` for the defect this closes
//! and the two rationales (ephemeral port, non-persisted token) it has to
//! preserve.
//!
//! # Why this is a separate file from the authorisation cache
//!
//! `crate::mcp::account::cache::CachedAuthorization` already writes JSON
//! atomically into the same state directory this module uses
//! (`crate::mcp::account::state_dir`), and the two files share a directory on
//! purpose — see [`McpHost::start`](super::host::McpHost::start), which loads
//! this one. But they cache different *kinds* of fact: `CachedAuthorization`
//! remembers a verdict the service handed down about a credential the user
//! owns, while [`EndpointIdentity`] remembers a secret this process minted for
//! itself. Folding the two into one file would make an unrelated change to
//! the account cache's shape a reason to touch the MCP transport, and vice
//! versa — two concerns that happen to share a directory should not be forced
//! to share a schema too.
//!
//! # Every failure here is "no state", never "broken app"
//!
//! [`EndpointIdentity::load`] collapses "the file does not exist" and "the
//! file exists but is not valid JSON" to the same `None`, exactly the way
//! `CachedAuthorization::load` does and for the same reason: the caller's
//! recovery is identical either way (mint a fresh port/token pair) and cheap,
//! so there is nothing a distinguishing error type would let a caller do
//! differently. The alternative — failing to start the MCP endpoint because a
//! state file was truncated by a crash — would be strictly worse than the
//! problem persistence was meant to solve.

use std::io::Write as _;
use std::path::Path;

/// The file this module reads and writes, inside the directory
/// [`crate::mcp::account::state_dir`] resolves.
pub const ENDPOINT_FILE: &str = "endpoint.json";

/// The port and token a previous launch used, worth trying again.
///
/// Both fields round-trip through existing types rather than being invented
/// here: `port` is exactly what [`PortPreference::Remembered`](super::endpoint::PortPreference::Remembered)
/// takes, and `token` is exactly what [`SessionToken::from_secret`](super::session::SessionToken::from_secret)
/// takes — this struct is only ever the seam between "bytes on disk" and
/// those two, never a parallel representation of either.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointIdentity {
    /// The port the listener bound last time.
    pub port: u16,
    /// The session token clients were given last time, in the same form
    /// [`SessionToken::expose`](super::session::SessionToken::expose) returns.
    pub token: String,
}

impl EndpointIdentity {
    /// Persist `self` into `<dir>/endpoint.json`, owner-readable only.
    ///
    /// Atomic against a torn write the same way
    /// `crate::mcp::account::cache::write_atomically` is — a temp file in the
    /// same directory, then `rename`, so a crash mid-write leaves either the
    /// old file or the new one and never a half-written one. Not built on
    /// that helper directly: this file's mode has to be `0600` from the
    /// moment it has content, and `write_atomically` has no way to say that.
    /// The permission is narrowed on the freshly created temp file, before
    /// any byte of the token is written to it, rather than widened-then-
    /// narrowed after — a `write` followed by a separate `chmod` would leave
    /// a bearer credential for the whole corpus sitting at the umask's
    /// default permissions for however long the gap between those two calls
    /// is.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let path = dir.join(ENDPOINT_FILE);
        let tmp = path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;

        {
            let mut file = std::fs::File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&bytes)?;
        }

        std::fs::rename(&tmp, &path)
    }

    /// Read `<dir>/endpoint.json`, if it holds anything usable.
    ///
    /// `None` for both "never written" and "unreadable" — see the module
    /// doc's "every failure here is 'no state'" section.
    pub fn load(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(dir.join(ENDPOINT_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exhaustive contract — round-trip, absence, corruption, and the
    /// unix mode — lives in `tests/mcp/endpoint_stability.rs`, which is the
    /// spec this module was written against. This pins the same round trip
    /// locally so a regression here fails in the file that broke it, not only
    /// in the integration suite two directories away.
    #[test]
    fn a_saved_identity_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let identity = EndpointIdentity {
            port: 4242,
            token: "deadbeef".to_owned(),
        };
        identity.save(dir.path()).expect("save must succeed");
        let loaded = EndpointIdentity::load(dir.path()).expect("must load what was just saved");
        assert_eq!(loaded, identity);
    }

    #[test]
    fn load_on_an_empty_directory_is_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(EndpointIdentity::load(dir.path()).is_none());
    }
}
