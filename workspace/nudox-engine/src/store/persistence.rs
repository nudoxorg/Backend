//! The local libpijul-backed IR store — [`PersistenceStore`] and its
//! [`IrSource`] decorator [`PersistedSource`].
//!
//! # The error class this closes
//!
//! `Corpus` is an in-memory `BTreeMap` and `PackageView` has no `Serialize`
//! (`corpus.rs`'s own module docs). Every engine launch therefore re-runs the
//! producer over every package, which `runtime::Engine::start_with_versions`'s
//! doc comment used to admit outright: *"There is no incremental or
//! shared-storage path between generations — the IR-native VCS in
//! `workspace/ir-vcs` is where that belongs, and wiring it in is not a
//! `nudox-engine` change."* That comment was true until this module. Wiring
//! it in **is** a `nudox-engine` change, and this is it — `docs/IR-STORAGE-PLAN.md`
//! §1a names the split precisely: `ir-vcs`'s libpijul-backed `IrRepository`
//! is the *local* store (a real POSIX disk, one process, one compiler), and
//! `heart::deployment::DeploymentProfile::embedded()` already provisions
//! `ir_repo_root: PathBuf` for exactly this — *"Root of the local libpijul
//! `IrRepository`"* — and never opened it before now.
//!
//! # Layout on disk
//!
//! One [`ir_vcs::IrRepository`] per resident package **lineage**, at
//! `<ir_repo_root>/<sanitized ecosystem>/<sanitized name>/`. Each of those
//! directories additionally holds a small `.nudox-lineage.json` sidecar
//! (`{ecosystem, name, version}`, plain `serde_json`) beside `IrRepository`'s
//! own `pristine/` and `changes/` subdirectories.
//!
//! The sidecar exists because [`PersistenceStore::materialize_all`] has to
//! **discover** which lineages are on disk without being told — the whole
//! point of `a_restart_repopulates_the_corpus_without_being_told_what_to_load`
//! (`tests/local_persistence.rs`) — and a package name may contain characters
//! `sanitize` maps onto the same directory segment (`npm`'s `@scope/pkg`, most
//! notably: the `/` is replaced the same way a literal `-` would be). Reading
//! the *sanitized path* back is therefore not reliable; reading the sidecar
//! is, because it carries the real `PackageLineageId` verbatim.
//!
//! # What identifies a stored generation (the P5 judgement call)
//!
//! `ir-vcs` offers three candidate identities for "is this the same
//! generation I already have": `IntroId` (per-declaration), a
//! `GenerationStamp` (does not exist as a type — there is no package-wide
//! content root today; that is `docs/IR-STORAGE-PLAN.md`'s own
//! `GenerationRoot`, still unbuilt), and a channel tip (`IrTip`, a Merkle over
//! the whole recorded history).
//!
//! This module uses **none of them directly** — it lets
//! [`ir_vcs::repo::IrRepository::record_generation`] answer the question
//! itself, because that is exactly what it already does: it diffs the
//! raised [`ir_vcs::wire::PayloadTable`] against the channel tip's working
//! copy *per symbol file*, skips writing any file whose bytes are unchanged,
//! and returns `Ok(None)` (no libpijul change recorded at all) when nothing
//! differs. A `GenerationRoot`-style whole-package hash would answer the same
//! question at coarser granularity — "did *anything* change" instead of
//! "which symbols changed" — for no benefit here, since this module never
//! needs the finer answer (fault-in / partial reuse is P6, not this change).
//! Reusing the mechanism `IrRepository` already has, rather than inventing a
//! second one on top of it, is the deliberate choice: two independent "is
//! this unchanged" answers that could disagree would be strictly worse than
//! one.
//!
//! # Failure posture
//!
//! Every public method here degrades loudly and returns a best-effort
//! result; **none of them panic**, and a broken/corrupt/unreadable local
//! store never prevents the engine from starting or producing. Persistence
//! is an optimisation on top of "the engine works," not a precondition for
//! it — see [`PersistenceStore::record`] and
//! [`PersistenceStore::materialize_all`]'s doc comments for exactly what
//! "degrade" means at each call site. `tracing::warn!` is the typed,
//! structured event a host can observe this through; there is no dedicated
//! `PackageLoadEvent` variant for it because a persistence failure is not a
//! *package* failure — the package still loads, just from source instead of
//! from the local store.
//!
//! # Fidelity — why this module writes two stores (P5, part 2)
//!
//! An earlier version of this module raised every entry through
//! [`ir_vcs::raise`] and stored *only* the resulting [`ir_vcs::wire::PayloadTable`]
//! in the [`ir_vcs::IrRepository`]. That format is a **lossy** compression of
//! the semantic IR — `raise.rs`'s own module docs document it — and every
//! symbol restored through it silently lost its type information. Measured on
//! `tests/persistence_fidelity.rs`'s fixture (real, not hypothetical):
//!
//! | declaration | produced | after restart via the wire round trip |
//! |---|---|---|
//! | `fn base_method(&self) -> u32` | `-> ty(u32,-)` | `-> id(?)` |
//! | `fn generic_fn<T: Derived>(input: T) -> impl Base` | `(input: T) -> impl Base` | `(_) -> ?` |
//! | `type BoundedAlias<T: Base> = Option<T>` | `= Option<T>` | `= any` |
//!
//! Every function lost its return type; every parameter lost its name and
//! type. `symbol_count` stayed identical (22/22) — the loss is in *fields on
//! existing entries*, invisible to an entry-count check, which is exactly why
//! `tests/local_persistence.rs` (asserting only `symbol_count > 0`) passed
//! while this shipped.
//!
//! ## Why the fix is not "make the wire format richer"
//!
//! The obvious-looking fix — extend [`ir_vcs::wire::TypeWire`]/`TypeRefWire`
//! to cover the `Type` variants it currently folds into `TypeWire::Any` — was
//! considered and rejected. Two structural facts, verified before writing any
//! code:
//!
//! 1. [`ir_vcs::wire::PayloadTable`] is **concretely typed**, not generic:
//!    `entries: BTreeMap<IntroId, (OwnedEntryPayload, Option<IntroId>)>`. There
//!    is no type parameter to substitute a lossless payload into —
//!    [`ir_vcs::repo::IrRepository::record_generation`] and `::materialize`
//!    are hard-typed to this one wire shape, with no other entry point
//!    (`raise.rs`'s own docs say so explicitly).
//! 2. The wire payload is not serialized by this crate at all — it goes
//!    through `ir_vcs::f1`, the NdIrF1 text format, whose key registry is
//!    **vendored into the libpijul fork itself**
//!    (`ir_vcs::f1::{KEY_*}` are re-exports of
//!    `libpijul::nudox_f1::registry::{KEY_*}`). Widening what a symbol file
//!    can encode means redesigning a format two layers below this crate, in a
//!    vendored dependency — the opposite of a local, low-risk fix, and exactly
//!    what `docs/IR-STORAGE-PLAN.md` §4-§5 already flags as out of scope for
//!    wiring up local persistence.
//!
//! So the coupling is structural, not incidental, and forcing losslessness
//! through `ir_vcs::wire` was ruled out rather than attempted.
//!
//! ## The fix actually taken: a lossless sidecar, the repo kept for versioning
//!
//! [`ir_vcs::IrRepository`] stays wired exactly as before — [`PersistenceStore::record`]
//! still raises and records a generation into it, so the versioning machinery
//! `docs/IR-STORAGE-PLAN.md` §1a/P5 wants (per-symbol diffing, generation
//! history, a real base for future offline-branch work) keeps accumulating
//! real history. What changes is that **nothing reads from it any more**.
//!
//! Instead, [`PersistenceStore::record`] additionally writes every live entry's
//! *semantic* [`Entry`] — the exact value `IrView` already holds, no raise, no
//! wire, no loss — into a small content-addressed sidecar
//! ([`EntrySidecarManifest`] + [`heart::cache::DiskCas`]), and
//! [`PersistenceStore::materialize_all`] reads **only** the sidecar. This is
//! not a new idea invented for this fix: it is `store::remote::IrSnapshot`
//! (`Vec<(IntroId, Entry, Option<IntroId>)>`), which already round-trips
//! semantic entries losslessly for the remote plane, moved onto local disk and
//! made content-addressed per entry so an unchanged symbol is not rewritten —
//! `docs/IR-STORAGE-PLAN.md` §5 names this shape directly as the fix for the
//! sibling "empty IR on macOS" problem, and §1's `GenerationRoot` is the same
//! `Vec<(IntroId, ContentHash)>` shape the sidecar's manifest uses, scoped
//! locally instead of the (still-unbuilt) remote plane.
//!
//! The cost, stated in the open: **two stores that can disagree.** If the
//! sidecar manifest is missing, or names a hash the CAS does not have (a
//! crash mid-write, disk corruption, a store written before this change), that
//! is treated as "nothing usable here" — [`PersistenceStore::materialize_all`]
//! skips the lineage entirely rather than serving whatever the *other* store
//! happens to hold. Reproducing from source is strictly better than serving a
//! degraded or partially-reconstructed copy; see the module-level "Failure
//! posture" section above and `tests/persistence_fidelity.rs`'s own framing of
//! the bar this has to clear.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt as _};
use heart::ContentHash;
use heart::cache::{CasError, DiskCas};
use nudox_ir::apply::PristineIntroTable;
use nudox_ir::change::{IntroId, PackageLineageId};
use nudox_ir::entry::Entry;
use nudox_ir::view::IrView;
use serde::{Deserialize, Serialize};

use crate::store::package::{PackageView, Provenance};
use crate::store::source::{Error as SourceError, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor};

// ---------------------------------------------------------------------------
// PersistenceStore
// ---------------------------------------------------------------------------

/// A libpijul-backed local IR store rooted at one directory, holding one
/// [`ir_vcs::IrRepository`] per resident package lineage.
///
/// Cheap to clone (a `PathBuf` and nothing else) — every operation opens the
/// relevant per-lineage repository fresh rather than caching an open handle,
/// trading a little per-call overhead (one `sanakirja` open) for zero shared
/// mutable state across the `Send + Sync + 'static` boundary [`IrSource`]
/// requires.
#[derive(Clone)]
pub(crate) struct PersistenceStore {
    root: PathBuf,
}

/// The channel every [`ir_vcs::IrRepository`] this module opens records its
/// generations on.
///
/// A single fixed name rather than one derived from anything version-shaped:
/// `PersistenceStore` records the *current* generation of a lineage, not a
/// branch per version — multi-version history for one lineage is
/// `crate::versions::VersionRegistry`'s job in the in-memory corpus today,
/// and extending the local store to mirror it is future work (see the
/// module docs' "What identifies a stored generation").
const CHANNEL: &str = "main";

impl PersistenceStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn package_root(&self, lineage: &PackageLineageId) -> PathBuf {
        self.root
            .join(sanitize(lineage.ecosystem.as_str()))
            .join(sanitize(lineage.name.as_str()))
    }

    /// Root of the entry sidecar's [`DiskCas`], shared across every lineage
    /// this store holds.
    ///
    /// One CAS rather than one per lineage: entries are content-addressed by
    /// `(Entry, parent)` bytes alone (never by package identity), so an
    /// identical declaration recurring across packages — a common shape
    /// re-exported, a trivial accessor duplicated by a codegen macro — is
    /// stored once. Dot-prefixed so it can never collide with
    /// [`sanitize`]'s output for a real ecosystem name (which never begins
    /// with `.`, since ecosystem identifiers are a fixed in-tree set, not
    /// attacker- or user-controlled input).
    fn entries_cas_root(&self) -> PathBuf {
        self.root.join(".nudox-entries-cas")
    }

    /// Record a **freshly produced** generation.
    ///
    /// Never panics and never blocks a load on failure. Three independent
    /// steps run — metadata, the fidelity-critical entry sidecar, and the
    /// `ir_vcs` versioning repo — each logged and swallowed on its own
    /// failure rather than short-circuiting the others: a repo write failure
    /// (versioning history only, never read back — see the module docs)
    /// must not also cost the sidecar write that fidelity actually depends
    /// on, and vice versa. The package the caller just produced is still
    /// resident in memory for this session regardless of which steps
    /// succeeded — it simply will not survive the next restart if the
    /// sidecar step failed, which is exactly the pre-existing behaviour for
    /// a host that never sets `ir_repo_root` at all.
    ///
    /// Blocking: performs synchronous filesystem I/O (and, for the repo
    /// step, `sanakirja`). Callers on an async runtime must run this via
    /// `spawn_blocking` (see `runtime::load::drive_load`'s call site).
    pub(crate) fn record(&self, lineage: &PackageLineageId, version: Option<&str>, view: &IrView) {
        let dir = self.package_root(lineage);
        if let Err(error) = std::fs::create_dir_all(&dir).map_err(|source| PersistenceError::Io {
            path: dir.clone(),
            source,
        }) {
            tracing::warn!(
                package = %lineage,
                %error,
                "local IR persistence: could not create the package directory; this generation will not survive a restart"
            );
            return;
        }

        if let Err(error) = write_metadata(&dir, lineage, version) {
            tracing::warn!(
                package = %lineage,
                %error,
                "local IR persistence: failed to write the lineage sidecar; this generation will not be discoverable at the next restart"
            );
        }

        // Fidelity-critical: see the module docs' "The fix actually taken".
        // This is what `materialize_all` reads back.
        if let Err(error) = self.try_record_entries(&dir, view) {
            tracing::warn!(
                package = %lineage,
                %error,
                "local IR persistence: failed to record the lossless entry sidecar; this generation will not survive a restart"
            );
        }

        // Versioning-only: accumulates `ir_vcs` history for future
        // diff/offline-branch work. Not consulted by any read path today, so
        // its failure never affects what a restart serves.
        if let Err(error) = self.try_record_repo(&dir, lineage, view) {
            tracing::warn!(
                package = %lineage,
                %error,
                "local IR persistence: failed to record this generation into the ir_vcs repository (versioning history only; restart fidelity is unaffected)"
            );
        }
    }

    /// Raise `view` into `ir_vcs`'s wire `PayloadTable` and record it as one
    /// generation of the per-lineage [`ir_vcs::IrRepository`].
    ///
    /// This is deliberately the *only* place `ir_vcs::raise` is still called
    /// from this module — see the module docs for why its output is no
    /// longer part of any read path.
    fn try_record_repo(
        &self,
        dir: &Path,
        lineage: &PackageLineageId,
        view: &IrView,
    ) -> Result<(), PersistenceError> {
        let repo = ir_vcs::IrRepository::open(dir, lineage.clone(), CHANNEL).map_err(|source| {
            PersistenceError::Repository {
                lineage: lineage.clone(),
                path: dir.to_path_buf(),
                source,
            }
        })?;
        let table = ir_vcs::raise::raise_view(view);
        repo.record_generation(&table)
            .map_err(|source| PersistenceError::Repository {
                lineage: lineage.clone(),
                path: dir.to_path_buf(),
                source,
            })?;
        Ok(())
    }

    /// Write every live entry's semantic [`Entry`] into the shared
    /// content-addressed sidecar, then overwrite this lineage's
    /// [`EntrySidecarManifest`] with the current live set.
    ///
    /// Content-addressing gives the same write-amplification property
    /// `docs/IR-STORAGE-PLAN.md` §2.1 describes for `GenerationRoot`: an
    /// entry whose `(Entry, parent)` bytes are unchanged from a prior
    /// generation hashes to the same key, and [`DiskCas::put_keyed_sync`] is a
    /// no-op (existence check, no write) when that key is already present —
    /// so a one-symbol edit costs one blob write, not a whole-package
    /// rewrite. The manifest itself (which entries are *currently* live) is
    /// always rewritten in full; it is tens of bytes per entry, not the
    /// entry payload.
    fn try_record_entries(&self, dir: &Path, view: &IrView) -> Result<(), PersistenceError> {
        let cas = DiskCas::open(self.entries_cas_root())?;
        let mut entries = Vec::new();
        for (intro, entry) in view.entries_sorted() {
            let parent = view.parent_of(intro);
            let record = EntryRecord {
                entry: entry.clone(),
                parent,
            };
            let bytes = serde_json::to_vec(&record).map_err(|source| PersistenceError::EntryCodec {
                lineage: view.package().clone(),
                intro,
                source,
            })?;
            let hash = ContentHash::of_bytes(&bytes);
            cas.put_keyed_sync(hash, Bytes::from(bytes))?;
            entries.push((intro, hash));
        }
        write_entries_manifest(dir, &EntrySidecarManifest { entries })
    }

    /// Materialize every lineage this store currently holds, as
    /// `(PackageView, version)` pairs ready to feed straight into
    /// [`crate::store::source::LoadEvent::Ready`].
    ///
    /// Best-effort and non-fatal at every level: a missing root directory
    /// (nothing has ever been persisted here) yields an empty result, not an
    /// error; one lineage directory that is corrupt, mid-write, or otherwise
    /// unreadable is skipped with a `warn` log rather than aborting the whole
    /// walk — a broken package must not make every *other* persisted package
    /// invisible.
    ///
    /// Blocking — see [`PersistenceStore::record`]'s note.
    pub(crate) fn materialize_all(&self) -> Vec<(Arc<PackageView>, Option<String>)> {
        let Ok(ecosystems) = std::fs::read_dir(&self.root) else {
            // Absent root: a fresh store, or a data root nothing has ever
            // written to. Not an error — see the doc comment.
            return Vec::new();
        };

        let mut out = Vec::new();
        for eco_entry in ecosystems.flatten() {
            if !eco_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(name_entries) = std::fs::read_dir(eco_entry.path()) else {
                continue;
            };
            for name_entry in name_entries.flatten() {
                let dir = name_entry.path();
                if !dir.is_dir() {
                    continue;
                }
                match self.try_materialize_one(&dir) {
                    Ok(Some(loaded)) => out.push(loaded),
                    Ok(None) => {}
                    Err(error) => tracing::warn!(
                        path = %dir.display(),
                        %error,
                        "local IR persistence: skipping an unreadable package directory"
                    ),
                }
            }
        }
        out
    }

    /// Rebuild one lineage's [`PackageView`] **from the entry sidecar only**.
    ///
    /// Deliberately does not consult `ir_vcs::IrRepository::materialize` —
    /// see the module docs' "The fix actually taken". Every early return here
    /// (missing lineage sidecar, missing entries manifest, an empty live set)
    /// is the same judgement call: treat "the lossless store is incomplete"
    /// exactly like "the lossless store is absent", because a partial
    /// reconstruction is a degraded copy the caller cannot distinguish from a
    /// complete one — the one thing `tests/persistence_fidelity.rs` forbids.
    fn try_materialize_one(
        &self,
        dir: &Path,
    ) -> Result<Option<(Arc<PackageView>, Option<String>)>, PersistenceError> {
        let Some(metadata) = read_metadata(dir)? else {
            // No lineage sidecar — not a lineage directory this store wrote
            // (or a write that crashed before the sidecar landed). Skip
            // quietly: this is the same "nothing here yet" case as a missing
            // root, just one level down.
            return Ok(None);
        };

        let Some(manifest) = read_entries_manifest(dir)? else {
            // The lineage sidecar exists but the entries manifest does not:
            // either this generation predates the sidecar (an `ir_vcs`-only
            // store from before this fix) or `try_record_entries` failed
            // partway through a prior run. Either way there is no lossless
            // copy to serve — recompute from source rather than falling back
            // to the lossy `ir_vcs` repo.
            return Ok(None);
        };
        if manifest.entries.is_empty() {
            return Ok(None);
        }

        let cas = DiskCas::open(self.entries_cas_root())?;
        let mut table = PristineIntroTable::new();
        for (intro, hash) in manifest.entries {
            let bytes = cas.get_sync(hash)?.ok_or_else(|| PersistenceError::MissingEntry {
                lineage: metadata.lineage.clone(),
                intro,
                hash,
            })?;
            let record: EntryRecord =
                serde_json::from_slice(&bytes).map_err(|source| PersistenceError::EntryCodec {
                    lineage: metadata.lineage.clone(),
                    intro,
                    source,
                })?;
            table.insert_live(intro, record.entry, record.parent);
        }

        let view = IrView::with_package(metadata.lineage.clone(), table);
        let package = Arc::new(PackageView::build(view, Provenance::SnapshotLocal));
        Ok(Some((package, metadata.version)))
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong reading or writing the local IR store.
///
/// Never propagated to a caller of [`PersistenceStore`] or [`PersistedSource`]
/// — both log-and-degrade instead (see the module docs' "Failure posture").
/// This type exists so the logging call sites have a real error to format
/// rather than a string, and so a future caller that *does* want to act on a
/// specific failure (rather than merely report it) has something to match on.
#[derive(Debug, thiserror::Error)]
enum PersistenceError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("local IR repository for {lineage} at {path}: {source}")]
    Repository {
        lineage: PackageLineageId,
        path: PathBuf,
        #[source]
        source: ir_vcs::VcsError,
    },
    #[error("lineage sidecar at {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("entries manifest at {path}: {source}")]
    EntriesManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The entry sidecar's shared [`DiskCas`] failed to open, read, or write.
    /// `#[from]` lets `?` convert directly at every `DiskCas` call site in
    /// this module (see `try_record_entries`/`try_materialize_one`).
    #[error("entry sidecar CAS: {0}")]
    Cas(#[from] CasError),
    /// A manifest named a hash the CAS does not have. Not necessarily
    /// corruption — it is also what a torn write between `put_keyed_sync`
    /// (per entry) and `write_entries_manifest` (whole-manifest) would look
    /// like if the process died mid-`try_record_entries`. Either way this
    /// lineage's sidecar is incomplete and must not be served partially —
    /// see `try_materialize_one`'s doc comment.
    #[error(
        "entry {intro} for {lineage}: entries manifest names hash {hash}, \
         which is not present in the sidecar CAS (corrupt or partially-written store)"
    )]
    MissingEntry {
        lineage: PackageLineageId,
        intro: IntroId,
        hash: ContentHash,
    },
    /// One entry's sidecar payload failed to encode (on write) or decode (on
    /// read). `Entry` is the semantic IR type this module now stores
    /// verbatim, via plain `serde_json` — the same codec `IrSnapshot`
    /// (`store::remote`) already proved works for it.
    #[error("entry {intro} sidecar payload for {lineage}: {source}")]
    EntryCodec {
        lineage: PackageLineageId,
        intro: IntroId,
        #[source]
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------------
// Directory naming + the lineage sidecar
// ---------------------------------------------------------------------------

/// Map a path segment to filesystem-safe characters.
///
/// Not a security boundary (this data never crosses a trust boundary — it is
/// the same process's own package names) — just enough to turn an npm scoped
/// name (`@scope/pkg`) or any other punctuation-bearing identifier into one
/// path component instead of an unintended subdirectory or an illegal
/// filename on the host OS.
fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The lineage sidecar's on-disk shape. Deliberately minimal — this is a
/// discovery index, not a metadata store (`crate::PackageMetadata` is not
/// persisted here at all; a restored package's `metadata()` is the type's
/// `Default`, same as `IrSnapshot::replay`'s equivalent gap in
/// `store::remote`).
#[derive(Serialize, Deserialize)]
struct LineageSidecar {
    ecosystem: String,
    name: String,
    version: Option<String>,
}

struct Metadata {
    lineage: PackageLineageId,
    version: Option<String>,
}

fn sidecar_path(dir: &Path) -> PathBuf {
    dir.join(".nudox-lineage.json")
}

fn write_metadata(
    dir: &Path,
    lineage: &PackageLineageId,
    version: Option<&str>,
) -> Result<(), PersistenceError> {
    let sidecar = LineageSidecar {
        ecosystem: lineage.ecosystem.as_str().to_owned(),
        name: lineage.name.as_str().to_owned(),
        version: version.map(str::to_owned),
    };
    let path = sidecar_path(dir);
    let bytes = serde_json::to_vec_pretty(&sidecar).map_err(|source| PersistenceError::Metadata {
        path: path.clone(),
        source,
    })?;
    std::fs::write(&path, bytes).map_err(|source| PersistenceError::Io { path, source })
}

fn read_metadata(dir: &Path) -> Result<Option<Metadata>, PersistenceError> {
    let path = sidecar_path(dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PersistenceError::Io { path, source }),
    };
    let sidecar: LineageSidecar =
        serde_json::from_slice(&bytes).map_err(|source| PersistenceError::Metadata {
            path: path.clone(),
            source,
        })?;
    Ok(Some(Metadata {
        lineage: PackageLineageId::new(
            nudox_ir::change::EcosystemId::new(sidecar.ecosystem),
            nudox_ir::change::PackageName::new(sidecar.name),
        ),
        version: sidecar.version,
    }))
}

// ---------------------------------------------------------------------------
// The entry sidecar — a lossless, content-addressed store for `Entry`
// ---------------------------------------------------------------------------
//
// See the module docs' "Fidelity" section for why this exists and what it
// replaces. Two on-disk pieces:
//
// - A shared `DiskCas` (root: `entries_cas_root()`) holding one blob per
//   distinct `(Entry, parent)` value, keyed by its content hash.
// - Per lineage, an `EntrySidecarManifest` (`.nudox-entries.json`, beside the
//   lineage sidecar) naming which hashes are *currently* live for that
//   package — `docs/IR-STORAGE-PLAN.md` §1's `GenerationRoot` shape
//   (`Vec<(IntroId, ContentHash)>`), scoped to the local store.

/// One CAS entry: a live declaration's semantic value plus its parent edge.
///
/// `parent` travels with the entry rather than living only in the manifest
/// so that a single CAS blob is a complete, independently-verifiable record —
/// `docs/IR-STORAGE-PLAN.md` §7's open question 3 ("where do parent edges
/// live") is resolved here in favour of "inside the hashed payload": a
/// symbol that is re-parented (moved between modules without changing
/// otherwise) is, correctly, a *different* stored value rather than a
/// mutation of the old one.
#[derive(Serialize, Deserialize)]
struct EntryRecord {
    entry: Entry,
    parent: Option<IntroId>,
}

/// One lineage's live generation: every currently-resident `IntroId`, paired
/// with the [`ContentHash`] of its [`EntryRecord`] in the shared CAS.
///
/// Sorted by construction (built from [`IrView::entries_sorted`], which is
/// itself deterministic — see that method's doc comment) so two writes of an
/// unchanged generation produce byte-identical manifest files, not just
/// byte-identical entries.
#[derive(Serialize, Deserialize)]
struct EntrySidecarManifest {
    entries: Vec<(IntroId, ContentHash)>,
}

fn entries_manifest_path(dir: &Path) -> PathBuf {
    dir.join(".nudox-entries.json")
}

fn write_entries_manifest(dir: &Path, manifest: &EntrySidecarManifest) -> Result<(), PersistenceError> {
    let path = entries_manifest_path(dir);
    let bytes =
        serde_json::to_vec_pretty(manifest).map_err(|source| PersistenceError::EntriesManifest {
            path: path.clone(),
            source,
        })?;
    std::fs::write(&path, bytes).map_err(|source| PersistenceError::Io { path, source })
}

fn read_entries_manifest(dir: &Path) -> Result<Option<EntrySidecarManifest>, PersistenceError> {
    let path = entries_manifest_path(dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PersistenceError::Io { path, source }),
    };
    let manifest = serde_json::from_slice(&bytes).map_err(|source| PersistenceError::EntriesManifest {
        path,
        source,
    })?;
    Ok(Some(manifest))
}

// ---------------------------------------------------------------------------
// PersistedSource — the IrSource decorator
// ---------------------------------------------------------------------------

/// Wraps any [`IrSource`] so that every lineage already present in a
/// [`PersistenceStore`] is served from the local store instead of the
/// wrapped source, and everything else falls through unchanged.
///
/// # Why a decorator, and why it filters rather than pre-filtering descriptors
///
/// The alternative — teaching `ProducerSource` (or its caller,
/// `Engine::start_with_versions`) to drop descriptors for already-persisted
/// lineages before construction — would work for that one concrete source but
/// leaves every *other* `IrSource` implementation (fixtures, a future
/// sync-derived source) to reinvent the same check. Filtering the *event
/// stream* instead makes "a persisted lineage wins" a property of
/// `PersistedSource` alone, so wrapping is the only integration point any
/// current or future source needs.
///
/// The cost is that the wrapped source may still attempt (and, for a deleted
/// source tree, fail) a package `PersistedSource` is about to shadow anyway —
/// see `load`'s doc comment for why that is an accepted, bounded cost rather
/// than something worth a second integration seam to avoid.
///
/// `store: None` (no `ir_repo_root` configured) makes `load` a direct,
/// zero-overhead pass-through to the wrapped source — this type costs nothing
/// for a host that never opts into local persistence.
pub(crate) struct PersistedSource<S> {
    store: Option<PersistenceStore>,
    inner: S,
}

impl<S: IrSource> PersistedSource<S> {
    /// `root: None` — persistence disabled, `load` is a transparent
    /// pass-through. `root: Some(path)` — the local store at `path` is
    /// consulted first on every `load` call.
    pub(crate) fn new(root: Option<PathBuf>, inner: S) -> Self {
        Self {
            store: root.map(PersistenceStore::new),
            inner,
        }
    }
}

impl<S: IrSource> IrSource for PersistedSource<S> {
    fn describe(&self) -> SourceDescriptor {
        self.inner.describe()
    }

    /// Replay everything the local store already holds, then drive the
    /// wrapped source with its events filtered to drop any lineage already
    /// replayed.
    ///
    /// # Why the wrapped source still runs for shadowed lineages
    ///
    /// `LoadRequest::filter` exists but is documented as not yet used by any
    /// in-tree `IrSource`, and threading a second, persistence-derived filter
    /// through every implementation would duplicate exactly the logic this
    /// type exists to centralise. So the wrapped source runs unfiltered and
    /// its *output* is filtered instead: a `Discovered`/`Progress`/`Ready`/
    /// `Failed` event naming an already-replayed lineage is dropped before it
    /// reaches the caller. This is what makes
    /// `ir_survives_a_restart_after_the_sources_are_deleted`
    /// (`tests/local_persistence.rs`) pass rather than panic: the wrapped
    /// `ProducerSource` genuinely attempts to compile the deleted source tree
    /// and genuinely fails, but that `Failed` event is swallowed here before
    /// `drive_load` (which panics on any `LoadFailed` in the test harness)
    /// ever sees it. The producer's wasted attempt is bounded by the
    /// `IrSource` contract itself ("must emit `Failed`, not hang").
    fn load(&self, req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        let Some(store) = self.store.clone() else {
            return self.inner.load(req);
        };
        let inner_stream = self.inner.load(req);

        stream::once(async move {
            let materialized: Vec<(Arc<PackageView>, Option<String>)> =
                tokio::task::spawn_blocking(move || store.materialize_all())
                    .await
                    // A panic inside `materialize_all` (it should never panic —
                    // every fallible step returns `Result` — but `spawn_blocking`
                    // itself can be cancelled/aborted) degrades to "nothing
                    // persisted" rather than poisoning the whole load stream.
                    .unwrap_or_default();

            let mut already = HashSet::with_capacity(materialized.len());
            let mut replay: Vec<Result<LoadEvent, SourceError>> =
                Vec::with_capacity(materialized.len() * 2);
            for (package, version) in materialized {
                let lineage = package.lineage().clone();
                already.insert(lineage.clone());
                replay.push(Ok(LoadEvent::Discovered {
                    hint: PackageHint {
                        display_name: lineage.name.as_str().to_owned(),
                        ecosystem: lineage.ecosystem.as_str().to_owned(),
                        version,
                    },
                    lineage,
                }));
                replay.push(Ok(LoadEvent::Ready { package }));
            }

            let filtered_inner = inner_stream.filter(move |event| {
                let keep = match event {
                    Ok(
                        LoadEvent::Discovered { lineage, .. }
                        | LoadEvent::Progress { lineage, .. }
                        | LoadEvent::Failed { lineage, .. },
                    ) => !already.contains(lineage),
                    Ok(LoadEvent::Ready { package }) => !already.contains(package.lineage()),
                    Err(_) => true,
                    // `LoadEvent` is `#[non_exhaustive]` for callers outside
                    // this crate, but `persistence.rs` is inside it, so this
                    // match is already exhaustive over every variant that
                    // exists today; a future variant added from within this
                    // crate will fail to compile here rather than silently
                    // falling through unfiltered.
                };
                std::future::ready(keep)
            });

            stream::iter(replay).chain(filtered_inner)
        })
        .flatten()
        .boxed()
    }
}
