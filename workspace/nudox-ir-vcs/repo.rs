//! [`IrRepository`] — libpijul-backed IR versioning for a single package.
//!
//! Maps the package's materialized IR onto a `symbols/{intro_hex}` working
//! tree, drives libpijul's record/apply/output/unrecord, and exposes a stable
//! API for recording generations, materializing the current tip, and sealing
//! serve archives.

use std::io::Write as IoWrite;
use std::path::Path;

use libpijul::changestore::filesystem::FileSystem as FsChanges;
use libpijul::changestore::memory::Memory as MemChanges;
use libpijul::changestore::ChangeStore;
use libpijul::pristine::sanakirja::{Pristine, SanakirjaError};
use libpijul::pristine::{ArcTxn, ChannelRef, Hash, Merkle, MutTxnT, Position, TxnT};
use libpijul::record::{Algorithm, Builder};
use libpijul::working_copy::memory::Memory as MemWc;
use libpijul::working_copy::{WorkingCopy, WorkingCopyRead};
use libpijul::{MutTxnTExt, TxnTExt};

use nudox_change::{ChangeSetFingerprint, IntroId, PackageLineageId, StableRef};
use nudox_ir::apply::{LinkRecord, PristineIntroTable};
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::wire::OwnedEntryPayload;
use nudox_ir_archive::{seal_from_entries, SealEntry, SealedArchive};

use crate::blob::SymbolView;
use crate::checkout::MaterializedIndex;
use crate::error::VcsError;
use crate::serialize::{intro_hex_of, is_symbol_path, symbol_path, LinkWire};
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
        return Err(VcsError::Pijul(anyhow::anyhow!("invalid hash hex length")));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16).ok_or_else(|| {
            VcsError::Pijul(anyhow::anyhow!("invalid hex digit"))
        })? as u8;
        let lo = (chunk[1] as char).to_digit(16).ok_or_else(|| {
            VcsError::Pijul(anyhow::anyhow!("invalid hex digit"))
        })? as u8;
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
}

impl IrRepository<MemChanges> {
    /// In-memory pristine (anon sanakirja) + memory changestore — fast,
    /// ephemeral, for tests.
    pub fn in_memory(package: PackageLineageId, channel: &str) -> Result<Self, VcsError> {
        let env = Pristine::new_anon()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("pristine init: {e}")))?;
        Ok(Self {
            env,
            changes: MemChanges::new(),
            working_copy: MemWc::new(),
            channel_name: channel.to_owned(),
            package,
            working_copy_tip: std::cell::Cell::new([0u8; 32]),
            #[cfg(test)]
            sync_output_count: std::cell::Cell::new(0),
        })
    }
}

impl IrRepository<FsChanges> {
    /// On-disk pristine (sanakirja under `root/pristine`) + **durable filesystem
    /// changestore** (under `root/changes`) — for production use. Both the
    /// pristine and every recorded change persist across process restarts; the
    /// working copy is ephemeral scratch, re-derived by `materialize`.
    pub fn open(root: &Path, package: PackageLineageId, channel: &str) -> Result<Self, VcsError> {
        std::fs::create_dir_all(root.join("changes"))
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("create changes dir: {e}")))?;
        let env = Pristine::new(root.join("pristine"))
            .map_err(|e: SanakirjaError| VcsError::Pijul(anyhow::anyhow!("pristine open: {e}")))?;
        let changes = FsChanges::from_root(root.join("changes"), CHANGESTORE_CACHE);
        Ok(Self {
            env,
            changes,
            working_copy: MemWc::new(),
            channel_name: channel.to_owned(),
            package,
            working_copy_tip: std::cell::Cell::new([0u8; 32]),
            #[cfg(test)]
            sync_output_count: std::cell::Cell::new(0),
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

    fn arc_txn(&self) -> Result<ArcTxn<libpijul::pristine::sanakirja::MutTxn0>, VcsError> {
        self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })
    }

    fn open_or_create_channel<T>(
        txn: &ArcTxn<T>,
        name: &str,
    ) -> Result<ChannelRef<T>, VcsError>
    where
        T: libpijul::pristine::MutTxnT + Send + Sync + 'static,
    {
        txn.write().open_or_create_channel(name).map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("open_or_create_channel: {e}"))
        })
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
            + libpijul::pristine::TreeMutTxnT<TreeError = <T as libpijul::pristine::GraphTxnT>::GraphError>
            + Send
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
            "",    // prefix: whole tree
            true,  // output_name_conflicts
            None,  // if_modified_since
            1,     // n_workers
            0,     // salt
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("{e}")))?;
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
    pub fn record_generation(&self, ir: &PristineIntroTable) -> Result<Option<ChangeHashHex>, VcsError> {
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
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state: {e}")))?
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
        //    For each live intro we build a SymbolFile. Links are stored on
        //    the canonical-owner side: the endpoint whose IntroId bytes are
        //    smallest (or, for cross-package links, the local endpoint).
        let package_id = &self.package;

        // Build a map: intro → (payload, parent, Vec<LinkWire>)
        let mut desired: std::collections::HashMap<IntroId, (OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>)> = std::collections::HashMap::new();
        for (intro, payload) in ir.live_entries() {
            let parent = ir.parent_of(intro);
            desired.entry(intro).or_insert_with(|| (payload.clone(), parent, Vec::new()));
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
                if a_intro.as_bytes() <= b_intro.as_bytes() { a_intro } else { b_intro }
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
                entry.2.push(LinkWire { other, kind_self, kind_other });
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
        let desired_files: std::collections::HashSet<String> = desired
            .keys()
            .map(|intro| symbol_path(*intro))
            .collect();

        // Files to remove (were live, now gone):
        for path in current_files.difference(&desired_files) {
            self.working_copy
                .remove_path(path, false)
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("remove_path: {e}")))?;
            txn.write().remove_file(path).map_err(|e| {
                VcsError::Pijul(anyhow::anyhow!("remove_file: {e}"))
            })?;
        }

        // Files to add or update:
        for (intro, (payload, parent, links)) in &desired {
            let path = symbol_path(*intro);
            // Textual, line-oriented per-symbol content so libpijul diffs at the
            // field/param level.
            let bytes = crate::blob::serialize_symbol_blob(payload, *parent, links);

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
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file (compare) {path}: {e}")))?;
                if existing != bytes {
                    self.working_copy
                        .write_file(&path, libpijul::pristine::Inode::ROOT)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file open: {e}")))?
                        .write_all(&bytes)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file write: {e}")))?;
                }
                // else: identical content — leave the file and its mtime alone.
            } else {
                // Add: put content into working copy and track in txn.
                self.working_copy.add_file(&path, bytes);
                txn.write().add_file(&path, 0).map_err(|e| {
                    VcsError::Pijul(anyhow::anyhow!("add_file: {e}"))
                })?;
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
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("record: {e}")))?;

        let rec = builder.finish();

        // If there are no actions, nothing changed.
        if rec.actions.is_empty() {
            txn.commit().map_err(|e| {
                VcsError::Pijul(anyhow::anyhow!("commit (no-op): {e}"))
            })?;
            return Ok(None);
        }

        // 5. Globalize actions.
        let actions = rec
            .actions
            .into_iter()
            .map(|a| {
                a.globalize(&*txn.read())
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("globalize: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // 6. Make Change.
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
            Vec::new(),
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("make_change: {e:?}")))?;

        // 7. Save change.
        let hash = self
            .changes
            .save_change(&mut change, |_, _| Ok::<_, anyhow::Error>(()))
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("save_change: {e}")))?;

        // 8. Apply local change.
        libpijul::apply::apply_local_change(
            &mut *txn.write(),
            &channel,
            &change,
            &hash,
            &rec.updatables,
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("apply_local_change: {e}")))?;

        // Snapshot the NEW tip (after apply, before commit) so we can update
        // `working_copy_tip` once the commit succeeds.  The WC reflects the
        // applied change, so after a successful commit the WC IS consistent
        // with this tip.
        let new_tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state (post-apply): {e}")))?
            .to_bytes();

        // 9. Commit.
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit: {e}"))
        })?;

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
    pub fn materialize(&self) -> Result<PristineIntroTable, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let wc = MemWc::new();
        libpijul::output::output_repository_no_pending(
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
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (materialize): {e}")))?;

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (materialize): {e}"))
        })?;

        let mut table = PristineIntroTable::new();
        let package_id = &self.package;

        let files = wc.list_files();
        for path in files.iter().filter(|p| is_symbol_path(p)) {
            let hex = intro_hex_of(path).unwrap_or("");
            // Parse IntroId from hex filename.
            let intro = parse_intro_hex(hex, path)?;

            let mut buf = Vec::new();
            wc.read_file(path, &mut buf)
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file {path}: {e}")))?;

            // Borrow-based scan of the textual blob (no content allocation on
            // the read path).
            let view = crate::blob::SymbolView::from_bytes(&buf).map_err(|e| {
                VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
            })?;
            let payload = view.to_owned_payload().map_err(|e| {
                VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
            })?;

            table.insert_live(intro, payload, view.parent());

            // Reconstruct links from the canonical-owner file: this intro owns
            // the link; the other endpoint is `link.other`.
            for link in view.links() {
                let self_ref = nudox_change::StableRef::new(package_id.clone(), intro);
                table.insert_link(LinkRecord {
                    a: self_ref,
                    b: link.to_stable_ref(),
                    kind_a: link.kind_self,
                    kind_b: link.kind_other,
                    added_by: nudox_change::ChangeId::from_raw([0u8; 32]), // libpijul owns provenance
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
        let txn = self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let merkle = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state: {e}")))?;
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (tip): {e}"))
        })?;
        Ok(IrTip { merkle_bytes: merkle.to_bytes() })
    }

    // -----------------------------------------------------------------------
    // has_change
    // -----------------------------------------------------------------------

    /// Returns `true` if the given change hash is in the channel.
    pub fn has_change(&self, hex: &ChangeHashHex) -> Result<bool, VcsError> {
        let hash = hex_to_hash(&hex.0)?;
        let txn = self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let result = txn
            .read()
            .has_change(&channel, &hash)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("has_change: {e}")))?
            .is_some();
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (has_change): {e}"))
        })?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // log
    // -----------------------------------------------------------------------

    /// Return all change hashes in the channel, in recorded order (oldest first).
    pub fn log(&self) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let mut result = Vec::new();
        let reader = txn.read();
        let log_iter = reader
            .log(&channel.read(), 0)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("log: {e}")))?;

        for item in log_iter {
            let (_n, (serialized_hash, _merkle)) = item
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("log iter: {e}")))?;
            let hash: Hash = serialized_hash.into();
            result.push(ChangeHashHex(hash_to_hex(&hash)));
        }

        drop(reader);
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (log): {e}"))
        })?;

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
        let txn = self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        // `unrecord` is exposed as a `MutTxnTExt` method (the free `unrecord`
        // module is private). Arg order: changes, channel, hash, salt, wc.
        // It reverts the pristine and updates the working copy itself.
        txn.write()
            .unrecord(&self.changes, &channel, &hash, 0, &self.working_copy)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("unrecord: {e}")))?;

        // Snapshot the tip AFTER unrecord (but before commit) so we can mark
        // the WC as consistent with the new channel state once the commit
        // succeeds.
        let new_tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state (post-unrecord): {e}")))?
            .to_bytes();

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (unrecord): {e}"))
        })?;

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
    /// archive's lookup indices are built from borrowed [`SymbolView`] metadata.
    /// The expensive libpijul reconstruction was already paid (incrementally) to
    /// build the index; this is a borrow + cheap memcpy assembly.
    pub fn seal_from_index(&self, index: &MaterializedIndex) -> Result<SealedArchive, VcsError> {
        // Stable intro order so the parallel owned side-tables line up.
        let mut intros: Vec<IntroId> = index.intros().collect();
        intros.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        // Borrowed views over the index bytes (no owned payloads).
        let mut views: Vec<SymbolView<'_>> = Vec::with_capacity(intros.len());
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

        // Owned side-tables that must outlive the `SealEntry` borrows: alias
        // pointer lists and the (owned `StableRef`) link tuples. Content stays
        // borrowed from the index bytes; only these small spines allocate.
        let aliases: Vec<Vec<&str>> = views.iter().map(|v| v.aliases().collect()).collect();
        let links: Vec<Vec<(StableRef, KindDiscriminant, KindDiscriminant)>> = views
            .iter()
            .map(|v| v.links().map(|l| (l.to_stable_ref(), l.kind_self, l.kind_other)).collect())
            .collect();

        let entries: Vec<SealEntry<'_>> = intros
            .iter()
            .enumerate()
            .map(|(i, &intro)| {
                let v = &views[i];
                let (span_start, span_end) = v.span();
                SealEntry {
                    intro,
                    name: v.name(),
                    aliases: &aliases[i],
                    visibility: v.visibility() as u8,
                    source_path: v.source_path(),
                    span_start,
                    span_end,
                    kind_disc: v.kind_disc(),
                    flags: v.flags().0,
                    payload_hash: v.payload_hash(),
                    parent: v.parent(),
                    type_fingerprint: v.type_fingerprint(),
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
        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (materialize_index): {e}")))?;
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
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state: {e}")))?
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
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (index_from_channel): {e}")))?;

        let mut symbols = std::collections::HashMap::new();
        for path in wc.list_files().into_iter().filter(|p| is_symbol_path(p)) {
            let intro = match crate::checkout::_try_intro_from_path(&path) {
                Some(i) => i,
                None => continue,
            };
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf)
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file {path}: {e}")))?;
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

    fn incremental_inner(
        &self,
        prev: &MaterializedIndex,
    ) -> Result<(MaterializedIndex, Option<usize>), VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let tip = txn
            .read()
            .current_state(&channel.read())
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state: {e}")))?
            .to_bytes();

        // Fast path: nothing changed.
        if tip == prev.tip {
            txn.commit()
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (incremental noop): {e}")))?;
            return Ok((prev.clone(), Some(0)));
        }

        let (index, count) = match self.plan_delta(&txn, &channel, prev)? {
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
                        &wc, &self.changes, &txn, &channel, &path, true, None, 1, 0,
                    )
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (delta {path}): {e}")))?;
                    outputs += 1;

                    if wc.list_files().iter().any(|p| p == &path) {
                        let mut buf = Vec::new();
                        wc.read_file(&path, &mut buf)
                            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file {path}: {e}")))?;
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
            None => (self.incremental_whole_tree(&txn, &channel, prev, tip)?, None),
        };

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (incremental): {e}")))?;
        Ok((index, count))
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
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("iter_graph_children: {e}")))?;
            for entry in it {
                let (_pos, _vertex, meta, name) = entry
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("graph child: {e}")))?;
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
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("reverse_log: {e}")))?
            {
                let (_n, (ser_hash, ser_merkle)) = entry
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("reverse_log entry: {e}")))?;
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
                let touched = txn_read
                    .touched_files(hash)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touched_files: {e}")))?;
                let Some(touched) = touched else { continue };
                for pos in touched {
                    let pos = pos
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touched pos: {e}")))?;
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
            &wc, &self.changes, txn, channel, "", true, None, 1, 0,
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (whole-tree): {e}")))?;

        let mut symbols = prev.symbols.clone();
        let mut new_intros: std::collections::HashSet<IntroId> = std::collections::HashSet::new();
        for path in wc.list_files().into_iter().filter(|p| is_symbol_path(p)) {
            let intro = match crate::checkout::_try_intro_from_path(&path) {
                Some(i) => i,
                None => continue,
            };
            new_intros.insert(intro);
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf)
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file {path}: {e}")))?;
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
        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (checkout_symbol): {e}")))?;
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
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (checkout_symbol): {e}")))?;

        let files = wc.list_files();
        if files.iter().any(|p| p == &path) {
            let mut buf = Vec::new();
            wc.read_file(&path, &mut buf)
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file {path}: {e}")))?;
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
    // Historical replay
    //
    // Efficient replay of any published version, without eager per-version
    // snapshots. Storage is the content-shared pijul graph (unchanged symbols
    // stored once across all versions). A version is a *frozen channel* forked
    // from the working channel at publish time; serving it is a graph walk in
    // O(state), not O(history) — Zod 2.0 and Zod 5.0 cost the same modulo their
    // sizes. Per-symbol history and version diffs are native graph reads.
    // =======================================================================

    /// Look up an existing channel by name (does **not** create it).
    fn load_channel_ref(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        name: &str,
    ) -> Result<Option<ChannelRef<libpijul::pristine::sanakirja::MutTxn0>>, VcsError> {
        let reader = txn.read();
        reader
            .load_channel(name)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("load_channel {name}: {e}")))
    }

    /// Require an existing version channel, mapping absence to
    /// [`VcsError::VersionNotFound`].
    fn require_version_channel(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        label: &VersionLabel,
    ) -> Result<ChannelRef<libpijul::pristine::sanakirja::MutTxn0>, VcsError> {
        self.load_channel_ref(txn, &label.channel_name())?
            .ok_or_else(|| VcsError::VersionNotFound { label: label.as_str().to_owned() })
    }

    /// **Tag the current working-channel tip as a published version.**
    ///
    /// Forks the working channel into a frozen channel `version/{label}` that
    /// shares the content-addressed pristine graph (a cheap copy-on-write of the
    /// channel's B-tree roots — no content is duplicated). The version channel is
    /// never recorded onto again, so it forever reconstructs exactly this IR.
    ///
    /// Returns the [`VersionState`] (the tip Merkle) identifying the version.
    /// Errors with [`VcsError::VersionAlreadyTagged`] if the version already
    /// exists — tagging is strict, never a silent overwrite.
    pub fn tag_version(&self, label: &VersionLabel) -> Result<VersionState, VcsError> {
        let txn = self.arc_txn()?;
        let version_channel = label.channel_name();

        if self.load_channel_ref(&txn, &version_channel)?.is_some() {
            return Err(VcsError::VersionAlreadyTagged { label: label.as_str().to_owned() });
        }

        let main = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let state = {
            let reader = txn.read();
            reader
                .current_state(&main.read())
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state (tag): {e}")))?
                .to_bytes()
        };

        // Fork the working channel at its current tip into the version channel.
        txn.write()
            .fork(&main, &version_channel)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("fork {version_channel}: {e}")))?;

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (tag_version): {e}")))?;

        Ok(VersionState::from_bytes(state))
    }

    /// The [`VersionState`] of a tagged version — its channel tip Merkle. Cheap:
    /// reads the tip, no output. Errors [`VcsError::VersionNotFound`] if untagged.
    pub fn version_state(&self, label: &VersionLabel) -> Result<VersionState, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_version_channel(&txn, label)?;
        let state = {
            let reader = txn.read();
            reader
                .current_state(&channel.read())
                .map_err(|e| VcsError::Pijul(anyhow::anyhow!("current_state (version): {e}")))?
                .to_bytes()
        };
        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (version_state): {e}")))?;
        Ok(VersionState::from_bytes(state))
    }

    /// **Reconstruct a version's full IR** as a [`MaterializedIndex`] of borrowed
    /// [`SymbolView`]s — a graph walk of that version's frozen channel, O(state).
    /// Age-independent: an old version costs no more than a new one of the same
    /// size.
    pub fn materialize_version(&self, label: &VersionLabel) -> Result<MaterializedIndex, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_version_channel(&txn, label)?;
        let index = self.index_from_channel(&txn, &channel)?;
        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (materialize_version): {e}")))?;
        Ok(index)
    }

    /// **Check out a single symbol at a version** — a per-file graph output,
    /// O(symbol). Returns `None` if that intro is absent from the version.
    pub fn checkout_symbol_at(
        &self,
        label: &VersionLabel,
        intro: IntroId,
    ) -> Result<Option<std::sync::Arc<[u8]>>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = self.require_version_channel(&txn, label)?;
        let out = self.checkout_symbol_from_channel(&txn, &channel, intro)?;
        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (checkout_symbol_at): {e}")))?;
        Ok(out)
    }

    /// **Per-symbol history / blame** over the working channel's linear history:
    /// every change that touched symbol `intro`, newest first.
    ///
    /// Native because each symbol is a stable `{intro}.nir` file (IntroId is
    /// stable across versions), so libpijul's `log_for_path` walks exactly that
    /// file's change history. Returns an empty vector if the symbol is absent
    /// from the current tip.
    pub fn symbol_history(&self, intro: IntroId) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;
        let target = symbol_path(intro);

        let mut hashes = Vec::new();
        {
            let reader = txn.read();
            let graph = channel.read();

            // Resolve the symbol file's graph Position among the root's children.
            let mut position: Option<Position<libpijul::pristine::ChangeId>> = None;
            for entry in libpijul::fs::iter_graph_children(
                &*reader,
                &self.changes,
                &graph,
                Position::ROOT,
            )
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("iter_graph_children: {e}")))?
            {
                let (pos, _vertex, meta, name) = entry
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("graph child: {e}")))?;
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
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("log_for_path: {e}")))?
                {
                    let hash =
                        item.map_err(|e| VcsError::Pijul(anyhow::anyhow!("log_for_path entry: {e}")))?;
                    hashes.push(ChangeHashHex(hash_to_hex(&hash)));
                }
            }
        }

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (symbol_history): {e}")))?;
        Ok(hashes)
    }

    /// **Symbol-level diff between two versions**: which intros were added,
    /// removed, or modified going from `from` to `to`.
    ///
    /// Correct and simple: reconstructs both version indices and compares by
    /// intro identity + payload bytes. (The change objects between the two
    /// channel states — see [`changes_between`](Self::changes_between) — give the
    /// native pijul view of the same delta.)
    pub fn diff_versions(
        &self,
        from: &VersionLabel,
        to: &VersionLabel,
    ) -> Result<VersionDiff, VcsError> {
        let a = self.materialize_version(from)?;
        let b = self.materialize_version(to)?;

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

        // Deterministic ordering by intro bytes.
        let by_bytes = |x: &IntroId, y: &IntroId| x.as_bytes().cmp(y.as_bytes());
        added.sort_by(by_bytes);
        removed.sort_by(by_bytes);
        modified.sort_by(by_bytes);

        Ok(VersionDiff { added, removed, modified })
    }

    /// The change hashes present in `to`'s channel but not in `from`'s — the
    /// **native pijul change delta** between two versions, in recorded order.
    pub fn changes_between(
        &self,
        from: &VersionLabel,
        to: &VersionLabel,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let txn = self.arc_txn()?;
        let from_channel = self.require_version_channel(&txn, from)?;
        let to_channel = self.require_version_channel(&txn, to)?;

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

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (changes_between): {e}")))?;
        Ok(delta)
    }

    /// The change hashes in a channel, oldest first. Does not commit.
    fn channel_log(
        &self,
        txn: &ArcTxn<libpijul::pristine::sanakirja::MutTxn0>,
        channel: &ChannelRef<libpijul::pristine::sanakirja::MutTxn0>,
    ) -> Result<Vec<ChangeHashHex>, VcsError> {
        let reader = txn.read();
        let mut out = Vec::new();
        for item in reader
            .log(&channel.read(), 0)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("channel_log: {e}")))?
        {
            let (_n, (serialized_hash, _merkle)) =
                item.map_err(|e| VcsError::Pijul(anyhow::anyhow!("channel_log entry: {e}")))?;
            let hash: Hash = serialized_hash.into();
            out.push(ChangeHashHex(hash_to_hex(&hash)));
        }
        Ok(out)
    }

    /// **Serve a version's sealed archive through the leaky-bucket hot cache.**
    ///
    /// - **Cache hit** → returns the sealed archive with **no graph walk**
    ///   ([`ServeSource::Cached`]).
    /// - **Miss, admitted** → materializes the version (one graph walk), seals
    ///   it, and stores it so the next read is a hit
    ///   ([`ServeSource::SealedAndStored`]).
    /// - **Miss, throttled** → materializes and seals but does **not** store it
    ///   ([`ServeSource::SealedThrottled`]); the leaky bucket is protecting the
    ///   hot set from a cold-version scan.
    ///
    /// Correctness never depends on the gate — every call returns the requested
    /// archive.
    pub fn serve_version_cached(
        &self,
        label: &VersionLabel,
        cache: &ServeCache,
    ) -> Result<ServedArchive, VcsError> {
        let state = self.version_state(label)?;

        if let Some(archive) = cache.get(state) {
            return Ok(ServedArchive { archive, source: ServeSource::Cached });
        }

        let index = self.materialize_version(label)?;
        let archive = std::sync::Arc::new(self.seal_from_index(&index)?);

        let source = if cache.try_admit() {
            cache.insert(state, std::sync::Arc::clone(&archive));
            ServeSource::SealedAndStored
        } else {
            ServeSource::SealedThrottled
        };

        Ok(ServedArchive { archive, source })
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
        let hi = (chunk[0] as char).to_digit(16).ok_or_else(|| VcsError::CorruptSymbolFile {
            path: path.to_owned(),
            reason: format!("invalid hex at byte {i}"),
        })? as u8;
        let lo = (chunk[1] as char).to_digit(16).ok_or_else(|| VcsError::CorruptSymbolFile {
            path: path.to_owned(),
            reason: format!("invalid hex at byte {i}"),
        })? as u8;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(IntroId::from_raw(bytes))
}

#[cfg(test)]
mod delta_tests {
    use super::*;
    use nudox_change::{EcosystemId, PackageName};
    use nudox_ir::apply::PristineIntroTable;
    use nudox_ir::kind::KindDiscriminant;
    use nudox_ir::wire::{EntryPayloadFlags, FunctionWire, KindWire, SymbolWire};

    fn pkg() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn func(name: &str) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.to_owned(),
            visibility: 0,
            documentation: None,
            source_path: "src/lib.rs".to_owned(),
            span_start: 0,
            span_end: name.len() as u32,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        };
        OwnedEntryPayload::sealed(
            sym,
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]) }),
            EntryPayloadFlags::default(),
        )
    }

    fn table(entries: &[(u8, &str)]) -> PristineIntroTable {
        let mut t = PristineIntroTable::new();
        for (n, name) in entries {
            t.insert_live(intro(*n), func(name), None);
        }
        t
    }

    /// Changing ONE symbol in a 4-symbol package must re-output exactly ONE file
    /// (true O(delta)), reuse the exact `Arc` for the other three, and match a
    /// fresh full materialize.
    #[test]
    fn incremental_output_is_o_delta() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        // Gen A: 4 symbols.
        repo.record_generation(&table(&[(1, "a"), (2, "b"), (3, "c"), (4, "d")]))
            .unwrap()
            .expect("gen A recorded");
        let prev = repo.materialize_index().unwrap();
        assert_eq!(prev.len(), 4);

        // Gen B: change exactly one symbol (intro 2's payload).
        repo.record_generation(&table(&[(1, "a"), (2, "b_CHANGED"), (3, "c"), (4, "d")]))
            .unwrap()
            .expect("gen B recorded");

        let (idx, count) = repo.materialize_index_incremental_counted(&prev).unwrap();

        // O(delta): the delta path ran and output EXACTLY ONE file (not 4).
        assert_eq!(count, Some(1), "must re-output only the one changed symbol");

        // Result equals a fresh full materialize.
        let full = repo.materialize_index().unwrap();
        assert_eq!(idx.symbols.len(), full.symbols.len());
        for (k, v) in &full.symbols {
            assert_eq!(&idx.get(*k).unwrap()[..], &v[..], "symbol {} bytes must match full", k.to_hex());
        }

        // Untouched symbols keep the EXACT same Arc (never re-output); the
        // changed one gets a fresh Arc.
        for n in [1u8, 3, 4] {
            assert!(
                std::sync::Arc::ptr_eq(prev.get(intro(n)).unwrap(), idx.get(intro(n)).unwrap()),
                "untouched symbol {n} must keep its Arc"
            );
        }
        assert!(
            !std::sync::Arc::ptr_eq(prev.get(intro(2)).unwrap(), idx.get(intro(2)).unwrap()),
            "changed symbol must have a fresh Arc"
        );
    }

    /// Deletion + addition in one step: `checkout`-free incremental drops the
    /// removed symbol and adds the new one, output count = added ∪ modified.
    #[test]
    fn incremental_add_and_remove() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();
        repo.record_generation(&table(&[(1, "a"), (2, "b"), (3, "c")])).unwrap().unwrap();
        let prev = repo.materialize_index().unwrap();

        // Remove symbol 2, add symbol 4.
        repo.record_generation(&table(&[(1, "a"), (3, "c"), (4, "d")])).unwrap().unwrap();
        let (idx, count) = repo.materialize_index_incremental_counted(&prev).unwrap();

        assert_eq!(count, Some(1), "only the added symbol needs output; removal needs none");
        let mut intros: Vec<u8> = idx.intros().map(|i| i.as_bytes()[0]).collect();
        intros.sort_unstable();
        assert_eq!(intros, vec![1, 3, 4]);
        // 1 and 3 untouched → same Arc.
        assert!(std::sync::Arc::ptr_eq(prev.get(intro(1)).unwrap(), idx.get(intro(1)).unwrap()));
        assert!(std::sync::Arc::ptr_eq(prev.get(intro(3)).unwrap(), idx.get(intro(3)).unwrap()));
    }

    // -----------------------------------------------------------------------
    // O(delta) write-path tests
    // -----------------------------------------------------------------------

    /// **Core O(delta) write invariant.**
    ///
    /// Build a 3-symbol package (N ≥ 3), record gen A, then change exactly ONE
    /// symbol and record gen B.  The second `record_generation` call MUST NOT
    /// perform a whole-tree `output_repository_no_pending` — the WC was already
    /// current after gen A, so `working_copy_tip` matches the channel tip and
    /// the baseline-resync is skipped.
    ///
    /// Verification via `sync_output_count()`:
    /// - Gen A may or may not call sync depending on whether the empty channel's
    ///   tip happens to equal `[0u8; 32]`.  We capture the count after gen A.
    /// - Gen B must NOT increase the count (working_copy_tip is current).
    /// - Gen C (another change) must also NOT increase the count.
    ///
    /// We also verify correctness: `materialize` after gen C returns all three
    /// symbols with updated names.
    #[test]
    fn record_generation_write_path_is_o_delta() {
        let repo = IrRepository::in_memory(pkg(), "main").unwrap();

        let gen_a = table(&[(1, "alpha"), (2, "beta"), (3, "gamma")]);
        repo.record_generation(&gen_a)
            .unwrap()
            .expect("gen A must produce a change");

        // Capture count after gen A: the first recording on a fresh repo may or
        // may not resync (depends on whether the empty channel tip == [0;32]).
        // What matters is that subsequent recordings do NOT resync.
        let count_after_a = repo.sync_output_count();

        // Gen B: only symbol 2 changes.  working_copy_tip now matches the gen A
        // tip, so no resync should occur.
        let gen_b = table(&[(1, "alpha"), (2, "beta_v2"), (3, "gamma")]);
        repo.record_generation(&gen_b)
            .unwrap()
            .expect("gen B must produce a change");

        assert_eq!(
            repo.sync_output_count(),
            count_after_a,
            "gen B must NOT trigger a whole-tree sync (working_copy_tip was current after gen A)"
        );

        // Gen C: change a different symbol.  Still no resync expected.
        let gen_c = table(&[(1, "alpha"), (2, "beta_v2"), (3, "gamma_v2")]);
        repo.record_generation(&gen_c)
            .unwrap()
            .expect("gen C must produce a change");

        assert_eq!(
            repo.sync_output_count(),
            count_after_a,
            "gen C must NOT trigger a whole-tree sync either"
        );

        // Correctness: materialize should reflect gen C.
        let mat = repo.materialize().unwrap();
        assert_eq!(mat.live_entries().count(), 3, "all three symbols survive");
        assert!(mat.is_live(intro(1)));
        assert!(mat.is_live(intro(2)));
        assert!(mat.is_live(intro(3)));
        // Symbol 2 must be updated (from gen B).
        let p2 = mat.get(intro(2)).and_then(|e| e.as_live()).unwrap();
        assert_eq!(p2.symbol.name, "beta_v2", "symbol 2 must have updated name from gen B");
        // Symbol 3 must be updated (from gen C).
        let p3 = mat.get(intro(3)).and_then(|e| e.as_live()).unwrap();
        assert_eq!(p3.symbol.name, "gamma_v2", "symbol 3 must have updated name from gen C");
    }

    /// **Stale WC path (durable re-open).**
    ///
    /// Record a generation through an on-disk `IrRepository`, drop it, then
    /// re-open the same root in a new `IrRepository` (fresh empty WC, but the
    /// pristine + changestore persist).  The first `record_generation` on the
    /// re-opened repository must perform one whole-tree resync (WC is empty but
    /// the channel is non-empty), then leave `working_copy_tip` current so that
    /// the SECOND recording skips the sync.
    ///
    /// Correctness: the final `materialize` must reflect all recorded symbols.
    #[test]
    fn record_generation_stale_wc_resyncs_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let pkg = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("testcrate"));

        // --- first open: record two generations then drop. ---
        {
            use libpijul::changestore::filesystem::FileSystem as FsChanges;

            let repo: IrRepository<FsChanges> =
                IrRepository::open(root, pkg.clone(), "main").unwrap();
            repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b"), (3, "c")]))
                .unwrap()
                .expect("gen A");
        }

        // --- second open: fresh IrRepository with empty WC. ---
        {
            use libpijul::changestore::filesystem::FileSystem as FsChanges;

            let repo: IrRepository<FsChanges> =
                IrRepository::open(root, pkg.clone(), "main").unwrap();

            // WC is empty, channel tip is non-empty → stale.
            // The first record_generation must do ONE whole-tree sync, then update
            // working_copy_tip so subsequent recordings can skip it.
            repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b_new"), (3, "c")]))
                .unwrap()
                .expect("gen B on re-open");

            assert_eq!(
                repo.sync_output_count(),
                1,
                "re-opened repo must resync once (stale WC path)"
            );

            // A second recording must NOT resync (WC is now current).
            repo.record_generation(&table_pkg(&pkg, &[(1, "a"), (2, "b_new"), (3, "c_new")]))
                .unwrap()
                .expect("gen C");

            assert_eq!(
                repo.sync_output_count(),
                1,
                "second recording after re-open must NOT resync again"
            );

            // Correctness: latest materialize has all three symbols with updated names.
            let mat = repo.materialize().unwrap();
            assert_eq!(mat.live_entries().count(), 3);
            let p3 = mat.get(intro(3)).and_then(|e| e.as_live()).unwrap();
            assert_eq!(p3.symbol.name, "c_new");
        }
    }

    /// Helper: build a `PristineIntroTable` tied to `pkg` (for durable-repo tests).
    fn table_pkg(
        _pkg: &PackageLineageId,
        entries: &[(u8, &str)],
    ) -> nudox_ir::apply::PristineIntroTable {
        table(entries)
    }
}
