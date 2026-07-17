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
use libpijul::pristine::{ArcTxn, ChannelRef, Hash, Merkle, Position};
use libpijul::record::{Algorithm, Builder};
use libpijul::working_copy::memory::Memory as MemWc;
use libpijul::working_copy::{WorkingCopy, WorkingCopyRead};
use libpijul::{MutTxnTExt, TxnTExt};

use nudox_change::{ChangeSetFingerprint, IntroId, PackageLineageId};
use nudox_ir::apply::{LinkRecord, PristineIntroTable};
use nudox_ir::wire::OwnedEntryPayload;
use nudox_ir_archive::{seal_package_archive, SealedArchive};

use crate::checkout::MaterializedIndex;
use crate::error::VcsError;
use crate::serialize::{intro_hex_of, is_symbol_path, symbol_path, LinkWire};

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

        // 1. Output current channel state into working copy so we start from
        //    the correct baseline.
        self.sync_output(&txn, &channel)?;

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
                // Update: write new content to working copy.
                self.working_copy
                    .write_file(&path, libpijul::pristine::Inode::ROOT)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file open: {e}")))?
                    .write_all(&bytes)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("write_file write: {e}")))?;
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

        // 9. Commit.
        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit: {e}"))
        })?;

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
    /// After unrecord, the working copy is inconsistent — call [`materialize`]
    /// or [`record_generation`] to re-sync.
    pub fn unrecord(&self, hex: &ChangeHashHex) -> Result<(), VcsError> {
        let hash = hex_to_hash(&hex.0)?;
        let txn = self.env.arc_txn_begin().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("arc_txn_begin: {e}"))
        })?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        // `unrecord` is exposed as a `MutTxnTExt` method (the free `unrecord`
        // module is private). Arg order: changes, channel, hash, salt, wc.
        // It reverts the pristine and updates the working copy itself, so we do
        // NOT call `sync_output` afterwards (materialize re-derives the tree).
        txn.write()
            .unrecord(&self.changes, &channel, &hash, 0, &self.working_copy)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("unrecord: {e}")))?;

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (unrecord): {e}"))
        })?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // seal
    // -----------------------------------------------------------------------

    /// Materialize the current channel tip and seal it as a [`SealedArchive`].
    pub fn seal(&self) -> Result<SealedArchive, VcsError> {
        let table = self.materialize()?;
        seal_package_archive(&table).map_err(VcsError::Seal)
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
            &txn,
            &channel,
            "",
            true,
            None,
            1,
            0,
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (materialize_index): {e}")))?;

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (materialize_index): {e}")))?;

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
        let path = symbol_path(intro);
        let txn = self.arc_txn()?;
        let channel = Self::open_or_create_channel(&txn, &self.channel_name)?;

        let wc = MemWc::new();
        libpijul::output::output_repository_no_pending(
            &wc,
            &self.changes,
            &txn,
            &channel,
            &path,
            true,
            None,
            1,
            0,
        )
        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("output (checkout_symbol): {e}")))?;

        txn.commit()
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("commit (checkout_symbol): {e}")))?;

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
}
