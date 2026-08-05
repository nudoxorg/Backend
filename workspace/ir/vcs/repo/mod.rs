//! [`IrRepository`] — libpijul-backed IR versioning for a single package.
//!
//! Maps the package's materialized IR onto a `symbols/{intro_hex}` working
//! tree, drives libpijul's record/apply/output/unrecord, and exposes a stable
//! API for recording generations, materializing the current tip, and sealing
//! serve archives.

use std::io;
use std::io::Write as IoWrite;
use std::path::Path;

use libpijul::changestore::ChangeStore;
use libpijul::changestore::filesystem::FileSystem as FsChanges;
use libpijul::changestore::memory::Memory as MemChanges;
use libpijul::pristine::sanakirja::{Pristine, SanakirjaError};
use libpijul::pristine::{ArcTxn, ChannelRef, ChannelTxnT, Hash, Merkle, MutTxnT, Position, TxnT};
use libpijul::record::{Algorithm, Builder};
use libpijul::working_copy::memory::Memory as MemWc;
use libpijul::working_copy::{WorkingCopy, WorkingCopyRead};
use libpijul::{MutTxnTExt, TxnTExt};

use crate::archive::{SealEntry, SealedArchive, seal_from_entries};
use crate::vcs_types::{ChangeSetFingerprint, LinkRecord};
use crate::wire::{OwnedEntryPayload, PayloadTable};
use ir::apply::PristineIntroTable;
use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::kind::KindDiscriminant;

use crate::checkout::MaterializedIndex;
use crate::error::VcsError;
use crate::serialize::{LinkWire, intro_hex_of, is_symbol_path, symbol_path};

/// Derive the type-skeleton fingerprint for a type-alias payload. F1 no longer
/// stores it as a frame (§6.2), so the seal path recomputes it from the alias's
/// type expression; non-alias kinds have no type fingerprint.
fn type_fingerprint_of(p: &OwnedEntryPayload) -> Option<crate::vcs_types::TypeFingerprintId> {
    if let crate::wire::KindWire::Type(alias) = &p.kind {
        Some(crate::vcs_types::type_fingerprint(&alias.ty))
    } else {
        None
    }
}
use crate::refs::{BranchName, Ref, ResolvedRef, TagName};
use crate::serve_cache::{ServeCache, ServeSource, ServedArchive};
use crate::version::{VersionLabel, VersionState};

// ---------------------------------------------------------------------------
// Public newtypes
// ---------------------------------------------------------------------------

/// The hex-encoded hash of a libpijul change (e.g. `"3ae7bc..."`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeHashHex(pub String);

/// The tip fingerprint of a channel at a given moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrTip {
    merkle_bytes: [u8; 32],
}

impl IrTip {
    /// Expose the tip as a [`ChangeSetFingerprint`] (equality-only digest).
    pub fn fingerprint(&self) -> ChangeSetFingerprint {
        ChangeSetFingerprint::from_raw(self.merkle_bytes)
    }
}

// ---------------------------------------------------------------------------
// Helper: hash ↔ hex
// ---------------------------------------------------------------------------

/// `pub(crate)` helper for session.rs to convert a libpijul `Hash` to its hex string.
pub(crate) fn hash_to_hex_pub(h: &Hash) -> String {
    hash_to_hex(h)
}

fn hash_to_hex(h: &Hash) -> String {
    match h {
        Hash::Blake3(b) => {
            let mut s = String::with_capacity(64);
            for byte in b {
                s.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
                s.push(char::from_digit((byte & 0xf) as u32, 16).unwrap());
            }
            s
        }
        _ => panic!("unexpected hash algorithm"),
    }
}

fn hex_to_hash(hex: &str) -> Result<Hash, VcsError> {
    if hex.len() != 64 {
        return Err(VcsError::InvalidHexDigit);
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or(VcsError::InvalidHexDigit)? as u8;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or(VcsError::InvalidHexDigit)? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(Hash::Blake3(bytes))
}

// ---------------------------------------------------------------------------
// IrRepository
// ---------------------------------------------------------------------------

/// LRU capacity (open change files) for the filesystem changestore.
const CHANGESTORE_CACHE: usize = 128;

/// The delta plan: `(changed_intros_to_output, current_intro_set)`.
type DeltaPlan = (Vec<IntroId>, std::collections::HashSet<IntroId>);

/// A libpijul-backed version store for one package's IR, generic over the
/// changestore backend `C` ([`MemChanges`] for tests, [`FsChanges`] for durable
/// on-disk storage).
///
/// Owns a pristine (sanakirja), a changestore, and an in-memory scratch working
/// copy. A single channel tracks the linear history of IR generations. The
/// durable state is the pristine + changestore; the working copy is ephemeral
/// and re-derived by `materialize`.
pub struct IrRepository<C = MemChanges> {
    env: Pristine,
    changes: C,
    working_copy: MemWc,
    channel_name: String,
    #[allow(dead_code)]
    package: PackageLineageId,
    /// The channel tip that `self.working_copy` currently reflects.
    ///
    /// Initialised to `[0u8; 32]` (the zero-state, which is never a real
    /// tip) so that the very first call to `record_generation` on a
    /// freshly-opened or newly-created repository always falls through to
    /// the one-time resync path.
    ///
    /// Updated to `current_state().to_bytes()` at the end of every method
    /// that leaves `self.working_copy` consistent with a committed tip:
    /// - `record_generation` (after apply + commit)
    /// - `unrecord` (after the unrecord + commit)
    ///
    /// `materialize`, `materialize_index`, and `checkout_symbol` use fresh
    /// throwaway working copies; they never touch `self.working_copy` and
    /// therefore never update this field.
    working_copy_tip: std::cell::Cell<[u8; 32]>,
    /// How many whole-tree `output_repository_no_pending` calls were made
    /// from `record_generation`'s baseline-sync step.  Exposed to tests
    /// via `sync_output_count()` to verify the O(delta) invariant.
    #[cfg(test)]
    sync_output_count: std::cell::Cell<u64>,
    /// Test-only wall-clock override for `record_generation`'s write stamps.
    /// Simulates a backwards-stepping `SystemTime` (see `stamp_written`).
    #[cfg(test)]
    mock_now: std::cell::Cell<Option<std::time::SystemTime>>,
}

impl IrRepository<MemChanges> {
    /// In-memory pristine (anon sanakirja) + memory changestore — fast,
    /// ephemeral, for tests.
    pub fn in_memory(package: PackageLineageId, channel: &str) -> Result<Self, VcsError> {
        // The working channel is a branch; validate it so `current_branch` is
        // always well-formed and the working branch can't sit in a reserved
        // (tag/ or version/) namespace.
        let branch = BranchName::new(channel)?;
        let env = Pristine::new_anon().map_err(|e| pijul_err(e))?;
        Ok(Self {
            env,
            changes: MemChanges::new(),
            working_copy: MemWc::new(),
            channel_name: branch.channel_name(),
            package,
            working_copy_tip: std::cell::Cell::new([0u8; 32]),
            #[cfg(test)]
            sync_output_count: std::cell::Cell::new(0),
            #[cfg(test)]
            mock_now: std::cell::Cell::new(None),
        })
    }
}

impl IrRepository<FsChanges> {
    /// On-disk pristine (sanakirja under `root/pristine`) + **durable filesystem
    /// changestore** (under `root/changes`) — for production use. Both the
    /// pristine and every recorded change persist across process restarts; the
    /// working copy is ephemeral scratch, re-derived by `materialize`.
    pub fn open(root: &Path, package: PackageLineageId, channel: &str) -> Result<Self, VcsError> {
        let branch = BranchName::new(channel)?;
        std::fs::create_dir_all(root.join("changes")).map_err(VcsError::Io)?;
        let env = Pristine::new(root.join("pristine")).map_err(|e: SanakirjaError| pijul_err(e))?;
        let changes = FsChanges::from_root(root.join("changes"), CHANGESTORE_CACHE);
        Ok(Self {
            env,
            changes,
            working_copy: MemWc::new(),
            channel_name: branch.channel_name(),
            package,
            working_copy_tip: std::cell::Cell::new([0u8; 32]),
            #[cfg(test)]
            sync_output_count: std::cell::Cell::new(0),
            #[cfg(test)]
            mock_now: std::cell::Cell::new(None),
        })
    }

    /// Apply external change files (already written to the filesystem changestore
    /// by `nudox-sync`'s `FsChangeIo`) to this repository's working channel.
    ///
    /// This is the **iroh-sync import path**: the caller (`nudox-sync`'s
    /// `RepoApplyHook`) must ensure all change files have been written to the
    /// on-disk changestore under `<repo-root>/changes/` via `FsChangeIo::write_change`
    /// before calling this method.
    ///
    /// For each hash in `hashes` (in dependency order, oldest first):
    /// - Parses the hex to a `libpijul::Hash`.
    /// - Skips if the change is already on the channel (idempotent).
    /// - Verifies the file is present in the changestore; returns
    ///   [`VcsError::ChangeMissingFromStore`] if not.
    /// - Applies via `libpijul::apply::apply_change_arc` (plain sequential
    ///   apply; `_rec` is not needed because the caller passes changes in
    ///   announcement order, which is already dependency-respecting — pijul
    ///   enforces this in `TipAnnouncement::changes`).
    ///
    /// After all changes are applied, commits the transaction and calls
    /// `reset_working_copy_tip()` to mark the in-memory working copy as stale.
    /// The next `record_generation` or `begin_recording` call will re-sync the
    /// working copy from the new channel tip via the existing stale-WC path.
    ///
    /// Returns the new channel tip as an [`IrTip`].
    pub fn apply_external_changes(&self, hashes: &[ChangeHashHex]) -> Result<IrTip, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        for hex in hashes {
            let hash = hex_to_hash(&hex.0)?;

            // Check if already on the channel (idempotent).
            let already_applied = txn
                .read()
                .has_change(&channel, &hash)
                .map_err(|e| pijul_err(e))?
                .is_some();
            if already_applied {
                continue;
            }

            // Verify the change file is present in the on-disk changestore.
            if !self.changes.has_change(&hash) {
                return Err(VcsError::ChangeMissingFromStore {
                    hash: hex.0.clone(),
                });
            }

            // Apply: plain `apply_change_arc` in dependency order. The `_rec`
            // variant recursively pulls transitive dependencies from the
            // changestore; we don't need it here because the announcement order
            // is already dependency-respecting (earlier entries are deps of
            // later ones) and all files are already in the store.
            libpijul::apply::apply_change_arc(&self.changes, &txn, &channel, &hash).map_err(
                |e| {
                    VcsError::Pijul(Box::new(std::io::Error::other(format_args!(
                        "apply_change_arc {}: {e}",
                        hex.0
                    ))))
                },
            )?;
        }

        // Snapshot the tip and commit.
        let merkle = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?;
        txn.commit().map_err(|e| pijul_err(e))?;

        // The in-memory working copy no longer reflects the channel tip; force
        // resync on the next record/begin_recording call.
        self.reset_working_copy_tip();

        Ok(IrTip {
            merkle_bytes: merkle.to_bytes(),
        })
    }
}

impl<C> IrRepository<C>
where
    C: ChangeStore + Clone + Send + 'static,
    C::Error: std::fmt::Display + Send + Sync + 'static,
{
    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// The wall clock, overridable in tests to simulate a backwards step.
    fn now(&self) -> std::time::SystemTime {
        #[cfg(test)]
        if let Some(t) = self.mock_now.get() {
            return t;
        }
        std::time::SystemTime::now()
    }

    fn arc_txn(&self) -> Result<ArcTxn<libpijul::pristine::sanakirja::MutTxn0>, VcsError> {
        self.env.arc_txn_begin().map_err(|e| pijul_err(e))
    }

    fn open_or_create_channel<T>(txn: &ArcTxn<T>, name: &str) -> Result<ChannelRef<T>, VcsError>
    where
        T: libpijul::pristine::MutTxnT + Send + Sync + 'static,
    {
        txn.write()
            .open_or_create_channel(name)
            .map_err(|e| pijul_err(e))
    }

    /// Output the current channel state into the working copy so that
    /// `working_copy` reflects the recorded IR.
    ///
    /// This is the O(package) baseline resync. In the steady state it is only
    /// called once per fresh process open (stale WC path). Subsequent calls to
    /// `record_generation` skip it when `working_copy_tip` already matches the
    /// channel tip.
    fn sync_output<T>(&self, txn: &ArcTxn<T>, channel: &ChannelRef<T>) -> Result<(), VcsError>
    where
        T: libpijul::pristine::MutTxnT
            + libpijul::pristine::ChannelMutTxnT
            + libpijul::pristine::TreeMutTxnT<
                TreeError = <T as libpijul::pristine::GraphTxnT>::GraphError,
            > + Send
            + Sync
            + 'static,
        T::Channel: Send + Sync + 'static,
        libpijul::working_copy::memory::Error: Send + Sync + 'static,
        libpijul::changestore::memory::Error: Send + 'static,
    {
        #[cfg(test)]
        self.sync_output_count.set(self.sync_output_count.get() + 1);

        libpijul::output::output_repository_no_pending(
            &self.working_copy,
            &self.changes,
            txn,
            channel,
            "",   // prefix: whole tree
            true, // output_name_conflicts
            None, // if_modified_since
            1,    // n_workers
            0,    // salt
        )
        .map_err(|e| pijul_err(e))?;
        Ok(())
    }

    /// Return how many whole-tree baseline-sync outputs `record_generation` has
    /// performed since this repository was created.  Used in tests to verify the
    /// O(delta) invariant (steady-state recordings must NOT trigger a resync).
    #[cfg(test)]
    pub(crate) fn sync_output_count(&self) -> u64 {
        self.sync_output_count.get()
    }

    // -----------------------------------------------------------------------
    // record_generation
    // -----------------------------------------------------------------------

    /// Serialize `ir` into the working tree and record it as one libpijul change.
    ///
    /// Returns `Some(hex)` if any files changed (i.e. a change was recorded),
    /// or `None` if the IR is identical to the current channel tip (no-op).
    pub fn record_generation(&self, ir: &PayloadTable) -> Result<Option<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        // 1. Ensure the working copy reflects the current channel tip.
        //
        //    Fast path (O(1)): if `working_copy_tip` already equals the
        //    current channel tip, `self.working_copy` is already up to date
        //    and the whole-tree output is unnecessary.  This is the steady
        //    state for every recording after the first.
        //
        //    Slow path (O(package)): on a fresh `open()` or after any method
        //    that left `working_copy_tip` stale (e.g. process restart with a
        //    durable repo whose on-disk pristine has changes the new empty WC
        //    does not know about), we do one-time whole-tree output to resync,
        //    then record `working_copy_tip`.  Only this path pays the resync
        //    cost.
        {
            let current_tip = txn
                .read()
                .current_state(&channel.read())
                .map_err(|e| pijul_err(e))?
                .to_bytes();

            if current_tip != self.working_copy_tip.get() {
                // WC is stale (or fresh process re-open): resync from channel.
                self.sync_output(&txn, &channel)?;
                // Mark WC as current immediately; if an error occurs later we
                // will not re-enter this branch on the next call (the WC is
                // now populated).  That is safe: the next call simply
                // proceeds from the already-synced WC.
                self.working_copy_tip.set(current_tip);
            }
            // else: WC already reflects the channel tip; skip the expensive
            // whole-tree output.
        }

        // 2. Compute the desired file set from `ir`.
        //    For each live intro we build a symbol blob. Links are stored on
        //    the canonical-owner side: the endpoint whose IntroId bytes are
        //    smallest (or, for cross-package links, the local endpoint).
        let package_id = &self.package;

        // Build a map: intro → (payload, parent, Vec<LinkWire>)
        let mut desired: std::collections::HashMap<
            IntroId,
            (OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>),
        > = std::collections::HashMap::new();
        for (intro, payload) in ir.live_entries() {
            let parent = ir.parent_of(intro);
            desired
                .entry(intro)
                .or_insert_with(|| (payload.clone(), parent, Vec::new()));
        }

        // Attach links to canonical owners.
        for link in ir.links() {
            // Determine which endpoint owns this link.
            // If both are from the same package, use the smaller IntroId.
            // For cross-package links, the local endpoint owns it.
            let a_intro = link.a.intro;
            let b_intro = link.b.intro;
            let a_is_local = &link.a.package == package_id;
            let b_is_local = &link.b.package == package_id;

            let owner_intro = if a_is_local && b_is_local {
                // Same package: owner is the smaller IntroId.
                if a_intro.as_bytes() <= b_intro.as_bytes() {
                    a_intro
                } else {
                    b_intro
                }
            } else if a_is_local {
                a_intro
            } else if b_is_local {
                b_intro
            } else {
                // Neither endpoint is local — skip (shouldn't happen in a local table).
                continue;
            };

            // Build the LinkWire from the owner's perspective.
            let (kind_self, kind_other, other) = if owner_intro == a_intro {
                (link.kind_a, link.kind_b, link.b.clone())
            } else {
                (link.kind_b, link.kind_a, link.a.clone())
            };

            if let Some(entry) = desired.get_mut(&owner_intro) {
                entry.2.push(LinkWire {
                    other,
                    kind_self,
                    kind_other,
                });
            }
            // If owner_intro is not in desired (deleted intro owns a link — shouldn't happen),
            // we skip silently.
        }

        // 3. Determine which files to add/update/remove.
        //    Current files in working copy:
        let current_files: std::collections::HashSet<String> = self
            .working_copy
            .list_files()
            .into_iter()
            .filter(|p| is_symbol_path(p))
            .collect();

        // NOTE: we intentionally do NOT create the `symbols/` directory inode
        // ourselves. libpijul's `add_file("symbols/{intro}", …)` auto-creates
        // the parent directory in both the working copy and the pristine tree.
        // Manually adding a `symbols` *directory* inode to the memory working
        // copy makes `unrecord`'s re-output call `touch("symbols")` on a
        // directory, which panics with `unreachable!()` inside libpijul.

        // Files we want to exist:
        let desired_files: std::collections::HashSet<String> =
            desired.keys().map(|intro| symbol_path(*intro)).collect();

        // Files to remove (were live, now gone):
        for path in current_files.difference(&desired_files) {
            self.working_copy
                .remove_path(path, false)
                .map_err(|e| pijul_err(e))?;
            txn.write().remove_file(path).map_err(|e| pijul_err(e))?;
        }

        // Files to add or update:
        for (intro, (payload, parent, links)) in &desired {
            let path = symbol_path(*intro);
            // NdIrF1 canonical format — text, one frame per field, TAB-separated.
            // libpijul diffs at the line level (one field per line).
            let bytes = crate::f1::serialize_f1(payload, *parent, links);

            if current_files.contains(&path) {
                // Update — but only if the content actually differs.
                //
                // Skipping an identical rewrite preserves the file's mtime, so
                // libpijul's stat cache (`modified_since_last_commit`) skips
                // re-diffing it during `record`. That makes the record diff
                // O(changed symbols) instead of O(package): the expensive
                // per-file Myers diff runs only for symbols whose bytes moved,
                // exactly matching the O(delta) we already get on the output
                // side. Correctness is independent of the stat cache — we only
                // ever skip a *no-op* write, never a real change.
                let mut existing = Vec::new();
                self.working_copy
                    .read_file(&path, &mut existing)
                    .map_err(|e| pijul_err(e))?;
                if existing != bytes {
                    self.working_copy
                        .write_file(&path, libpijul::pristine::Inode::ROOT)
                        .map_err(|e| pijul_err(e))?
                        .write_all(&bytes)
                        .map_err(|e| pijul_err(e))?;
                    // libpijul's record consults a per-file stat cache: a file
                    // is re-diffed only when its mtime is >= the channel's
                    // last-modified time (truncated to the second). The mtime
                    // comes from the non-monotonic wall clock, so a backwards
                    // step across a second boundary between two recordings
                    // would make this really-changed file look stale and its
                    // modification would be silently dropped from the change.
                    // Clamp the stamp so a written file can never predate the
                    // channel tip.
                    let channel_ms = txn.read().last_modified(&*channel.read());
                    let floor =
                        std::time::UNIX_EPOCH + std::time::Duration::from_millis(channel_ms);
                    self.working_copy
                        .touch(&path, self.now().max(floor))
                        .map_err(|e| pijul_err(e))?;
                }
                // else: identical content — leave the file and its mtime alone.
            } else {
                // Add: put content into working copy and track in txn.
                self.working_copy.add_file(&path, bytes);
                txn.write().add_file(&path, 0).map_err(|e| pijul_err(e))?;
            }
        }

        // 4. Record.
        let mut builder = Builder::new();
        builder
            .record(
                txn.clone(),
                Algorithm::default(),
                false,
                &libpijul::DEFAULT_SEPARATOR,
                channel.clone(),
                &self.working_copy,
                &self.changes,
                "", // prefix: whole tree
                1,  // n_workers
            )
            .map_err(|e| pijul_err(e))?;

        let rec = builder.finish();

        // If there are no actions, nothing changed.
        if rec.actions.is_empty() {
            txn.commit().map_err(|e| pijul_err(e))?;
            return Ok(None);
        }

        // 5. Globalize actions.
        let actions = rec
            .actions
            .into_iter()
            .map(|a| a.globalize(&*txn.read()).map_err(|e| pijul_err(e)))
            .collect::<Result<Vec<_>, _>>()?;

        // 6. Make Change with GenerationMeta (§7.6).
        let gen_meta = crate::session::GenerationMeta::default().encode();
        let contents = std::mem::take(&mut *rec.contents.lock());
        let mut change = libpijul::change::Change::make_change(
            &*txn.read(),
            &channel,
            actions,
            contents,
            libpijul::change::ChangeHeader {
                message: "record_generation".to_string(),
                authors: vec![],
                description: None,
                timestamp: jiff::Timestamp::now(),
            },
            gen_meta,
        )
        .map_err(|e| pijul_err(e))?;

        // 7. Save change.
        let hash = self
            .changes
            .save_change(&mut change, |_, _| Ok::<_, anyhow::Error>(()))
            .map_err(|e| pijul_err(e))?;

        // 8. Apply local change.
        libpijul::apply::apply_local_change(
            &mut *txn.write(),
            &channel,
            &change,
            &hash,
            &rec.updatables,
        )
        .map_err(|e| pijul_err(e))?;

        // Snapshot the NEW tip (after apply, before commit) so we can update
        // `working_copy_tip` once the commit succeeds.  The WC reflects the
        // applied change, so after a successful commit the WC IS consistent
        // with this tip.
        let new_tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?
            .to_bytes();

        // 9. Commit.
        txn.commit().map_err(|e| pijul_err(e))?;

        // WC is now consistent with `new_tip`.
        self.working_copy_tip.set(new_tip);

        Ok(Some(ChangeHashHex(hash_to_hex(&hash))))
    }

    // -----------------------------------------------------------------------
    // materialize
    // -----------------------------------------------------------------------

    /// Output the current channel tip into a **fresh** working copy and rebuild
    /// a [`PristineIntroTable`] from the symbol files.
    ///
    /// Using a throwaway working copy (rather than `self.working_copy`) means the
    /// result reflects the channel *exactly* — never a stale file left behind by
    /// a prior generation or an `unrecord` (libpijul's `output` writes and
    /// updates files but does not delete ones absent from the channel).
    pub fn materialize(&self) -> Result<PayloadTable, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let wc = MemWc::new();
        // §12.3: check for conflicts BEFORE parsing any F1 blob.
        let conflicts = libpijul::output::output_repository_no_pending(
            &wc,
            &self.changes,
            &txn,
            &channel,
            "",
            true,
            None,
            1,
            0,
        )
        .map_err(|e| pijul_err(e))?;

        if !conflicts.is_empty() {
            let paths: Vec<String> = conflicts.iter().map(|c| format!("{:?}", c)).collect();
            return Err(VcsError::ConflictedState { paths });
        }

        txn.commit().map_err(|e| pijul_err(e))?;

        let mut table = PayloadTable::new();
        let package_id = &self.package;

        let files = wc.list_files();
        for path in files.iter().filter(|p| is_symbol_path(p)) {
            let hex = intro_hex_of(path).unwrap_or("");
            let intro = parse_intro_hex(hex, path)?;

            let mut buf = Vec::new();
            wc.read_file(path, &mut buf).map_err(|e| pijul_err(e))?;

            // Parse F1 canonical format.
            let view =
                crate::f1::F1View::from_bytes(&buf).map_err(|e| VcsError::CorruptSymbolFile {
                    path: path.clone(),
                    reason: e.to_string(),
                })?;
            let payload = view
                .to_owned_payload()
                .map_err(|e| VcsError::CorruptSymbolFile {
                    path: path.clone(),
                    reason: e.to_string(),
                })?;

            table.insert_live(intro, payload, view.parent());

            for link in view.links() {
                let self_ref = ir::change::StableRef::new(package_id.clone(), intro);
                table.insert_link(LinkRecord {
                    a: self_ref,
                    b: link.other.clone(),
                    kind_a: link.kind_self,
                    kind_b: link.kind_other,
                });
            }
        }

        Ok(table)
    }

    // -----------------------------------------------------------------------
    // tip
    // -----------------------------------------------------------------------

    /// Return the current channel tip as an [`IrTip`].
    pub fn tip(&self) -> Result<IrTip, VcsError> {
        let txn = self.env.arc_txn_begin().map_err(|e| pijul_err(e))?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let merkle = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(IrTip {
            merkle_bytes: merkle.to_bytes(),
        })
    }

    // -----------------------------------------------------------------------
    // has_change
    // -----------------------------------------------------------------------

    /// Returns `true` if the given change hash is in the channel.
    pub fn has_change(&self, hex: &ChangeHashHex) -> Result<bool, VcsError> {
        let hash = hex_to_hash(&hex.0)?;
        let txn = self.env.arc_txn_begin().map_err(|e| pijul_err(e))?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let result = txn
            .read()
            .has_change(&channel, &hash)
            .map_err(|e| pijul_err(e))?
            .is_some();
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // log
    // -----------------------------------------------------------------------

    /// Return all change hashes in the channel, in recorded order (oldest first).
    pub fn log(&self) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.env.arc_txn_begin().map_err(|e| pijul_err(e))?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let mut result = Vec::new();
        let reader = txn.read();
        let log_iter = reader.log(&channel.read(), 0).map_err(|e| pijul_err(e))?;

        for item in log_iter {
            let (_n, (serialized_hash, _merkle)) = item.map_err(|e| pijul_err(e))?;
            let hash: Hash = serialized_hash.into();
            result.push(ChangeHashHex(hash_to_hex(&hash)));
        }

        drop(reader);
        txn.commit().map_err(|e| pijul_err(e))?;

        Ok(result)
    }

    // -----------------------------------------------------------------------
    // unrecord
    // -----------------------------------------------------------------------

    /// Remove the given change from the channel history (undo it).
    ///
    /// libpijul's `unrecord` updates `self.working_copy` in place so that it
    /// reflects the post-unrecord channel tip.  We record that new tip in
    /// `working_copy_tip` so that the next call to `record_generation` can skip
    /// the expensive whole-tree resync.
    pub fn unrecord(&self, hex: &ChangeHashHex) -> Result<(), VcsError> {
        let hash = hex_to_hash(&hex.0)?;
        let txn = self.env.arc_txn_begin().map_err(|e| pijul_err(e))?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        // `unrecord` is exposed as a `MutTxnTExt` method (the free `unrecord`
        // module is private). Arg order: changes, channel, hash, salt, wc.
        // It reverts the pristine and updates the working copy itself.
        txn.write()
            .unrecord(&self.changes, &channel, &hash, 0, &self.working_copy)
            .map_err(|e| pijul_err(e))?;

        // Snapshot the tip AFTER unrecord (but before commit) so we can mark
        // the WC as consistent with the new channel state once the commit
        // succeeds.
        let new_tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?
            .to_bytes();

        txn.commit().map_err(|e| pijul_err(e))?;

        // `self.working_copy` now reflects `new_tip`.
        self.working_copy_tip.set(new_tip);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // seal
    // -----------------------------------------------------------------------

    /// Materialize the current channel tip and seal it as a [`SealedArchive`].
    pub fn seal(&self) -> Result<SealedArchive, VcsError> {
        let index = self.materialize_index()?;
        self.seal_from_index(&index)
    }

    /// Seal a serve archive **directly from a [`MaterializedIndex`]** — the read
    /// path never materializes an owned `PristineIntroTable`. Each symbol's index
    /// bytes are handed to the archive verbatim as the (opaque) payload; the
    /// archive's lookup indices are built from borrowed [`F1View`](crate::f1::F1View)
    /// metadata. The expensive libpijul reconstruction was already paid
    /// (incrementally) to build the index; this is a borrow + cheap memcpy
    /// assembly.
    pub fn seal_from_index(&self, index: &MaterializedIndex) -> Result<SealedArchive, VcsError> {
        // Stable intro order so the parallel owned side-tables line up.
        let mut intros: Vec<IntroId> = index.intros().collect();
        intros.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        // Borrowed views over the index bytes (no owned payloads).
        let mut views: Vec<crate::f1::F1View<'_>> = Vec::with_capacity(intros.len());
        for &intro in &intros {
            let view = index
                .view(intro)
                .expect("intro came from index.intros()")
                .map_err(|e| VcsError::CorruptSymbolFile {
                    path: symbol_path(intro),
                    reason: e.to_string(),
                })?;
            views.push(view);
        }

        // F1 deliberately drops the derived `payload_hash`/`type_fingerprint`
        // frames (§6.2), so the seal reconstructs owned payloads once and
        // derives the seal metadata from them. The owned payloads outlive the
        // borrowed `SealEntry`s below.
        let payloads: Vec<OwnedEntryPayload> = views
            .iter()
            .enumerate()
            .map(|(i, v)| {
                v.to_owned_payload()
                    .map_err(|e| VcsError::CorruptSymbolFile {
                        path: symbol_path(intros[i]),
                        reason: e.to_string(),
                    })
            })
            .collect::<Result<_, _>>()?;

        let aliases: Vec<Vec<&str>> = payloads
            .iter()
            .map(|p| p.symbol.aliases.iter().map(String::as_str).collect())
            .collect();
        let links: Vec<Vec<(StableRef, KindDiscriminant, KindDiscriminant)>> = views
            .iter()
            .map(|v| {
                v.links()
                    .iter()
                    .map(|l| (l.other.clone(), l.kind_self, l.kind_other))
                    .collect()
            })
            .collect();
        let type_fps: Vec<Option<crate::vcs_types::TypeFingerprintId>> =
            payloads.iter().map(type_fingerprint_of).collect();

        let entries: Vec<SealEntry<'_>> = intros
            .iter()
            .enumerate()
            .map(|(i, &intro)| {
                let p = &payloads[i];
                SealEntry {
                    intro,
                    name: p.symbol.name.as_str(),
                    aliases: &aliases[i],
                    visibility: p.symbol.visibility as u8,
                    source_path: p.symbol.source_path.as_str(),
                    span_start: p.symbol.span_start,
                    span_end: p.symbol.span_end,
                    kind_disc: p.kind_disc,
                    flags: p.flags.0,
                    payload_hash: p.payload_hash,
                    parent: views[i].parent(),
                    type_fingerprint: type_fps[i],
                    links: &links[i],
                    payload_bytes: &index.get(intro).expect("present")[..],
                }
            })
            .collect();

        seal_from_entries(entries).map_err(VcsError::Seal)
    }

    // -----------------------------------------------------------------------
    // materialize_index (FULL)
    // -----------------------------------------------------------------------

    /// Output the whole channel into a fresh working copy and return a
    /// [`MaterializedIndex`] containing the raw bytes of every symbol file,
    /// keyed by [`IntroId`].
    ///
    /// `tip` is set to `current_state().to_bytes()` so callers can detect
    /// unchanged channels with a single 32-byte comparison.
    pub fn materialize_index(&self) -> Result<MaterializedIndex, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let index = self.index_from_channel(&txn, &channel)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(index)
    }

    /// Output an already-opened channel's whole tree into a fresh [`MemWc`] and
    /// read every `{hex}.nir` file back into a [`MaterializedIndex`].
    ///
    /// Channel-parametrized so it serves both the working channel
    /// ([`materialize_index`](Self::materialize_index)) and any frozen version
    /// channel ([`materialize_version`](Self::materialize_version)). The caller
    /// owns the transaction lifecycle (this does **not** commit). The tip Merkle
    /// is snapshotted inside the same transaction so the resulting index's `tip`
    /// is consistent with the bytes read.
    fn index_from_channel(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
    ) -> Result<MaterializedIndex, VcsError> {
        // Snapshot the tip BEFORE output so it's in the same transaction.
        let tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?
            .to_bytes();

        let wc = MemWc::new();
        libpijul::output::output_repository_no_pending(
            &wc,
            &self.changes,
            txn,
            channel,
            "",
            true,
            None,
            1,
            0,
        )
        .map_err(|e| pijul_err(e))?;

        let mut symbols = std::collections::HashMap::new();
        for path in wc.list_files().into_iter().filter(|p| is_symbol_path(p)) {
            let intro = match crate::checkout::_try_intro_from_path(&path) {
                Some(i) => i,
                None => continue,
            };
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf).map_err(|e| pijul_err(e))?;
            let arc: std::sync::Arc<[u8]> = buf.into();
            symbols.insert(intro, arc);
        }

        Ok(MaterializedIndex { tip, symbols })
    }

    // -----------------------------------------------------------------------
    // materialize_index_incremental
    // -----------------------------------------------------------------------

    /// Incrementally rebuild a [`MaterializedIndex`] from a prior one.
    ///
    /// - **Fast path:** if `current_state() == prev.tip`, returns `prev.clone()`
    ///   with no I/O.
    /// - **O(delta) path:** determine the changed symbols *without* a whole-tree
    ///   output — a cheap channel-graph file listing gives the current intro set
    ///   (so deletions fall out of a list-diff), and `reverse_log` + per-change
    ///   `touched_files` → `find_youngest_path` gives the added/modified set.
    ///   Only those symbols are output (one file each, like `checkout_symbol`);
    ///   untouched symbols keep their exact `Arc` (`Arc::ptr_eq`).
    /// - **Fallback:** if `prev.tip` is not an ancestor of the current tip
    ///   (empty/stale `prev`, diverged history after `unrecord`) or a touched
    ///   *existing* file cannot be resolved, fall back to a whole-tree output +
    ///   byte-compare. Correctness is never traded for the optimization.
    pub fn materialize_index_incremental(
        &self,
        prev: &MaterializedIndex,
    ) -> Result<MaterializedIndex, VcsError> {
        Ok(self.incremental_inner(prev)?.0)
    }

    /// Test-visible variant that also reports *how* the rebuild was done:
    /// `Some(k)` = true O(delta) that output `k` single-symbol files; `None` =
    /// fell back to a whole-tree output.
    #[cfg(test)]
    pub(crate) fn materialize_index_incremental_counted(
        &self,
        prev: &MaterializedIndex,
    ) -> Result<(MaterializedIndex, Option<usize>), VcsError> {
        self.incremental_inner(prev)
    }

    /// **Replay a reference's full IR incrementally from a prior index** — the
    /// "replay on the seal" primitive. Given a checkpoint's [`MaterializedIndex`]
    /// (`prev`), rebuild `reference`'s index in O(delta): only the symbols that
    /// changed between `prev.tip` and `reference`'s tip are re-output; every
    /// untouched symbol keeps its exact `Arc` from `prev`. Falls back to a full
    /// walk (correctly) if `prev.tip` is not an ancestor of `reference` (e.g. a
    /// checkpoint on a divergent branch).
    pub fn materialize_ref_incremental(
        &self,
        reference: &Ref,
        prev: &MaterializedIndex,
    ) -> Result<MaterializedIndex, VcsError> {
        Ok(self.materialize_ref_incremental_counted(reference, prev)?.0)
    }

    /// As [`materialize_ref_incremental`](Self::materialize_ref_incremental), also
    /// reporting how it was rebuilt: `Some(k)` = O(delta) re-output of `k`
    /// symbols; `None` = whole-tree fallback (`prev` was not a usable base).
    pub fn materialize_ref_incremental_counted(
        &self,
        reference: &Ref,
        prev: &MaterializedIndex,
    ) -> Result<(MaterializedIndex, Option<usize>), VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_ref_channel(&txn, reference)?;
        let result = self.incremental_core(&txn, &channel, prev)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(result)
    }

    fn incremental_inner(
        &self,
        prev: &MaterializedIndex,
    ) -> Result<(MaterializedIndex, Option<usize>), VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let result = self.incremental_core(&txn, &channel, prev)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(result)
    }

    /// The channel-parametrized O(delta) rebuild core (no commit — caller owns
    /// the transaction). Shared by `incremental_inner` (working channel) and
    /// `materialize_ref_incremental` (any ref's channel).
    fn incremental_core(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
        prev: &MaterializedIndex,
    ) -> Result<(MaterializedIndex, Option<usize>), VcsError> {
        let tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| pijul_err(e))?
            .to_bytes();

        // Fast path: nothing changed.
        if tip == prev.tip {
            return Ok((prev.clone(), Some(0)));
        }

        let result = match self.plan_delta(txn, channel, prev)? {
            Some((changed, current_intros)) => {
                // O(delta): reuse every untouched Arc; drop removed symbols;
                // re-output only the changed ones.
                let mut symbols = prev.symbols.clone();
                symbols.retain(|intro, _| current_intros.contains(intro));

                let mut outputs = 0usize;
                for intro in &changed {
                    let path = symbol_path(*intro);
                    let wc = MemWc::new();
                    libpijul::output::output_repository_no_pending(
                        &wc,
                        &self.changes,
                        txn,
                        channel,
                        &path,
                        true,
                        None,
                        1,
                        0,
                    )
                    .map_err(|e| pijul_err(e))?;
                    outputs += 1;

                    if wc.list_files().iter().any(|p| p == &path) {
                        let mut buf = Vec::new();
                        wc.read_file(&path, &mut buf).map_err(|e| pijul_err(e))?;
                        // Only allocate a new Arc if the bytes actually differ,
                        // so a spuriously-touched-but-identical symbol keeps its
                        // exact prior Arc.
                        match prev.symbols.get(intro) {
                            Some(a) if &a[..] == buf.as_slice() => {}
                            _ => {
                                symbols.insert(*intro, buf.into());
                            }
                        }
                    }
                    // Absent after output ⇒ deleted; already dropped by retain.
                }
                (MaterializedIndex { tip, symbols }, Some(outputs))
            }
            None => (self.incremental_whole_tree(txn, channel, prev, tip)?, None),
        };

        Ok(result)
    }

    /// Compute `(changed_intros, current_intros)` for the O(delta) path without a
    /// whole-tree output, or `None` if we cannot do so reliably (→ fall back).
    fn plan_delta(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
        prev: &MaterializedIndex,
    ) -> Result<Option<DeltaPlan>, VcsError> {
        use std::collections::HashSet;

        // 1. Cheap current-file listing from the channel graph (names only).
        let mut current_intros: HashSet<IntroId> = HashSet::new();
        {
            let txn_read = txn.read();
            let graph = channel.read();
            let it = libpijul::fs::iter_graph_children(
                &*txn_read,
                &self.changes,
                &graph,
                Position::ROOT,
            )
            .map_err(|e| pijul_err(e))?;
            for entry in it {
                let (_pos, _vertex, meta, name) = entry.map_err(|e| pijul_err(e))?;
                if meta.is_dir() {
                    continue;
                }
                if let Some(intro) = crate::checkout::_try_intro_from_path(name.as_ref()) {
                    current_intros.insert(intro);
                }
            }
        }

        // 2. Collect the changes applied since prev.tip via the reverse log.
        let mut new_changes: Vec<Hash> = Vec::new();
        let mut found_prev = false;
        {
            let txn_read = txn.read();
            let graph = channel.read();
            for entry in txn_read
                .reverse_log(&graph, None)
                .map_err(|e| pijul_err(e))?
            {
                let (_n, (ser_hash, ser_merkle)) = entry.map_err(|e| pijul_err(e))?;
                let merkle: Merkle = ser_merkle.into();
                if merkle.to_bytes() == prev.tip {
                    found_prev = true;
                    break;
                }
                let hash: Hash = ser_hash.into();
                new_changes.push(hash);
            }
        }
        if !found_prev {
            // prev.tip not an ancestor of the current tip → whole-tree fallback.
            return Ok(None);
        }

        // 3. Map touched files of those changes to CURRENT intros. A touched file
        //    that no longer resolves (Ok(None)) was deleted — handled by the
        //    list diff; a genuine resolution error aborts the plan.
        let mut changed: HashSet<IntroId> = HashSet::new();
        {
            let txn_read = txn.read();
            for hash in &new_changes {
                let touched = txn_read.touched_files(hash).map_err(|e| pijul_err(e))?;
                let Some(touched) = touched else { continue };
                for pos in touched {
                    let pos = pos.map_err(|e| pijul_err(e))?;
                    match txn_read.find_youngest_path(&self.changes, channel, pos) {
                        // The resolved path is authoritative; libpijul reports
                        // flat root files with `is_dir = true`, so we must NOT
                        // filter on it. `_try_intro_from_path` already rejects
                        // the empty (root) path and any non-`.nir` name.
                        Ok(Some((path, _is_dir))) => {
                            if let Some(intro) = crate::checkout::_try_intro_from_path(&path)
                                && current_intros.contains(&intro)
                            {
                                changed.insert(intro);
                            }
                        }
                        Ok(None) => {}
                        Err(_) => return Ok(None),
                    }
                }
            }
        }

        // Added symbols (present now, absent from `prev`) come from the list diff
        // — definitional, and independent of whether `touched_files` resolves for
        // a freshly-introduced file.
        for intro in &current_intros {
            if !prev.symbols.contains_key(intro) {
                changed.insert(*intro);
            }
        }

        Ok(Some((changed.into_iter().collect(), current_intros)))
    }

    /// Whole-tree output + byte-compare rebuild (the O(package) fallback). Does
    /// not commit — the caller owns the transaction lifecycle.
    fn incremental_whole_tree(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
        prev: &MaterializedIndex,
        tip: [u8; 32],
    ) -> Result<MaterializedIndex, VcsError> {
        let wc = MemWc::new();
        libpijul::output::output_repository_no_pending(
            &wc,
            &self.changes,
            txn,
            channel,
            "",
            true,
            None,
            1,
            0,
        )
        .map_err(|e| pijul_err(e))?;

        let mut symbols = prev.symbols.clone();
        let mut new_intros: std::collections::HashSet<IntroId> = std::collections::HashSet::new();
        for path in wc.list_files().into_iter().filter(|p| is_symbol_path(p)) {
            let intro = match crate::checkout::_try_intro_from_path(&path) {
                Some(i) => i,
                None => continue,
            };
            new_intros.insert(intro);
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf).map_err(|e| pijul_err(e))?;
            match prev.symbols.get(&intro) {
                Some(a) if &a[..] == buf.as_slice() => {}
                _ => {
                    symbols.insert(intro, buf.into());
                }
            }
        }
        symbols.retain(|intro, _| new_intros.contains(intro));
        Ok(MaterializedIndex { tip, symbols })
    }

    // -----------------------------------------------------------------------
    // checkout_symbol
    // -----------------------------------------------------------------------

    /// Output only the symbol file for `intro` and return its raw bytes,
    /// or `None` if the symbol is not present in the current channel tip.
    ///
    /// Uses `output_repository_no_pending` with `prefix = symbol_path(intro)`,
    /// which selects exactly that flat root file (verified: a bare filename
    /// prefix suffices for the flat `{hex}.nir` layout).
    pub fn checkout_symbol(
        &self,
        intro: IntroId,
    ) -> Result<Option<std::sync::Arc<[u8]>>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let out = self.checkout_symbol_from_channel(&txn, &channel, intro)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(out)
    }

    /// Output only `symbol_path(intro)` from an already-opened channel into a
    /// fresh [`MemWc`] and return its bytes, or `None` if absent from that
    /// channel's tip. Channel-parametrized so it serves both the working
    /// channel and any frozen version channel
    /// ([`checkout_symbol_at`](Self::checkout_symbol_at)). Does **not** commit.
    fn checkout_symbol_from_channel(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
        intro: IntroId,
    ) -> Result<Option<std::sync::Arc<[u8]>>, VcsError> {
        let path = symbol_path(intro);
        let wc = MemWc::new();
        libpijul::output::output_repository_no_pending(
            &wc,
            &self.changes,
            txn,
            channel,
            &path,
            true,
            None,
            1,
            0,
        )
        .map_err(|e| pijul_err(e))?;

        let files = wc.list_files();
        if files.iter().any(|p| p == &path) {
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf).map_err(|e| pijul_err(e))?;
            Ok(Some(buf.into()))
        } else {
            Ok(None)
        }
    }

    // -----------------------------------------------------------------------
    // checkout_symbols
    // -----------------------------------------------------------------------

    /// Partial multi-symbol checkout.
    ///
    /// Returns a map of `IntroId → raw bytes` for every intro in `intros` that
    /// is present in the current channel tip.  Absent intros are simply omitted.
    pub fn checkout_symbols(
        &self,
        intros: &[IntroId],
    ) -> Result<std::collections::HashMap<IntroId, std::sync::Arc<[u8]>>, VcsError> {
        let mut result = std::collections::HashMap::new();
        for &intro in intros {
            if let Some(bytes) = self.checkout_symbol(intro)? {
                result.insert(intro, bytes);
            }
        }
        Ok(result)
    }

    // =======================================================================
    // Reference model — branches, tags, versions, changes
    //
    // The store is a frozen mirror of an upstream VCS, specialized to IR:
    // references are the whole interface. Channels ARE branches (movable); tags
    // and versions are frozen channels (`tag/…`, `version/…`) that share the
    // content-addressed pristine graph — a fork is a cheap copy-on-write of the
    // channel B-tree roots, duplicating no content. Every channel-backed ref
    // serves through one fast path: a graph walk in O(state), not O(history)
    // (Zod 2.0 and Zod 5.0 cost the same modulo size). Per-symbol history and
    // ref-to-ref diffs are native graph reads. No eager per-version snapshots.
    // =======================================================================

    /// Look up an existing channel by name (does **not** create it).
    fn load_channel_ref(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        name: &str,
    ) -> Result<Option<ChannelRef<libpijul::pristine::sanakirja::MutTxn0>>, VcsError> {
        let reader = txn.read();
        reader.load_channel(name).map_err(|e| pijul_err(e))
    }

    /// Require the channel backing a reference. A bare [`Ref::Change`] is not a
    /// channel → [`VcsError::RefNotServable`]; an absent branch/tag/version →
    /// [`VcsError::RefNotFound`].
    fn require_ref_channel(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        reference: &Ref,
    ) -> Result<ChannelRef<libpijul::pristine::sanakirja::MutTxn0>, VcsError> {
        let name = reference
            .channel_name()
            .ok_or_else(|| VcsError::RefNotServable {
                reference: reference.to_string(),
            })?;
        self.load_channel_ref(txn, &name)?
            .ok_or_else(|| VcsError::RefNotFound {
                reference: reference.to_string(),
            })
    }

    /// The repository's current working branch — the channel that
    /// `record_generation` and the default `materialize`/`seal`/`symbol_history`
    /// operate on.
    pub fn current_branch(&self) -> BranchName {
        BranchName::new(self.channel_name.clone())
            .expect("the working channel name is validated as a branch at construction")
    }

    /// Resolve a channel-backed reference (branch/tag/version) to its channel
    /// name + current tip [`VersionState`]. Cheap — reads the tip, no output.
    pub fn resolve_ref(&self, reference: &Ref) -> Result<ResolvedRef, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_ref_channel(&txn, reference)?;
        let channel_name = reference.channel_name().expect("channel-backed above");
        let state = {
            let reader = txn.read();
            reader
                .current_state(&channel.read())
                .map_err(|e| pijul_err(e))?
                .to_bytes()
        };
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(ResolvedRef {
            kind: reference.kind(),
            channel_name,
            state: VersionState::from_bytes(state),
        })
    }

    // ----- lifecycle: fork + channel helpers -----

    /// Fork the channel backing `from` into a new channel, strictly (errors
    /// [`VcsError::RefAlreadyExists`] if it exists). The fork is a copy-on-write
    /// of the channel B-tree roots — content stays shared in the pristine graph.
    /// Returns the frozen tip state. Shared by branch-fork / tag / version.
    fn fork_into(
        &self,
        from: &Ref,
        new_channel: &str,
        new_label: String,
    ) -> Result<VersionState, VcsError> {
        let txn = self.arc_txn()?;
        if self.load_channel_ref(&txn, new_channel)?.is_some() {
            return Err(VcsError::RefAlreadyExists {
                reference: new_label,
            });
        }
        let src = self.require_ref_channel(&txn, from)?;
        let state = {
            let reader = txn.read();
            reader
                .current_state(&src.read())
                .map_err(|e| pijul_err(e))?
                .to_bytes()
        };
        txn.write()
            .fork(&src, new_channel)
            .map_err(|e| pijul_err(e))?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(VersionState::from_bytes(state))
    }

    /// Enumerate channels, mapping each name through `pick` (which returns `Some`
    /// for the channels it wants). Results are sorted by channel name.
    fn list_channels<T, F>(&self, pick: F) -> Result<Vec<T>, VcsError>
    where
        F: Fn(&str) -> Option<T>,
    {
        let txn = self.arc_txn()?;
        let mut names: Vec<String> = {
            let reader = txn.read();
            reader
                .channels("")
                .map_err(|e| pijul_err(e))?
                .into_iter()
                .map(|ch| ch.read().name.as_str().to_owned())
                .collect()
        };
        txn.commit().map_err(|e| pijul_err(e))?;
        names.sort();
        Ok(names.iter().filter_map(|n| pick(n)).collect())
    }

    /// Drop a channel by name; errors [`VcsError::RefNotFound`] if it did not
    /// exist.
    fn drop_channel_strict(&self, channel_name: &str, label: &str) -> Result<(), VcsError> {
        let txn = self.arc_txn()?;
        let existed = txn
            .write()
            .drop_channel(channel_name)
            .map_err(|e| pijul_err(e))?;
        txn.commit().map_err(|e| pijul_err(e))?;
        if existed {
            Ok(())
        } else {
            Err(VcsError::RefNotFound {
                reference: label.to_owned(),
            })
        }
    }

    /// Rename a channel; the target must not exist and the source must.
    fn rename_channel_strict(
        &self,
        old_channel: &str,
        new_channel: &str,
        old_label: &str,
    ) -> Result<(), VcsError> {
        let txn = self.arc_txn()?;
        if self.load_channel_ref(&txn, new_channel)?.is_some() {
            return Err(VcsError::RefAlreadyExists {
                reference: new_channel.to_owned(),
            });
        }
        let mut channel =
            self.load_channel_ref(&txn, old_channel)?
                .ok_or_else(|| VcsError::RefNotFound {
                    reference: old_label.to_owned(),
                })?;
        txn.write()
            .rename_channel(&mut channel, new_channel)
            .map_err(|e| pijul_err(e))?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(())
    }

    // ----- branches (channels) -----

    /// Create a new, **empty** branch (channel). Strict: errors if it exists.
    pub fn create_branch(&self, branch: &BranchName) -> Result<(), VcsError> {
        let txn = self.arc_txn()?;
        let name = branch.channel_name();
        if self.load_channel_ref(&txn, &name)?.is_some() {
            return Err(VcsError::RefAlreadyExists {
                reference: format!("branch:{branch}"),
            });
        }
        Self::open_or_create_channel(&txn, &name)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(())
    }

    /// Fork `from` into a new branch. Returns the new branch's tip state.
    pub fn fork_branch(&self, from: &Ref, new: &BranchName) -> Result<VersionState, VcsError> {
        self.fork_into(from, &new.channel_name(), format!("branch:{new}"))
    }

    /// Whether a branch exists.
    pub fn branch_exists(&self, branch: &BranchName) -> Result<bool, VcsError> {
        let txn = self.arc_txn()?;
        let exists = self
            .load_channel_ref(&txn, &branch.channel_name())?
            .is_some();
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(exists)
    }

    /// List every branch (channels outside the reserved tag/version namespaces).
    pub fn list_branches(&self) -> Result<Vec<BranchName>, VcsError> {
        self.list_channels(|name| {
            if name.starts_with(crate::refs::TAG_PREFIX)
                || name.starts_with(crate::refs::VERSION_PREFIX)
            {
                None
            } else {
                BranchName::new(name).ok()
            }
        })
    }

    /// Delete a branch. Refuses to delete the current working branch.
    pub fn delete_branch(&self, branch: &BranchName) -> Result<(), VcsError> {
        if branch == &self.current_branch() {
            return Err(VcsError::CurrentBranchProtected {
                branch: branch.to_string(),
            });
        }
        self.drop_channel_strict(&branch.channel_name(), &format!("branch:{branch}"))
    }

    /// Rename a branch. Refuses to rename the current working branch.
    pub fn rename_branch(&self, old: &BranchName, new: &BranchName) -> Result<(), VcsError> {
        if old == &self.current_branch() {
            return Err(VcsError::CurrentBranchProtected {
                branch: old.to_string(),
            });
        }
        self.rename_channel_strict(
            &old.channel_name(),
            &new.channel_name(),
            &format!("branch:{old}"),
        )
    }

    /// **Switch the working branch.** The branch must exist. Resets the scratch
    /// working copy so the next `record_generation` re-syncs from the target
    /// branch's tip — a fresh working copy carries no cross-branch stale files.
    pub fn switch_branch(&mut self, branch: &BranchName) -> Result<(), VcsError> {
        if !self.branch_exists(branch)? {
            return Err(VcsError::RefNotFound {
                reference: format!("branch:{branch}"),
            });
        }
        self.channel_name = branch.channel_name();
        self.working_copy = MemWc::new();
        self.working_copy_tip.set([0u8; 32]);
        Ok(())
    }

    // ----- tags (frozen channels) -----

    /// Create an immutable tag at the state of `from`. Strict.
    pub fn create_tag(&self, tag: &TagName, from: &Ref) -> Result<VersionState, VcsError> {
        self.fork_into(from, &tag.channel_name(), format!("tag:{tag}"))
    }

    /// Whether a tag exists.
    pub fn tag_exists(&self, tag: &TagName) -> Result<bool, VcsError> {
        let txn = self.arc_txn()?;
        let exists = self.load_channel_ref(&txn, &tag.channel_name())?.is_some();
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(exists)
    }

    /// List every tag.
    pub fn list_tags(&self) -> Result<Vec<TagName>, VcsError> {
        self.list_channels(|name| {
            name.strip_prefix(crate::refs::TAG_PREFIX)
                .and_then(|n| TagName::new(n).ok())
        })
    }

    /// Delete a tag.
    pub fn delete_tag(&self, tag: &TagName) -> Result<(), VcsError> {
        self.drop_channel_strict(&tag.channel_name(), &format!("tag:{tag}"))
    }

    /// The state a tag points at.
    pub fn tag_state(&self, tag: &TagName) -> Result<VersionState, VcsError> {
        self.resolve_ref(&Ref::Tag(tag.clone())).map(|r| r.state)
    }

    // ----- versions (frozen channels, semver identity) -----

    /// Create a version pointing at the state of an arbitrary `from` reference.
    pub fn create_version_from(
        &self,
        label: &VersionLabel,
        from: &Ref,
    ) -> Result<VersionState, VcsError> {
        self.fork_into(
            from,
            &label.channel_name(),
            format!("version:{}", label.as_str()),
        )
    }

    /// List every version.
    pub fn list_versions(&self) -> Result<Vec<VersionLabel>, VcsError> {
        self.list_channels(|name| {
            name.strip_prefix(crate::refs::VERSION_PREFIX)
                .and_then(|n| VersionLabel::new(n).ok())
        })
    }

    /// Delete a version.
    pub fn delete_version(&self, label: &VersionLabel) -> Result<(), VcsError> {
        self.drop_channel_strict(
            &label.channel_name(),
            &format!("version:{}", label.as_str()),
        )
    }

    // ----- ref-based serving -----

    /// **Reconstruct a reference's full IR** (branch/tag/version) as a
    /// [`MaterializedIndex`] of borrowed [`F1View`](crate::f1::F1View)s — a graph walk of that
    /// ref's channel, O(state). Age-independent.
    pub fn materialize_ref(&self, reference: &Ref) -> Result<MaterializedIndex, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_ref_channel(&txn, reference)?;
        let index = self.index_from_channel(&txn, &channel)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(index)
    }

    /// **Check out a single symbol at a reference** — a per-file graph output,
    /// O(symbol). Returns `None` if that intro is absent from the reference.
    pub fn checkout_symbol_at_ref(
        &self,
        reference: &Ref,
        intro: IntroId,
    ) -> Result<Option<std::sync::Arc<[u8]>>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_ref_channel(&txn, reference)?;
        let out = self.checkout_symbol_from_channel(&txn, &channel, intro)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(out)
    }

    /// **Per-symbol history / blame** within a reference's channel: every change
    /// that touched `intro`. Empty if the symbol is absent from that ref's tip.
    pub fn symbol_history_on(
        &self,
        reference: &Ref,
        intro: IntroId,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_ref_channel(&txn, reference)?;
        let hashes = self.symbol_history_in_channel(&txn, &channel, intro)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(hashes)
    }

    /// Walk a symbol's change history within an already-opened channel (no
    /// commit). Native: each symbol is a stable `{intro}.nir` file, so
    /// `log_for_path` walks exactly its history.
    fn symbol_history_in_channel(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
        intro: IntroId,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let target = symbol_path(intro);
        let mut hashes = Vec::new();
        let reader = txn.read();
        let graph = channel.read();

        // Resolve the symbol file's graph Position among the root's children.
        let mut position: Option<Position<libpijul::pristine::ChangeId>> = None;
        for entry in
            libpijul::fs::iter_graph_children(&*reader, &self.changes, &graph, Position::ROOT)
                .map_err(|e| pijul_err(e))?
        {
            let (pos, _vertex, meta, name) = entry.map_err(|e| pijul_err(e))?;
            if meta.is_dir() {
                continue;
            }
            if name == target {
                position = Some(pos);
                break;
            }
        }

        if let Some(pos) = position {
            for item in reader
                .log_for_path(&graph, pos, 0)
                .map_err(|e| pijul_err(e))?
            {
                let hash = item.map_err(|e| pijul_err(e))?;
                hashes.push(ChangeHashHex(hash_to_hex(&hash)));
            }
        }

        Ok(hashes)
    }

    /// **Symbol-level diff between two references**: intros added, removed, or
    /// modified going from `from` to `to`. Correct and simple — reconstructs both
    /// and compares by intro identity + payload bytes. (The change objects
    /// between the two states — [`changes_between_refs`](Self::changes_between_refs)
    /// — give the native pijul view of the same delta.)
    pub fn diff_refs(&self, from: &Ref, to: &Ref) -> Result<VersionDiff, VcsError> {
        let a = self.materialize_ref(from)?;
        let b = self.materialize_ref(to)?;
        Ok(Self::diff_indices(&a, &b))
    }

    fn diff_indices(a: &MaterializedIndex, b: &MaterializedIndex) -> VersionDiff {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut modified = Vec::new();

        for intro in b.intros() {
            match a.get(intro) {
                None => added.push(intro),
                Some(old) => {
                    let new = b.get(intro).expect("intro came from b.intros()");
                    if old[..] != new[..] {
                        modified.push(intro);
                    }
                }
            }
        }
        for intro in a.intros() {
            if b.get(intro).is_none() {
                removed.push(intro);
            }
        }

        let by_bytes = |x: &IntroId, y: &IntroId| x.as_bytes().cmp(y.as_bytes());
        added.sort_by(by_bytes);
        removed.sort_by(by_bytes);
        modified.sort_by(by_bytes);

        VersionDiff {
            added,
            removed,
            modified,
        }
    }

    /// The change hashes present in `to`'s channel but not in `from`'s — the
    /// **native pijul change delta** between two references, in recorded order.
    pub fn changes_between_refs(
        &self,
        from: &Ref,
        to: &Ref,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        let from_channel = self.require_ref_channel(&txn, from)?;
        let to_channel = self.require_ref_channel(&txn, to)?;

        let from_set: std::collections::HashSet<String> = self
            .channel_log(&txn, &from_channel)?
            .into_iter()
            .map(|h| h.0)
            .collect();
        let delta = self
            .channel_log(&txn, &to_channel)?
            .into_iter()
            .filter(|h| !from_set.contains(&h.0))
            .collect();

        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(delta)
    }

    /// **Serve a reference's sealed archive through the leaky-bucket hot cache.**
    ///
    /// - **Cache hit** → the sealed archive with **no graph walk**
    ///   ([`ServeSource::Cached`]).
    /// - **Miss, admitted** → materialize (one graph walk) + seal + store
    ///   ([`ServeSource::SealedAndStored`]).
    /// - **Miss, throttled** → materialize + seal, not stored
    ///   ([`ServeSource::SealedThrottled`]); the bucket is protecting the hot set
    ///   from a cold scan.
    ///
    /// Correctness never depends on the gate.
    pub fn serve_ref_cached(
        &self,
        reference: &Ref,
        cache: &ServeCache,
    ) -> Result<ServedArchive, VcsError> {
        let state = self.resolve_ref(reference)?.state;

        if let Some(archive) = cache.get(state) {
            return Ok(ServedArchive {
                archive,
                source: ServeSource::Cached,
            });
        }

        let index = self.materialize_ref(reference)?;
        let archive = std::sync::Arc::new(self.seal_from_index(&index)?);

        let source = if cache.try_admit() {
            cache.insert(state, std::sync::Arc::clone(&archive));
            ServeSource::SealedAndStored
        } else {
            ServeSource::SealedThrottled
        };

        Ok(ServedArchive { archive, source })
    }

    /// **Tag the current working branch's tip as a published version.**
    ///
    /// A version is a frozen `version/{label}` channel forked from the working
    /// branch — sharing the content graph, never recorded onto again, so it
    /// forever reconstructs exactly this IR. Strict: errors
    /// [`VcsError::RefAlreadyExists`] rather than overwriting.
    pub fn tag_version(&self, label: &VersionLabel) -> Result<VersionState, VcsError> {
        self.create_version_from(label, &Ref::Branch(self.current_branch()))
    }

    /// The [`VersionState`] of a tagged version — its channel tip Merkle.
    pub fn version_state(&self, label: &VersionLabel) -> Result<VersionState, VcsError> {
        self.resolve_ref(&Ref::Version(label.clone()))
            .map(|r| r.state)
    }

    /// **Reconstruct a version's full IR** — see
    /// [`materialize_ref`](Self::materialize_ref).
    pub fn materialize_version(&self, label: &VersionLabel) -> Result<MaterializedIndex, VcsError> {
        self.materialize_ref(&Ref::Version(label.clone()))
    }

    /// **Check out a single symbol at a version** — see
    /// [`checkout_symbol_at_ref`](Self::checkout_symbol_at_ref).
    pub fn checkout_symbol_at(
        &self,
        label: &VersionLabel,
        intro: IntroId,
    ) -> Result<Option<std::sync::Arc<[u8]>>, VcsError> {
        self.checkout_symbol_at_ref(&Ref::Version(label.clone()), intro)
    }

    /// **Per-symbol history / blame** over the current working branch. Tolerant:
    /// if the working branch has no channel yet, or the symbol is absent, returns
    /// an empty vector. (For an explicit reference, use
    /// [`symbol_history_on`](Self::symbol_history_on).)
    pub fn symbol_history(&self, intro: IntroId) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        // open_or_create (not require) so a fresh working branch yields empty
        // history rather than an error.
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let hashes = self.symbol_history_in_channel(&txn, &channel, intro)?;
        txn.commit().map_err(|e| pijul_err(e))?;
        Ok(hashes)
    }

    /// **Symbol-level diff between two versions** — see
    /// [`diff_refs`](Self::diff_refs).
    pub fn diff_versions(
        &self,
        from: &VersionLabel,
        to: &VersionLabel,
    ) -> Result<VersionDiff, VcsError> {
        self.diff_refs(&Ref::Version(from.clone()), &Ref::Version(to.clone()))
    }

    /// The native pijul change delta between two versions — see
    /// [`changes_between_refs`](Self::changes_between_refs).
    pub fn changes_between(
        &self,
        from: &VersionLabel,
        to: &VersionLabel,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        self.changes_between_refs(&Ref::Version(from.clone()), &Ref::Version(to.clone()))
    }

    // -----------------------------------------------------------------------
    // RecordingSession support — pub(crate) accessors + begin_recording
    // -----------------------------------------------------------------------

    /// Begin a streaming [`RecordingSession`] on this repository.
    ///
    /// Takes `&mut self` to prevent concurrent sessions at the type level.
    /// Ensures the working copy is synced to the current channel tip (same
    /// logic as `record_generation`'s step 1) and captures the set of intros
    /// currently in the working copy as the deletion baseline.
    pub fn begin_recording(&mut self) -> Result<crate::session::RecordingSession<'_, C>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        // Sync WC if stale (same fast/slow paths as record_generation).
        {
            let current_tip = txn
                .read()
                .current_state(&channel.read())
                .map_err(|e| pijul_err(e))?
                .to_bytes();

            if current_tip != self.working_copy_tip.get() {
                self.sync_output(&txn, &channel)?;
                self.working_copy_tip.set(current_tip);
            }
        }

        txn.commit().map_err(|e| pijul_err(e))?;

        // Capture the current set of symbol intros in the WC.
        let symbol_paths: Vec<String> = self
            .working_copy
            .list_files()
            .into_iter()
            .filter(|p| is_symbol_path(p))
            .collect();

        let tip_intros: std::collections::HashSet<ir::change::IntroId> = symbol_paths
            .iter()
            .filter_map(|p| {
                let hex = intro_hex_of(p)?;
                crate::checkout::_try_intro_from_path(p).or_else(|| {
                    if hex.len() != 64 {
                        return None;
                    }
                    let mut bytes = [0u8; 32];
                    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
                        let hi = (chunk[0] as char).to_digit(16)? as u8;
                        let lo = (chunk[1] as char).to_digit(16)? as u8;
                        bytes[i] = (hi << 4) | lo;
                    }
                    Some(ir::change::IntroId::from_raw(bytes))
                })
            })
            .collect();

        // Materialize tip_table for Phase B continuity matching (§5.1).
        // Read existing F1 files from the working copy, lower each OwnedEntryPayload
        // to a nudox-ir Entry (via crate::lower), and insert into the Entry-based
        // PristineIntroTable so that ir::continuity::resolve can operate on it.
        let mut tip_table = ir::apply::PristineIntroTable::new();
        for path in &symbol_paths {
            let intro = match crate::checkout::_try_intro_from_path(path) {
                Some(i) => i,
                None => continue,
            };
            let mut buf = Vec::new();
            if self.working_copy.read_file(path, &mut buf).is_err() {
                continue;
            }
            if let Ok(view) = crate::f1::F1View::from_bytes(&buf)
                && let Ok(payload) = view.to_owned_payload()
            {
                let entry = crate::lower::lower_payload(&payload);
                tip_table.insert_live(intro, entry, view.parent());
            }
        }

        Ok(crate::session::RecordingSession::new(
            self, tip_intros, tip_table,
        ))
    }

    /// `pub(crate)` arc_txn for session.rs.
    pub(crate) fn arc_txn_pub(
        &self,
    ) -> Result<libpijul::pristine::ArcTxn<libpijul::pristine::sanakirja::MutTxn0>, VcsError> {
        self.arc_txn()
    }

    /// `pub(crate)` open_or_create_channel for session.rs.
    pub(crate) fn open_or_create_channel_pub(
        txn: &libpijul::pristine::ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        name: &str,
    ) -> Result<libpijul::pristine::ChannelRef<libpijul::pristine::sanakirja::MutTxn0>, VcsError>
    {
        Self::open_or_create_channel(txn, name)
    }

    /// `pub(crate)` reference to the working copy for session.rs.
    pub(crate) fn working_copy_ref(&self) -> &libpijul::working_copy::memory::Memory {
        &self.working_copy
    }

    /// `pub(crate)` reference to the changestore for session.rs.
    pub(crate) fn changes_ref(&self) -> &C {
        &self.changes
    }

    /// `pub(crate)` reference to the channel name for session.rs.
    pub(crate) fn channel_name_ref(&self) -> &str {
        &self.channel_name
    }

    /// `pub(crate)` reference to the package id for session.rs.
    pub(crate) fn package_id(&self) -> &ir::change::PackageLineageId {
        &self.package
    }

    /// `pub(crate)` set the working copy tip (used by session after commit).
    pub(crate) fn set_working_copy_tip(&self, tip: [u8; 32]) {
        self.working_copy_tip.set(tip);
    }

    /// Reset the working copy tip to zero, forcing a full resync on the next
    /// `record_generation` or `begin_recording` call.
    ///
    /// Called by `apply_external_changes` and session abandonment.
    pub fn reset_working_copy_tip(&self) {
        self.working_copy_tip.set([0u8; 32]);
    }

    /// `pub(crate)` now() accessor for session.rs.
    pub(crate) fn now_pub(&self) -> std::time::SystemTime {
        self.now()
    }

    /// The change hashes in a channel, oldest first. Does not commit.
    fn channel_log(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let reader = txn.read();
        let mut out = Vec::new();
        for item in reader.log(&channel.read(), 0).map_err(|e| pijul_err(e))? {
            let (_n, (serialized_hash, _merkle)) = item.map_err(|e| pijul_err(e))?;
            let hash: Hash = serialized_hash.into();
            out.push(ChangeHashHex(hash_to_hex(&hash)));
        }
        Ok(out)
    }

    /// **Serve a version's sealed archive through the leaky-bucket hot cache** —
    /// see [`serve_ref_cached`](Self::serve_ref_cached).
    pub fn serve_version_cached(
        &self,
        label: &VersionLabel,
        cache: &ServeCache,
    ) -> Result<ServedArchive, VcsError> {
        self.serve_ref_cached(&Ref::Version(label.clone()), cache)
    }
}

// ---------------------------------------------------------------------------
// VersionDiff
// ---------------------------------------------------------------------------

/// The symbol-level delta between two versions (see
/// [`IrRepository::diff_versions`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionDiff {
    /// Intros present in `to` but not in `from`.
    pub added: Vec<IntroId>,
    /// Intros present in `from` but not in `to`.
    pub removed: Vec<IntroId>,
    /// Intros present in both, with different payload bytes.
    pub modified: Vec<IntroId>,
}

impl VersionDiff {
    /// True if the two versions are identical at the symbol level.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.modified.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parse IntroId from hex filename
// ---------------------------------------------------------------------------

fn parse_intro_hex(hex: &str, path: &str) -> Result<IntroId, VcsError> {
    if hex.len() != 64 {
        return Err(VcsError::CorruptSymbolFile {
            path: path.to_owned(),
            reason: format!("filename hex length {} != 64", hex.len()),
        });
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or_else(|| VcsError::CorruptSymbolFile {
                path: path.to_owned(),
                reason: format!("invalid hex at byte {i}"),
            })? as u8;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or_else(|| VcsError::CorruptSymbolFile {
                path: path.to_owned(),
                reason: format!("invalid hex at byte {i}"),
            })? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(IntroId::from_raw(bytes))
}

#[cfg(test)]
#[path = "delta_tests.rs"]
mod delta_tests;
