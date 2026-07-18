//! [`RecordingSession`] — streaming incremental IR recording.
//!
//! A `RecordingSession` allows callers to incrementally stage IR symbols into
//! the repository's working copy in multiple batches, take optional mid-session
//! checkpoints, and finally commit the full change (including deletions) via
//! [`RecordingSession::finish`]. This is the streaming counterpart to
//! [`crate::repo::IrRepository::record_generation`], which requires the full
//! [`nudox_ir::apply::PristineIntroTable`] upfront.
//!
//! # Lifecycle
//!
//! ```text
//! begin_recording(&mut repo)
//!   → RecordingSession
//!       .stage(batch)  // repeat any number of times
//!       [.checkpoint(msg)]  // optional partial commit (no deletions)
//!       .finish()  // deletions + final commit → FinishReport
//!       | .abandon()  // discard (WC stays; next call will resync)
//! ```

use std::collections::HashSet;
use std::io::Write as IoWrite;

use libpijul::changestore::ChangeStore;
use libpijul::pristine::ChannelTxnT;
use libpijul::record::{Algorithm, Builder};
use libpijul::working_copy::{WorkingCopy, WorkingCopyRead};
use libpijul::{MutTxnTExt, TxnTExt};

use nudox_change::{IntroId, StableRef};
use nudox_ir::wire::OwnedEntryPayload;

use crate::error::VcsError;
use crate::repo::{ChangeHashHex, IrRepository, IrTip};
use crate::serialize::{symbol_path, LinkWire};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single symbol to stage during a recording session.
pub struct StagedEntry {
    /// The stable reference for this symbol.  `stable.intro` is the [`IntroId`].
    pub stable: StableRef,
    /// The serialized IR payload for this symbol.
    pub payload: OwnedEntryPayload,
    /// The parent intro (for nesting), if any.
    pub parent: Option<IntroId>,
    /// Links from this symbol's perspective.
    ///
    /// The canonical-owner rule is applied at stage time: a link is stored on
    /// this intro's file only if this intro is the canonical owner.  Because
    /// every `StagedEntry` must belong to this repository's package (enforced
    /// by [`RecordingSession::stage`]), same-package links are owned by the
    /// endpoint with the smaller [`IntroId`] (byte comparison); cross-package
    /// links are always owned by the local (this entry's) endpoint.
    pub links: Vec<LinkWire>,
}

/// Per-[`RecordingSession::stage`] report.
#[derive(Debug, Default)]
pub struct StageReport {
    /// Number of symbols added to the working copy for the first time.
    pub added: u64,
    /// Number of symbols whose content changed and were rewritten.
    pub updated: u64,
    /// Number of symbols whose content was identical to what was already in
    /// the working copy.
    pub unchanged: u64,
    /// Up to 5 (intro, name) pairs from the added/updated symbols.
    pub sample: Vec<(IntroId, String)>,
}

/// Report returned by [`RecordingSession::finish`].
#[derive(Debug)]
pub struct FinishReport {
    /// The repository tip after the session.
    pub tip: IrTip,
    /// The change hash recorded by `finish`, if any content changed.
    pub change: Option<ChangeHashHex>,
    /// Total symbols added across all `stage` calls.
    pub added: u64,
    /// Total symbols updated across all `stage` calls.
    pub updated: u64,
    /// Symbols deleted (present in the tip when the session began, absent
    /// from all `stage` calls).
    pub deleted: u64,
}

/// Streaming incremental IR recording session.
///
/// Obtain one via [`IrRepository::begin_recording`].  Stage entries in any
/// number of batches, optionally checkpoint, then either [`finish`](Self::finish)
/// (records a change) or [`abandon`](Self::abandon) (discards without recording).
pub struct RecordingSession<'r, C: ChangeStore> {
    pub(crate) repo: &'r IrRepository<C>,
    /// Intros staged during this session (union of all `stage` calls).
    staged: HashSet<IntroId>,
    /// Intros present in the WC at session start (populated by `begin_recording`).
    pub(crate) tip_intros: HashSet<IntroId>,
    /// Paths currently tracked in the working copy — populated on the first
    /// `stage` call and kept up-to-date as files are added.  This avoids
    /// re-calling the O(package) `list_files()` on every batch.
    wc_paths: Option<HashSet<String>>,
    /// Cumulative counts.
    total_added: u64,
    total_updated: u64,
    total_unchanged: u64,
}

impl<'r, C> RecordingSession<'r, C>
where
    C: ChangeStore + Clone + Send + 'static,
    C::Error: std::fmt::Display + Send + Sync + 'static,
{
    /// Create a new session from a repository reference and its current WC intros.
    pub(crate) fn new(repo: &'r IrRepository<C>, tip_intros: HashSet<IntroId>) -> Self {
        Self {
            repo,
            staged: HashSet::new(),
            tip_intros,
            wc_paths: None,
            total_added: 0,
            total_updated: 0,
            total_unchanged: 0,
        }
    }

    /// Stage a batch of entries into the working copy.
    ///
    /// All entries must belong to this repository's package
    /// ([`StableRef::package`] == `repo.package_id()`).  An entry whose package
    /// differs is rejected with [`VcsError::ForeignPackage`] **before** any
    /// working-copy mutation for that entry; entries processed before the
    /// foreign one in the same batch are left in the WC (partial-batch
    /// semantics).  The caller is responsible for not mixing packages in a
    /// batch; the session records all entries that pass validation as if they
    /// had been submitted individually.
    ///
    /// Files with identical content to what is already in the working copy are
    /// left untouched (mtime preserved) so libpijul's stat cache can skip
    /// re-diffing them during [`checkpoint`](Self::checkpoint) /
    /// [`finish`](Self::finish).
    ///
    /// # Performance
    ///
    /// One transaction, one channel open, and one `list_files()` snapshot are
    /// shared across the entire batch.  The path snapshot is folded into the
    /// session's `wc_paths` cache so that subsequent `stage` calls never call
    /// `list_files()` again — new paths are inserted as files are added.  The
    /// mtime floor (channel `last_modified`) is computed once per call.
    pub fn stage(&mut self, batch: Vec<StagedEntry>) -> Result<StageReport, VcsError> {
        let mut report = StageReport::default();
        let package_id = self.repo.package_id();

        // ONE txn + ONE channel open for the entire batch.
        let txn = self.repo.arc_txn_pub()?;
        let channel = IrRepository::<C>::open_or_create_channel_pub(&txn, self.repo.channel_name_ref())?;

        // Compute the mtime floor once for the whole batch.
        let channel_ms = txn.read().last_modified(&*channel.read());
        let floor = std::time::UNIX_EPOCH + std::time::Duration::from_millis(channel_ms);
        let mtime_floor = self.repo.now_pub().max(floor);

        // ONE list_files() snapshot, taken at most once per session (subsequent
        // stage() calls reuse + update the cached set).
        if self.wc_paths.is_none() {
            let paths: HashSet<String> = self
                .repo
                .working_copy_ref()
                .list_files()
                .into_iter()
                .collect();
            self.wc_paths = Some(paths);
        }
        // SAFETY: set above if it was None.
        let wc_paths = self.wc_paths.as_mut().expect("wc_paths initialised");

        for entry in batch {
            let intro = entry.stable.intro;

            // SV-10: reject entries that don't belong to this package before
            // any WC mutation.
            if &entry.stable.package != package_id {
                return Err(VcsError::ForeignPackage {
                    expected: format!("{package_id:?}"),
                    got: format!("{:?}", entry.stable.package),
                });
            }

            // Canonical-owner rule: since `entry.stable.package == package_id`
            // is now guaranteed (enforced above), `a_is_local` is always true.
            // For same-package links the owner is the endpoint with the smaller
            // IntroId; for cross-package links the local endpoint (this intro)
            // always owns the link.
            let mut owned_links: Vec<LinkWire> = Vec::new();
            for link in &entry.links {
                let b_intro = link.other.intro;
                let b_is_local = &link.other.package == package_id;

                let owner_intro = if b_is_local {
                    // Both endpoints local: owner = smaller IntroId.
                    if intro.as_bytes() <= b_intro.as_bytes() { intro } else { b_intro }
                } else {
                    // Cross-package: the local endpoint (this intro) owns it.
                    intro
                };

                if owner_intro == intro {
                    owned_links.push(link.clone());
                }
            }

            let bytes = crate::blob::serialize_symbol_blob(&entry.payload, entry.parent, &owned_links);
            let path = symbol_path(intro);

            if wc_paths.contains(&path) {
                // Read and compare.
                let mut existing = Vec::new();
                self.repo
                    .working_copy_ref()
                    .read_file(&path, &mut existing)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file (compare) {path}: {e}")))?;

                if existing != bytes {
                    self.repo
                        .working_copy_ref()
                        .write_file(&path, libpijul::pristine::Inode::ROOT)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file open: {e}")))?
                        .write_all(&bytes)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file write: {e}")))?;

                    // Clamp mtime so modified files are never invisible to
                    // libpijul's stat cache.
                    self.repo
                        .working_copy_ref()
                        .touch(&path, mtime_floor)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touch {path}: {e}")))?;

                    if self.tip_intros.contains(&intro) {
                        report.updated += 1;
                    } else {
                        report.added += 1;
                    }
                    // Single push site for sample (covers both added and updated).
                    if report.sample.len() < 5 {
                        report.sample.push((intro, entry.payload.symbol.name.clone()));
                    }
                } else {
                    report.unchanged += 1;
                }
            } else {
                // New file: add to WC and register in transaction.
                self.repo.working_copy_ref().add_file(&path, bytes);
                txn.write().add_file(&path, 0).map_err(|e| {
                    VcsError::Pijul(anyhow::anyhow!("add_file: {e}"))
                })?;
                // Touch the newly-added file to ensure mtime >= channel tip.
                self.repo
                    .working_copy_ref()
                    .touch(&path, mtime_floor)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touch (new) {path}: {e}")))?;

                // Track the new path so subsequent stage() calls don't re-list.
                wc_paths.insert(path.clone());

                report.added += 1;
                if report.sample.len() < 5 {
                    report.sample.push((intro, entry.payload.symbol.name.clone()));
                }
            }

            // Always track as staged (even if unchanged content).
            self.staged.insert(intro);
        }

        // ONE commit for the entire batch.
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (stage): {e}"))
        })?;

        self.total_added += report.added;
        self.total_updated += report.updated;
        self.total_unchanged += report.unchanged;
        Ok(report)
    }

    /// Stage additional links for a symbol that was already staged.
    ///
    /// This is the links-only path used by the stream host when the producer
    /// emits a `Links { from, batch }` frame after the symbol was already
    /// staged via a `Symbols` batch. We cannot re-stage the full entry because
    /// we don't have the payload here (and re-encoding a synthetic payload would
    /// violate payload semantics). Instead, we read the existing blob, merge the
    /// new links under the canonical-owner rule, and rewrite the file only if the
    /// merged content actually differs.
    ///
    /// SV-10 enforcement: `from` must belong to this repository's package.
    /// Links whose canonical owner is the `from` intro are stored; non-owned
    /// links (where the other endpoint is local and has a smaller `IntroId`) are
    /// silently dropped — the canonical owner will store them when its own entry
    /// is staged.
    ///
    /// If `from` has not been staged yet in this session, the call is a no-op
    /// for the links (the entry's own `stage` call will carry the correct links).
    /// If it has been staged, the file is updated in place.
    pub fn stage_links(&mut self, from: StableRef, links: Vec<LinkWire>) -> Result<(), VcsError> {
        let package_id = self.repo.package_id();

        // SV-10: only accept links whose `from` belongs to this package.
        if &from.package != package_id {
            return Err(VcsError::ForeignPackage {
                expected: format!("{package_id:?}"),
                got: format!("{:?}", from.package),
            });
        }

        let intro = from.intro;
        let path = crate::serialize::symbol_path(intro);

        // Ensure wc_paths is initialized.
        if self.wc_paths.is_none() {
            let paths: HashSet<String> = self
                .repo
                .working_copy_ref()
                .list_files()
                .into_iter()
                .collect();
            self.wc_paths = Some(paths);
        }
        let wc_paths = self.wc_paths.as_mut().expect("wc_paths initialised");

        // If the symbol file doesn't exist yet, nothing to do — it will be
        // created when stage() is called for this intro.
        if !wc_paths.contains(&path) {
            return Ok(());
        }

        // Read the existing blob.
        let mut existing = Vec::new();
        self.repo
            .working_copy_ref()
            .read_file(&path, &mut existing)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file (stage_links) {path}: {e}")))?;

        // Parse the blob to extract the current payload + parent, then merge links.
        let view = crate::blob::SymbolView::from_bytes(&existing).map_err(|e| {
            VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
        })?;
        let payload = view.to_owned_payload().map_err(|e| {
            VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
        })?;
        let parent = view.parent();

        // Collect existing links.
        let mut merged: Vec<LinkWire> = view
            .links()
            .map(|l| LinkWire {
                other: l.to_stable_ref(),
                kind_self: l.kind_self,
                kind_other: l.kind_other,
            })
            .collect();

        // Apply canonical-owner rule for new links and merge in.
        for link in &links {
            let b_intro = link.other.intro;
            let b_is_local = &link.other.package == package_id;
            let owner_intro = if b_is_local {
                if intro.as_bytes() <= b_intro.as_bytes() { intro } else { b_intro }
            } else {
                intro
            };
            if owner_intro == intro && !merged.iter().any(|m| m.other == link.other) {
                merged.push(link.clone());
            }
        }

        let new_bytes = crate::blob::serialize_symbol_blob(&payload, parent, &merged);

        if new_bytes == existing {
            return Ok(());
        }

        let txn = self.repo.arc_txn_pub()?;
        let channel_ms = {
            let ch = IrRepository::<C>::open_or_create_channel_pub(&txn, self.repo.channel_name_ref())?;
            txn.read().last_modified(&*ch.read())
        };
        let floor = std::time::UNIX_EPOCH + std::time::Duration::from_millis(channel_ms);
        let mtime_floor = self.repo.now_pub().max(floor);

        self.repo
            .working_copy_ref()
            .write_file(&path, libpijul::pristine::Inode::ROOT)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file (stage_links): {e}")))?
            .write_all(&new_bytes)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write (stage_links): {e}")))?;
        self.repo
            .working_copy_ref()
            .touch(&path, mtime_floor)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touch (stage_links): {e}")))?;

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (stage_links): {e}"))
        })?;

        Ok(())
    }

    /// Record the currently staged symbols as a libpijul change, without
    /// performing deletions.
    ///
    /// This is a partial commit: symbols that have not yet been staged are
    /// still present in the working copy.  Only `finish` performs deletions.
    ///
    /// Returns the change hash, or `None` if nothing has changed since the
    /// last checkpoint (or since session start).
    pub fn checkpoint(&mut self, msg: &str) -> Result<Option<ChangeHashHex>, VcsError> {
        self.record_and_apply(msg)
    }

    /// Finalize the session.
    ///
    /// 1. Deletes WC files for intros that were in `tip_intros` but not staged.
    /// 2. Records a final change (or `None` if nothing changed).
    /// 3. Returns [`FinishReport`] with cumulative counts and the final tip.
    pub fn finish(self) -> Result<FinishReport, VcsError> {
        // Determine deletions: intros that existed at session start but were
        // not staged during the session.
        let to_delete: Vec<IntroId> = self
            .tip_intros
            .iter()
            .filter(|i| !self.staged.contains(i))
            .copied()
            .collect();
        let deleted = to_delete.len() as u64;

        if !to_delete.is_empty() {
            let txn = self.repo.arc_txn_pub()?;
            for intro in &to_delete {
                let path = symbol_path(*intro);
                self.repo
                    .working_copy_ref()
                    .remove_path(&path, false)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("remove_path: {e}")))?;
                txn.write().remove_file(&path).map_err(|e| {
                    VcsError::Pijul(anyhow::anyhow!("remove_file: {e}"))
                })?;
            }
            txn.commit().map_err(|e| {
                VcsError::Pijul(anyhow::anyhow!("commit (finish-delete): {e}"))
            })?;
        }

        let change = self.record_and_apply("finish")?;
        let tip = self.repo.tip()?;

        Ok(FinishReport {
            tip,
            change,
            added: self.total_added,
            updated: self.total_updated,
            deleted,
        })
    }

    /// Abandon the session without recording a change.
    ///
    /// The working copy may contain staged files; the next `record_generation`
    /// or `begin_recording` call will trigger a full WC resync because
    /// `working_copy_tip` is reset to zero here.
    pub fn abandon(self) -> Result<(), VcsError> {
        self.repo.reset_working_copy_tip();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal: record + apply + commit the current WC state.
    // -----------------------------------------------------------------------

    fn record_and_apply(&self, msg: &str) -> Result<Option<ChangeHashHex>, VcsError> {
        let txn = self.repo.arc_txn_pub()?;
        let channel = IrRepository::<C>::open_or_create_channel_pub(&txn, self.repo.channel_name_ref())?;

        let mut builder = Builder::new();
        builder
            .record(
                txn.clone(),
                Algorithm::default(),
                false,
                &libpijul::DEFAULT_SEPARATOR,
                channel.clone(),
                self.repo.working_copy_ref(),
                self.repo.changes_ref(),
                "",
                1,
            )
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("record: {e}")))?;

        let rec = builder.finish();

        if rec.actions.is_empty() {
            txn.commit().map_err(|e| {
                VcsError::Pijul(anyhow::anyhow!("commit (no-op): {e}"))
            })?;
            return Ok(None);
        }

        let actions = rec
            .actions
            .into_iter()
            .map(|a| {
                a.globalize(&*txn.read())
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("globalize: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let contents = std::mem::take(&mut *rec.contents.lock());
        let mut change = libpijul::change::Change::make_change(
            &*txn.read(),
            &channel,
            actions,
            contents,
            libpijul::change::ChangeHeader {
                message: msg.to_string(),
                authors: vec![],
                description: None,
                timestamp: jiff::Timestamp::now(),
            },
            Vec::new(),
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("make_change: {e:?}")))?;

        let hash = self
            .repo
            .changes_ref()
            .save_change(&mut change, |_, _| Ok::<_, anyhow::Error>(()))
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("save_change: {e}")))?;

        libpijul::apply::apply_local_change(
            &mut *txn.write(),
            &channel,
            &change,
            &hash,
            &rec.updatables,
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("apply_local_change: {e}")))?;

        let new_tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state (post-apply): {e}")))?
            .to_bytes();

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit: {e}"))
        })?;

        self.repo.set_working_copy_tip(new_tip);

        Ok(Some(ChangeHashHex(crate::repo::hash_to_hex_pub(&hash))))
    }
}
