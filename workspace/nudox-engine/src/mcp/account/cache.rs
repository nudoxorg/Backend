//! Where the *non-secret* half of account state lives on disk, and the atomic
//! write both it and [`super::ledger`] are built on.
//!
//! # What is on disk and what is not
//!
//! ```text
//! ~/Library/Application Support/nudox/
//!   authorization.json   ← who we are, when the service last said yes, last quota
//!   usage-ledger.json    ← unflushed tool calls, and any in-flight batch
//! ```
//!
//! **Neither file contains a credential.** They name one, by
//! [`super::credential::KeyFingerprint`] — a truncated SHA-256, which is not
//! reversible — and that is what lets them be plain JSON in a directory the
//! user can read. The key itself is in the Keychain and nowhere else
//! ([`super::store`]).
//!
//! # Why the fingerprint is in the file
//!
//! A cached "yes" is a statement about *a particular key*. If the file did not
//! name one, replacing the key in the Keychain would inherit the previous key's
//! verification and its grace window: paste a revoked key over a good one and
//! keep working for seven days. Binding the cache to a fingerprint makes that a
//! mismatch we detect on load, and a mismatch discards the cache.
//!
//! # What this does not defend against
//!
//! Editing `verified_at` forward. Nothing here is tamper-proof and it is not
//! designed to be — see [`super::state`]'s closing note. The properties this
//! file *does* give are that a cache cannot be transplanted between keys and
//! cannot be silently half-written, and both of those are about accidents.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::credential::KeyFingerprint;
use super::state::{Account, GateState, QuotaKnowledge, UserId};
use super::store::KeySource;

/// Environment override for the state directory.
///
/// The suite sets this to a `TempDir` for every test that touches disk. Without
/// it, tests would either share one directory (and interfere) or write into the
/// developer's real application-support folder (and persist between runs, which
/// is how a test starts passing because of what the *previous* run left behind).
pub const STATE_DIR_ENV: &str = "NUDOX_STATE_DIR";

/// The file holding the cached authorisation verdict.
pub const AUTHORIZATION_FILE: &str = "authorization.json";

/// The on-disk format version.
///
/// Bumped when the shape changes. An unrecognised version is discarded rather
/// than guessed at: the cost of discarding is one `authorize` round trip, and
/// the cost of guessing is a user in a state nothing in this build understands.
const FORMAT_VERSION: u32 = 1;

/// Resolve the directory both state files live in, creating it if needed.
///
/// Order: [`STATE_DIR_ENV`], then the platform application-support directory,
/// then `None`. There is no fallback to the current working directory — a
/// process that cannot find a home directory should run without a cache, not
/// scatter JSON wherever it happens to have been started.
pub fn state_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os(STATE_DIR_ENV) {
        Some(explicit) if !explicit.is_empty() => PathBuf::from(explicit),
        // The platform state directory, shared with `acquire` via
        // `crate::platform` so the HOME/XDG/LOCALAPPDATA rules live in one
        // place. `None` here means "no usable home/data root", which is why
        // there is no fallback to the current working directory.
        _ => crate::platform::app_state_dir("nudox")?,
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How hard a write should try to survive.
///
/// The distinction exists because [`super::ledger`] writes on *every tool
/// call*, and an `fsync` per call is real latency on a path that is supposed to
/// have none, while an `fsync` per *flush* is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Atomic against a torn write and against a **process** crash, because
    /// `rename` is atomic and the page cache outlives the process. Not proof
    /// against a machine losing power.
    ///
    /// This is what the hot path uses. A GUI application that crashes is the
    /// realistic loss event; a kernel panic between two tool calls is not, and
    /// paying an `fsync` per call to cover it would be paying on the wrong axis.
    Fast,
    /// Additionally `fsync`ed, so the bytes are on the device before anything
    /// points at them. Used where a loss would cost money in either direction:
    /// sealing a batch, and shutting down.
    Durable,
}

/// Write `contents` to `path` so a crash leaves either the old file or the new
/// one, never a half-written one.
///
/// Temp file in the same directory — so `rename` stays within one filesystem
/// and is therefore atomic — then optionally `sync_all`, then rename.
///
/// Shared with [`super::ledger`], which needs it more than this module does: a
/// torn authorisation cache costs one round trip, and a torn usage ledger costs
/// the user's money in one direction or ours in the other.
pub fn write_atomically(
    path: &Path,
    contents: &[u8],
    durability: Durability,
) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents)?;
        if durability == Durability::Durable {
            file.sync_all()?;
        }
    }
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// The persisted record
// ---------------------------------------------------------------------------

/// The on-disk form of a verified account.
///
/// A separate type from [`GateState`] on purpose. `GateState` has variants that
/// must never be cached — `Unverified` and `StoreUnavailable` are facts about
/// *this run*, and persisting them would make a transient keychain hiccup
/// survive a restart. Only two things are worth remembering across launches:
/// that the service said yes, and that it said no.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CachedAuthorization {
    /// Format version; see [`FORMAT_VERSION`].
    version: u32,
    /// Which key this verdict is about.
    fingerprint: KeyFingerprint,
    /// The verdict.
    verdict: CachedVerdict,
}

/// The two verdicts worth persisting.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
enum CachedVerdict {
    /// The service said yes at `verified_at`.
    Allowed {
        /// Who.
        user: UserId,
        /// When. Every grace computation is a function of this.
        verified_at: SystemTime,
        /// The last usage snapshot, if we ever got one.
        quota: QuotaKnowledge,
    },
    /// The service said no.
    ///
    /// Persisted so a revoked key does not get a fresh grace window on every
    /// relaunch. Without this, "restart the app" would be a workaround for
    /// revocation.
    Denied {
        /// The service's own reason, verbatim.
        reason: String,
        /// When it said so.
        at: SystemTime,
    },
}

impl CachedAuthorization {
    /// Derive the cacheable part of a state, if there is one.
    ///
    /// Returns `None` for the states that are facts about this run rather than
    /// about the account.
    pub fn from_state(state: &GateState) -> Option<Self> {
        match state {
            GateState::Authorized {
                account, quota, ..
            } => Some(Self {
                version: FORMAT_VERSION,
                fingerprint: account.fingerprint.clone(),
                verdict: CachedVerdict::Allowed {
                    user: account.user,
                    verified_at: account.verified_at,
                    quota: quota.clone(),
                },
            }),
            GateState::Revoked {
                fingerprint,
                reason,
                at,
            } => Some(Self {
                version: FORMAT_VERSION,
                fingerprint: fingerprint.clone(),
                verdict: CachedVerdict::Denied {
                    reason: reason.clone(),
                    at: *at,
                },
            }),
            GateState::SignedOut
            | GateState::StoreUnavailable { .. }
            | GateState::Unverified { .. } => None,
        }
    }

    /// Rehydrate a state for the key we actually hold.
    ///
    /// Returns `None` when the cache is about a different key. That is the
    /// transplant defence: a verdict earned by one credential never applies to
    /// another.
    ///
    /// `now` is taken so a cached `verified_at` in the future — a clock that
    /// moved backwards between runs — is clamped exactly as a fresh one is.
    pub fn into_state(
        self,
        holding: &KeyFingerprint,
        source: KeySource,
        now: SystemTime,
    ) -> Option<GateState> {
        if self.version != FORMAT_VERSION || &self.fingerprint != holding {
            return None;
        }
        Some(match self.verdict {
            CachedVerdict::Allowed {
                user,
                verified_at,
                quota,
            } => GateState::verified(user, self.fingerprint, source, verified_at, now, quota),
            CachedVerdict::Denied { reason, at } => GateState::Revoked {
                fingerprint: self.fingerprint,
                reason,
                at,
            },
        })
    }

    /// Read the cache from `dir`, if it holds anything usable.
    ///
    /// Every failure is `None`: a missing file, unreadable bytes, invalid JSON,
    /// and a version we do not know all mean the same thing to the caller —
    /// *re-authorise*. This is the one place in this module where collapsing
    /// failures is right, because the recovery is identical and cheap, and the
    /// alternative is a user blocked out of their app by a corrupt cache file.
    pub fn load(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(dir.join(AUTHORIZATION_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Write the cache into `dir`.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        write_atomically(&dir.join(AUTHORIZATION_FILE), &bytes, Durability::Durable)
    }

    /// Remove the cache, on sign-out.
    ///
    /// A missing file is success: signing out twice must not fail.
    pub fn clear(dir: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(dir.join(AUTHORIZATION_FILE)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Everything about `account` that a UI needs and no credential.
///
/// Exists so `lindsey` can render a signed-in state without ever holding an
/// [`super::credential::ApiKey`]. The GUI is the place a secret is most likely
/// to end up somewhere permanent — a rendered string, a copied value, a
/// screenshot — and the cheapest defence is that it never has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSummary {
    /// Who.
    pub user: UserId,
    /// `ndx_…` plus the last four characters of the key.
    pub key_hint: String,
    /// Which store the key came from.
    pub source: KeySource,
    /// When the service last said yes.
    pub verified_at: SystemTime,
}

impl AccountSummary {
    /// Build a summary from an account and the display hint of its key.
    pub fn new(account: &Account, key_hint: String, source: KeySource) -> Self {
        Self {
            user: account.user,
            key_hint,
            source,
            verified_at: account.verified_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::mcp::account::credential::ApiKey;

    fn key(body: &str) -> ApiKey {
        ApiKey::parse(&format!("ndx_{body}")).expect("test key parses")
    }

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
    }

    /// A scratch directory that removes itself.
    ///
    /// Hand-rolled rather than pulling in `tempfile`: this crate has no such
    /// dependency and adding one for four tests is not a trade worth making.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "nudox-account-cache-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn allowed_state(k: &ApiKey, verified_at: SystemTime) -> GateState {
        GateState::verified(
            UserId(24),
            k.fingerprint(),
            KeySource::Keychain,
            verified_at,
            verified_at,
            QuotaKnowledge::Unknown,
        )
    }

    #[test]
    fn an_allowed_verdict_round_trips_through_disk_with_its_timestamp_intact() {
        let scratch = Scratch::new("roundtrip");
        let k = key("2f8c41a9b60d47e3a5710c9fbe2d836a4517");
        let state = allowed_state(&k, t0());

        CachedAuthorization::from_state(&state)
            .expect("an authorised state is cacheable")
            .save(scratch.path())
            .expect("save succeeds");

        let restored = CachedAuthorization::load(scratch.path())
            .expect("the file is there")
            .into_state(&k.fingerprint(), KeySource::Keychain, t0())
            .expect("the fingerprint matches");

        match restored {
            GateState::Authorized { account, .. } => {
                assert_eq!(account.user, UserId(24));
                assert_eq!(
                    account.verified_at,
                    t0(),
                    "the grace window is measured from this instant; losing it to \
                     serialisation would silently restart the clock on every launch"
                );
            }
            other => panic!("expected Authorized, got {other:?}"),
        }
    }

    #[test]
    fn a_cached_verdict_does_not_transfer_to_a_different_key() {
        // Paste a revoked key over a good one and the good key's grace must not
        // come with it.
        let scratch = Scratch::new("transplant");
        let good = key("2f8c41a9b60d47e3a5710c9fbe2d836a4517");
        let other = key("0000000000000000000000000000000000000000");

        CachedAuthorization::from_state(&allowed_state(&good, t0()))
            .expect("cacheable")
            .save(scratch.path())
            .expect("save succeeds");

        let loaded = CachedAuthorization::load(scratch.path()).expect("the file is there");
        assert!(
            loaded
                .into_state(&other.fingerprint(), KeySource::Keychain, t0())
                .is_none(),
            "a verdict earned by one key must never authorise another"
        );
    }

    #[test]
    fn a_revocation_survives_a_restart() {
        // Without this, quitting and relaunching would be a workaround for a
        // revoked key: the cache would be empty, the state would be Unverified,
        // and the next authorize would have to fail again to re-establish it.
        let scratch = Scratch::new("revoked");
        let k = key("2f8c41a9b60d47e3a5710c9fbe2d836a4517");
        let state = GateState::Revoked {
            fingerprint: k.fingerprint(),
            reason: "revoked by user".to_owned(),
            at: t0(),
        };

        CachedAuthorization::from_state(&state)
            .expect("a revocation is cacheable")
            .save(scratch.path())
            .expect("save succeeds");

        match CachedAuthorization::load(scratch.path())
            .expect("the file is there")
            .into_state(&k.fingerprint(), KeySource::Keychain, t0())
        {
            Some(GateState::Revoked { reason, .. }) => assert_eq!(reason, "revoked by user"),
            other => panic!("a revocation must survive a restart, got {other:?}"),
        }
    }

    #[test]
    fn transient_states_are_never_written_to_disk() {
        // Persisting "the keychain was locked" would make a one-off failure
        // outlive its cause.
        for state in [
            GateState::SignedOut,
            GateState::StoreUnavailable {
                message: "locked".to_owned(),
                help: "unlock it",
            },
            GateState::Unverified {
                fingerprint: key("2f8c41a9b60d47e3a5710c9fbe2d836a4517").fingerprint(),
                source: KeySource::Keychain,
                probe: crate::mcp::account::state::ProbeStatus::Pending,
            },
        ] {
            assert!(
                CachedAuthorization::from_state(&state).is_none(),
                "{} must not be cacheable",
                state.posture(t0()).tag()
            );
        }
    }

    #[test]
    fn a_corrupt_cache_reads_as_no_cache_rather_than_as_a_failure() {
        let scratch = Scratch::new("corrupt");
        std::fs::write(scratch.path().join(AUTHORIZATION_FILE), b"{ this is not json")
            .expect("write garbage");
        assert!(
            CachedAuthorization::load(scratch.path()).is_none(),
            "a corrupt cache costs one round trip; treating it as an error would \
             lock the user out of their own app"
        );
    }

    #[test]
    fn an_atomic_write_replaces_the_old_contents_and_leaves_no_temp_file() {
        let scratch = Scratch::new("atomic");
        let path = scratch.path().join("thing.json");
        write_atomically(&path, b"first", Durability::Durable).expect("first write");
        write_atomically(&path, b"second", Durability::Fast).expect("second write");
        assert_eq!(std::fs::read(&path).expect("readable"), b"second");
        assert!(
            !path.with_extension("tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
    }

    #[test]
    fn clearing_an_absent_cache_succeeds() {
        let scratch = Scratch::new("clear");
        CachedAuthorization::clear(scratch.path()).expect("clearing nothing is not a failure");
        CachedAuthorization::clear(scratch.path()).expect("and is idempotent");
    }
}
