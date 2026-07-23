//! [`GitMonitor`] (INDEX-PLAN §6.4): poll one cpp stem's git refs, diff against
//! the stored `git_watermarks` row, and on change emit
//! [`CatalogOp::SourceMoved`] plus a fresh version enumeration
//! (REGISTRYLESS-PLAN §7.4 step 4).
//!
//! # Behaviour
//!
//! `tick(stem)`:
//! 1. `ls-remote` the stem's URL via the [`GitRepository`] adapter and compute a
//!    single **combined ref digest** — a deterministic fingerprint of every
//!    `(oid, ref)` pair — as the comparable watermark. HEAD is the natural
//!    single-rev anchor, but a monitor must react to *any* new tag, so the
//!    digest over all refs is the honest high-water mark.
//! 2. Read the stem's `git_watermarks.last_rev`. Equal digest ⇒ nothing moved;
//!    return with the checked-at time bumped (no ops, no error).
//! 3. Changed digest ⇒ emit `SourceMoved { rev: <digest> }` and re-enumerate
//!    versions (`enumerate::enumerate_git_versions`), returning both as one op
//!    batch the driver applies atomically.
//! 4. On a git failure the tick returns a [`MonitorError`] carrying the message
//!    the caller writes to `git_watermarks.last_error`; the watermark's
//!    `last_rev` is **not** advanced on failure.
//!
//! The monitor is pure over its adapter, so tests drive it with a fake
//! [`GitRepository`]. Persisting the watermark and applying ops is the driver's
//! job; this type hands back a typed [`TickOutcome`] describing what to persist.

use crate::ids::PackageStemId;
use crate::protocol::{CatalogOp, GitRev};

use crate::ingest::enumerate::{enumerate_git_versions, EnumerateError};
use crate::ingest::git::{GitRepository, GitRepositoryError};

/// What one [`GitMonitor::tick`] observed — the driver turns this into a
/// watermark write and (when changed) an atomic op batch.
#[derive(Debug)]
pub enum TickOutcome {
    /// The remote's ref set is unchanged since `last_rev`. Persist only the new
    /// `checked_at`; emit nothing.
    Unchanged {
        /// The combined ref digest (equal to the stored `last_rev`).
        rev: String,
    },
    /// The remote moved. Apply `ops` atomically, then persist `rev` as the new
    /// `git_watermarks.last_rev`.
    Moved {
        /// The new combined ref digest to store as `last_rev`.
        rev: String,
        /// `SourceMoved` followed by the re-enumerated `UpsertVersion` ops.
        ops: Vec<CatalogOp>,
    },
}

/// Why a tick failed. The caller records `to_string()` in
/// `git_watermarks.last_error` and leaves `last_rev` untouched (INDEX-PLAN §6.4).
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// The git adapter failed to list refs.
    #[error(transparent)]
    Git(#[from] GitRepositoryError),
    /// Enumeration of the moved stem's versions failed.
    #[error(transparent)]
    Enumerate(#[from] EnumerateError),
}

/// Polls cpp direct-git stems for ref movement (INDEX-PLAN §6.4).
pub struct GitMonitor<Repository: GitRepository> {
    git: Repository,
}

impl<Repository: GitRepository> GitMonitor<Repository> {
    /// Wrap a git adapter.
    pub fn new(git: Repository) -> Self {
        Self { git }
    }

    /// Poll one stem's refs and diff against `last_rev` (INDEX-PLAN §6.4).
    ///
    /// `stem_id` names the catalog stem; `repo_slug` / `repo_url` are its
    /// normalized identity and fetch URL; `last_rev` is the stored
    /// `git_watermarks.last_rev` (`None` on first tick); `checked_at` is the
    /// wall-clock the caller will also stamp into the watermark; `commit_time`
    /// feeds pseudo-version synthesis on an untagged move.
    pub fn tick(
        &self,
        stem_id: PackageStemId,
        repo_slug: &str,
        repo_url: &str,
        last_rev: Option<&str>,
        checked_at: i64,
        commit_time: u64,
    ) -> Result<TickOutcome, MonitorError> {
        let refs = self.git.list_remote_refs(repo_url)?;
        let digest = combined_ref_digest(&refs);

        if last_rev == Some(digest.as_str()) {
            return Ok(TickOutcome::Unchanged { rev: digest });
        }

        // The ref set moved: record the move and re-enumerate versions so new
        // tags become versions incrementally (REGISTRYLESS-PLAN §7.4 step 4).
        let mut ops = Vec::new();
        ops.push(CatalogOp::SourceMoved {
            stem: stem_id,
            rev: GitRev(digest.clone().into()),
            checked_at,
        });
        ops.extend(enumerate_git_versions(&self.git, repo_slug, repo_url, commit_time)?);

        Ok(TickOutcome::Moved { rev: digest, ops })
    }
}

/// A deterministic fingerprint of a remote's entire ref set, order-independent
/// so a reordered `ls-remote` does not read as a move. Two remotes with the
/// same `(oid, ref)` multiset produce the same digest; any added, removed, or
/// repointed ref changes it.
fn combined_ref_digest(refs: &[crate::ingest::git::LsRemoteRef]) -> String {
    use sha2::{Digest, Sha256};

    // Sort so the digest is independent of line order.
    let mut lines: Vec<String> =
        refs.iter().map(|entry| format!("{}\t{}", entry.object_id, entry.reference)).collect();
    lines.sort();

    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    // Hex-encode; a 64-char string fits `git_watermarks.last_rev` (TEXT).
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
