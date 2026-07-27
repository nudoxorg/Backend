//! [`RecordingSession`] — two-phase incremental IR recording session.
//!
//! # Phase A: streaming stage
//!
//! The caller calls `.stage(batch)`, `.stage_links(from, links)`, and
//! optionally `.checkpoint(msg)` any number of times.  Files are written to
//! the working copy using wire-ids; no continuity matching occurs yet.
//!
//! # Phase B: finish
//!
//! `.finish()` runs the continuity matcher (§5), performs the σ substitution
//! cascade (K-Subst-Cascade), writes the final WC state, records a change
//! with [`GenerationMeta`], and returns [`FinishReport`].

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write as IoWrite;

use libpijul::changestore::ChangeStore;
use libpijul::pristine::ChannelTxnT;
use libpijul::record::{Algorithm, Builder};
use libpijul::working_copy::{WorkingCopy, WorkingCopyRead};
use libpijul::{MutTxnTExt, TxnTExt};

use ir::change::{IntroId, StableRef};
use ir::apply::PristineIntroTable;
use ir::body::{BodyEmbed, BodyMergeNote};
use ir::codec::{encode_body, decode_body, ir_path, Plane};
use ir::view::IrView;
use ir::continuity::Policy as ContinuityPolicy;

use crate::protocol::BodyWire;
use crate::f1::ContinuitySummary;
use crate::error::VcsError;
use crate::f1::{serialize_f1, F1View};
use crate::repo::{ChangeHashHex, IrRepository, IrTip};
use crate::serialize::{symbol_path, LinkWire};
use crate::wire::OwnedEntryPayload;

// ---------------------------------------------------------------------------
// GenerationMeta (§7.6)
// ---------------------------------------------------------------------------

/// Metadata attached to every change produced by a recording session.
///
/// Encoded with `postcard` under domain `nudox.genmeta.v1` and passed as the
/// `metadata` argument to `Change::make_change`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GenerationMeta {
    /// Schema epoch — currently always `2`.
    pub schema_epoch: u16,
    /// Config identifier (empty string = no config).
    pub config: String,
    /// Producer identifier string (e.g. `"rustdoc"`, `"deno_doc"`).
    pub producer: String,
    /// Job key (e.g. package version or job hash).
    pub job: String,
    /// Blake3 of the delta (content-hash of all new/changed F1 files), computed
    /// after σ substitution is complete.
    pub delta_digest: [u8; 32],
    /// Whether this is a partial generation (not all symbols were staged).
    pub partial: bool,
}

impl Default for GenerationMeta {
    fn default() -> Self {
        Self {
            schema_epoch: 2,
            config: String::new(),
            producer: String::new(),
            job: String::new(),
            delta_digest: [0u8; 32],
            partial: false,
        }
    }
}

impl GenerationMeta {
    /// Encode `self` as bytes for `Change::make_change`'s metadata argument.
    pub fn encode(&self) -> Vec<u8> {
        // Domain prefix + postcard payload.
        let postcard_bytes = postcard::to_stdvec(self).unwrap_or_default();
        let domain = b"nudox.genmeta.v1\0";
        let mut out = Vec::with_capacity(domain.len() + postcard_bytes.len());
        out.extend_from_slice(domain);
        out.extend_from_slice(&postcard_bytes);
        out
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single symbol to stage during a recording session.
pub struct StagedEntry {
    /// The stable reference for this symbol.  `stable.intro` is the wire id.
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
    /// Continuity summary from the P2 matcher.
    pub continuity: ContinuitySummary,
}

/// Streaming incremental IR recording session.
///
/// Obtain one via [`IrRepository::begin_recording`].  Stage entries in any
/// number of batches, optionally checkpoint (Phase A), then either
/// [`finish`](Self::finish) (Phase B: continuity + change) or
/// [`abandon`](Self::abandon) (discards without recording).
pub struct RecordingSession<'r, C: ChangeStore> {
    pub(crate) repo: &'r IrRepository<C>,
    /// Wire-id → (payload, parent, links) for all staged entries.
    /// Kept so Phase B can run the σ cascade and rewrite files.
    staged_payloads: HashMap<IntroId, (OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>)>,
    /// Wire-id → merged body facts (the implementation-plane `.nb` companion).
    /// Populated by [`stage_bodies`](Self::stage_bodies); Phase B feeds these to
    /// the continuity matcher (body axis) and rewrites the `.nb` path wire→durable
    /// in the σ cascade.
    staged_bodies: HashMap<IntroId, BodyEmbed>,
    /// Intros present in the WC at session start (populated by `begin_recording`).
    pub(crate) tip_intros: HashSet<IntroId>,
    /// Tip table materialized at begin_recording for Phase B continuity matching.
    pub(crate) tip_table: PristineIntroTable,
    /// Paths currently tracked in the working copy.
    wc_paths: Option<HashSet<String>>,
    /// Cumulative counts.
    total_added: u64,
    total_updated: u64,
    total_unchanged: u64,
    /// GenerationMeta template — callers may mutate before `.finish()`.
    pub meta: GenerationMeta,
}

impl<'r, C> RecordingSession<'r, C>
where
    C: ChangeStore + Clone + Send + 'static,
    C::Error: std::fmt::Display + Send + Sync + 'static,
{
    /// Create a new session from a repository reference and its current WC intros.
    pub(crate) fn new(
        repo: &'r IrRepository<C>,
        tip_intros: HashSet<IntroId>,
        tip_table: PristineIntroTable,
    ) -> Self {
        Self {
            repo,
            staged_payloads: HashMap::new(),
            staged_bodies: HashMap::new(),
            tip_intros,
            tip_table,
            wc_paths: None,
            total_added: 0,
            total_updated: 0,
            total_unchanged: 0,
            meta: GenerationMeta::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Phase A: stage / stage_links / checkpoint
    // -----------------------------------------------------------------------

    /// Stage a batch of entries into the working copy (Phase A).
    ///
    /// All entries must belong to this repository's package
    /// ([`StableRef::package`] == `repo.package_id()`).  An entry whose package
    /// differs is rejected with [`VcsError::ForeignPackage`] **before** any
    /// working-copy mutation for that entry.
    ///
    /// Files are written with [`serialize_f1`] (NdIrF1 canonical format).
    /// Wire-ids are used at this point; durable-id substitution happens in
    /// [`finish`](Self::finish) (Phase B).
    pub fn stage(&mut self, batch: Vec<StagedEntry>) -> Result<StageReport, VcsError> {
        let mut report = StageReport::default();
        let package_id = self.repo.package_id();

        let txn = self.repo.arc_txn_pub()?;
        let channel = IrRepository::<C>::open_or_create_channel_pub(&txn, self.repo.channel_name_ref())?;

        let channel_ms = txn.read().last_modified(&*channel.read());
        let floor = std::time::UNIX_EPOCH + std::time::Duration::from_millis(channel_ms);
        let mtime_floor = self.repo.now_pub().max(floor);

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

        for entry in batch {
            let intro = entry.stable.intro;

            if &entry.stable.package != package_id {
                return Err(VcsError::ForeignPackage {
                    expected: format!("{package_id:?}"),
                    got: format!("{:?}", entry.stable.package),
                });
            }

            // Canonical-owner rule for links.
            let mut owned_links: Vec<LinkWire> = Vec::new();
            for link in &entry.links {
                let b_intro = link.other.intro;
                let b_is_local = &link.other.package == package_id;
                let owner_intro = if b_is_local {
                    if intro.as_bytes() <= b_intro.as_bytes() { intro } else { b_intro }
                } else {
                    intro
                };
                if owner_intro == intro {
                    owned_links.push(link.clone());
                }
            }

            let bytes = serialize_f1(&entry.payload, entry.parent, &owned_links);
            let path = symbol_path(intro);

            if wc_paths.contains(&path) {
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

                    self.repo
                        .working_copy_ref()
                        .touch(&path, mtime_floor)
                        .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touch {path}: {e}")))?;

                    if self.tip_intros.contains(&intro) {
                        report.updated += 1;
                    } else {
                        report.added += 1;
                    }
                    if report.sample.len() < 5 {
                        report.sample.push((intro, entry.payload.symbol.name.clone()));
                    }
                } else {
                    report.unchanged += 1;
                }
            } else {
                self.repo.working_copy_ref().add_file(&path, bytes);
                txn.write().add_file(&path, 0).map_err(|e| {
                    VcsError::Pijul(anyhow::anyhow!("add_file: {e}"))
                })?;
                self.repo
                    .working_copy_ref()
                    .touch(&path, mtime_floor)
                    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("touch (new) {path}: {e}")))?;

                wc_paths.insert(path.clone());

                report.added += 1;
                if report.sample.len() < 5 {
                    report.sample.push((intro, entry.payload.symbol.name.clone()));
                }
            }

            self.staged_payloads.insert(intro, (entry.payload, entry.parent, owned_links));
        }

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (stage): {e}"))
        })?;

        self.total_added += report.added;
        self.total_updated += report.updated;
        self.total_unchanged += report.unchanged;
        Ok(report)
    }

    /// Stage additional links for an already-staged symbol (Phase A).
    ///
    /// Reads the existing F1 file, merges new links under the canonical-owner
    /// rule, and rewrites only if content differs.
    pub fn stage_links(&mut self, from: StableRef, links: Vec<LinkWire>) -> Result<(), VcsError> {
        let package_id = self.repo.package_id();

        if &from.package != package_id {
            return Err(VcsError::ForeignPackage {
                expected: format!("{package_id:?}"),
                got: format!("{:?}", from.package),
            });
        }

        let intro = from.intro;
        let path = symbol_path(intro);

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

        if !wc_paths.contains(&path) {
            return Ok(());
        }

        let mut existing = Vec::new();
        self.repo
            .working_copy_ref()
            .read_file(&path, &mut existing)
            .map_err(|e| VcsError::Pijul(anyhow::anyhow!("read_file (stage_links) {path}: {e}")))?;

        let view = F1View::from_bytes(&existing).map_err(|e| {
            VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
        })?;
        let payload = view.to_owned_payload().map_err(|e| {
            VcsError::CorruptSymbolFile { path: path.clone(), reason: e.to_string() }
        })?;
        let parent = view.parent();

        let mut merged: Vec<LinkWire> = view.links().to_vec();

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

        // Update the in-memory staged_payloads entry so Phase B sees the merged links.
        if let Some(entry) = self.staged_payloads.get_mut(&intro) {
            entry.2 = merged.clone();
        }

        let new_bytes = serialize_f1(&payload, parent, &merged);

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
    /// performing deletions (Phase A partial commit).
    ///
    /// Returns the change hash, or `None` if nothing has changed since the
    /// last checkpoint (or since session start).
    pub fn checkpoint(&mut self, msg: &str) -> Result<Option<ChangeHashHex>, VcsError> {
        self.record_and_apply(msg, Vec::new())
    }

    /// Stage merged body facts for staged symbols (Phase A).
    ///
    /// A [`BodyWire`] is the implementation-plane companion of a declaration
    /// entry, keyed by the same wire-id. Bodies are remembered in memory (not
    /// written to the working copy yet); Phase B ([`finish`](Self::finish)):
    /// (1) feeds them to the continuity matcher as the **body axis** signal, and
    /// (2) writes each to its `.nb` companion path at the *durable* id in the σ
    /// cascade. Calling this with an intro that never gets a matching `stage`
    /// entry simply leaves an orphan body that the cascade skips.
    pub fn stage_bodies(&mut self, batch: &[BodyWire]) {
        for bw in batch {
            self.staged_bodies.insert(bw.intro, bw.body.clone());
        }
    }

    // -----------------------------------------------------------------------
    // Phase B: finish
    // -----------------------------------------------------------------------

    /// Finalize the session (Phase B).
    ///
    /// 1. Runs the continuity matcher against `tip_table`.
    /// 2. Computes σ (wire-id → durable-id map).
    /// 3. Rewrites all in-generation files with durable ids (σ cascade).
    /// 4. Deletes WC files for intros absent from all `stage` calls.
    /// 5. Records a change with [`GenerationMeta`] metadata.
    /// 6. Returns [`FinishReport`].
    pub fn finish(self) -> Result<FinishReport, VcsError> {
        // --- Collect all staged entries for the continuity matcher ---
        // Sorted by wire-id: iteration order feeds `delta_hasher` below, and a
        // HashMap's order is nondeterministic — sorting makes `delta_digest`
        // reproducible (§7.6).
        let mut staged_wire_entries: Vec<(IntroId, OwnedEntryPayload, Option<IntroId>, Vec<LinkWire>)> =
            self.staged_payloads
                .iter()
                .map(|(wire_id, (payload, parent, links))| {
                    (*wire_id, payload.clone(), *parent, links.clone())
                })
                .collect();
        staged_wire_entries.sort_by_key(|(id, _, _, _)| *id);

        // --- Candidate-deleted set: tip ids with no staged (wire) payload ---
        // This is exactly the set the continuity matcher treats as deletion
        // candidates (`compute_sigma`'s internal `deleted_ids`) — the ONLY tips
        // whose bodies the matcher ever compares. Computed once, before σ, so we
        // can (a) materialize only these tip bodies and (b) derive the final
        // deletion set from it after σ.
        let candidate_deleted: HashSet<IntroId> = self
            .tip_intros
            .iter()
            .filter(|i| !self.staged_payloads.contains_key(*i))
            .copied()
            .collect();

        // --- Body maps for the continuity body axis (§3.7) ---
        // Staged bodies are keyed by wire-id. Tip bodies are materialized ONLY
        // for the deletion candidates (the matcher never consults a preserved
        // tip's body) — CONTINUITY-PQGRAM-PLAN §4.4 materialization deferral:
        // O(|deleted|) `.nb` reads instead of O(|live|). They come from the
        // working-copy `.nb` companions a prior generation's σ cascade wrote
        // (empty on the first body-carrying generation → the axis abstains,
        // which is never-merge-safe).
        let staged_bodies_bt: BTreeMap<IntroId, BodyEmbed> = self
            .staged_bodies
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let mut tip_bodies_bt: BTreeMap<IntroId, BodyEmbed> = BTreeMap::new();
        for tip_id in &candidate_deleted {
            let mut buf = Vec::new();
            if self
                .repo
                .working_copy_ref()
                .read_file(&ir_path(*tip_id, Plane::Body), &mut buf)
                .is_ok()
                && !buf.is_empty()
                && let Ok(body) = decode_body(&buf)
            {
                tip_bodies_bt.insert(*tip_id, body);
            }
        }

        // --- Phase B: continuity matching (declaration + body axis) ---
        //
        // The matcher now lives in `nudox_ir::continuity`, because it is a pure
        // function of two sealed generations and needs nothing from the patch
        // engine. `IrView` carries the bodies alongside the table, which is why
        // the old two-entry-point split (`compute_sigma` for declarations,
        // `compute_sigma_with_bodies` for declarations+bodies) collapsed into a
        // single `resolve` — the body axis is part of the input, not a variant
        // of the call.
        let mut prev_view = IrView::new(self.tip_table.clone());
        for (id, body) in &tip_bodies_bt {
            prev_view.set_body(*id, body.clone());
        }

        let next_view = crate::lower::build_ir_view_unnamed(
            staged_wire_entries
                .iter()
                .map(|(id, payload, parent, _links)| (*id, payload.clone(), *parent)),
            &staged_bodies_bt,
        );

        let subst = ir::continuity::resolve(&prev_view, &next_view, &ContinuityPolicy::default());
        let sigma = subst.sigma();
        let continuity = crate::lower::summarize_continuity(&subst, &prev_view, &next_view);

        // --- σ cascade: rewrite all staged files with durable ids ---
        let txn = self.repo.arc_txn_pub()?;
        let channel_ms = {
            let ch = IrRepository::<C>::open_or_create_channel_pub(&txn, self.repo.channel_name_ref())?;
            txn.read().last_modified(&*ch.read())
        };
        let floor = std::time::UNIX_EPOCH + std::time::Duration::from_millis(channel_ms);
        let mtime_floor = self.repo.now_pub().max(floor);

        // Delta hasher for GenerationMeta.delta_digest
        let mut delta_hasher = blake3::Hasher::new();

        for (wire_id, payload, parent, links) in &staged_wire_entries {
            // Map wire references to durable ids.
            let durable_id = sigma.get(wire_id).copied().unwrap_or(*wire_id);
            let durable_parent = parent.map(|p| sigma.get(&p).copied().unwrap_or(p));
            let durable_links: Vec<LinkWire> = links
                .iter()
                .map(|l| {
                    let durable_other = sigma.get(&l.other.intro).copied().unwrap_or(l.other.intro);
                    LinkWire {
                        other: ir::change::StableRef::new(l.other.package.clone(), durable_other),
                        kind_self: l.kind_self,
                        kind_other: l.kind_other,
                    }
                })
                .collect();

            // σ substitution cascade (§5, K-Subst-Cascade): rewrite every
            // in-payload reference w→σ(w) and re-seal before the WC write, so a
            // reused durable id propagates into all signatures/fields/reexport
            // targets that mention it. Parent + links are remapped above; this
            // covers the intra-payload refs.
            let durable_payload = crate::subst::substitute_and_reseal(payload, &sigma);
            let new_bytes = serialize_f1(&durable_payload, durable_parent, &durable_links);
            delta_hasher.update(&new_bytes);

            let new_path = symbol_path(durable_id);
            let old_path = symbol_path(*wire_id);

            // If the durable id differs from wire id, remove the old file and
            // add the new one at the durable path.
            if durable_id != *wire_id {
                // Remove wire-id file.
                let _ = self.repo.working_copy_ref().remove_path(&old_path, false);
                let _ = txn.write().remove_file(&old_path);

                // Write durable-id file.
                self.repo.working_copy_ref().add_file(&new_path, new_bytes.clone());
                let _ = txn.write().add_file(&new_path, 0);
            } else {
                // Overwrite in place (file was already at the correct path).
                let mut existing = Vec::new();
                let _ = self.repo.working_copy_ref().read_file(&new_path, &mut existing);
                if existing != new_bytes
                    && let Ok(mut w) = self.repo
                        .working_copy_ref()
                        .write_file(&new_path, libpijul::pristine::Inode::ROOT)
                    {
                        let _ = w.write_all(&new_bytes);
                    }
            }
            let _ = self.repo.working_copy_ref().touch(&new_path, mtime_floor);

            // σ cascade for the `.nb` body companion: persist the staged body at
            // the DURABLE path. Unlike the declaration file, a body is never
            // written at the wire path (`stage_bodies` only buffers in memory),
            // so there is no wire-path `.nb` to remove on a wire→durable remap.
            // The durable `.nb` may or may not already exist (it does when the
            // tip carried a body for this id — e.g. a rename reusing a durable id
            // whose prior generation had a body), so create-or-overwrite, never a
            // blind `add_file` on an already-tracked path. A body-only edit that
            // serializes to identical bytes is skipped (no spurious change).
            if let Some(body) = self.staged_bodies.get(wire_id)
                && let Ok(body_bytes) = encode_body(body)
            {
                let new_bpath = ir_path(durable_id, Plane::Body);
                let mut existing = Vec::new();
                let _ = self.repo.working_copy_ref().read_file(&new_bpath, &mut existing);
                if existing.is_empty() {
                    self.repo.working_copy_ref().add_file(&new_bpath, body_bytes);
                    let _ = txn.write().add_file(&new_bpath, 0);
                } else if existing != body_bytes
                    && let Ok(mut w) = self
                        .repo
                        .working_copy_ref()
                        .write_file(&new_bpath, libpijul::pristine::Inode::ROOT)
                {
                    let _ = w.write_all(&body_bytes);
                }
                let _ = self.repo.working_copy_ref().touch(&new_bpath, mtime_floor);
            }
        }

        // --- Deletions ---
        // A candidate-deleted tip whose durable id is REUSED by σ (a rename/move
        // target) is NOT a deletion — the σ cascade just rewrote its `.nir`/`.nb`
        // with the new content at that same durable id. Excluding σ's durable
        // targets keeps finish()'s working-copy deletions consistent with
        // `compute_sigma`'s `Deleted` ops (which likewise exclude matched tips).
        // Without this, every σ-matched rename would delete the very symbol it
        // just reused — silent data loss.
        let sigma_targets: HashSet<IntroId> = sigma.values().copied().collect();
        let mut to_delete: Vec<IntroId> = candidate_deleted
            .iter()
            .filter(|i| !sigma_targets.contains(*i))
            .copied()
            .collect();
        to_delete.sort(); // deterministic deletion order
        let deleted = to_delete.len() as u64;

        for intro in &to_delete {
            let path = symbol_path(*intro);
            let _ = self.repo.working_copy_ref().remove_path(&path, false);
            let _ = txn.write().remove_file(&path);
            // Remove the `.nb` body companion for the deleted symbol, if any.
            let bpath = ir_path(*intro, Plane::Body);
            let _ = self.repo.working_copy_ref().remove_path(&bpath, false);
            let _ = txn.write().remove_file(&bpath);
        }

        txn.commit().map_err(|e| {
            VcsError::Pijul(anyhow::anyhow!("commit (finish-cascade): {e}"))
        })?;

        // Build GenerationMeta with finalized delta_digest.
        let mut meta = self.meta.clone();
        meta.delta_digest = delta_hasher.finalize().into();
        let meta_bytes = meta.encode();

        let change = self.record_and_apply_with_meta("finish", meta_bytes)?;
        let tip = self.repo.tip()?;

        Ok(FinishReport {
            tip,
            change,
            added: self.total_added,
            updated: self.total_updated,
            deleted,
            continuity,
        })
    }

    /// Abandon the session without recording a change.
    pub fn abandon(self) -> Result<(), VcsError> {
        self.repo.reset_working_copy_tip();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn record_and_apply(
        &self,
        msg: &str,
        metadata: Vec<u8>,
    ) -> Result<Option<ChangeHashHex>, VcsError> {
        self.record_and_apply_with_meta(msg, metadata)
    }

    fn record_and_apply_with_meta(
        &self,
        msg: &str,
        metadata: Vec<u8>,
    ) -> Result<Option<ChangeHashHex>, VcsError> {
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
            metadata,
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
