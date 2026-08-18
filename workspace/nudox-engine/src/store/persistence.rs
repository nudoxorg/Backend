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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt as _};
use nudox_ir::change::PackageLineageId;
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

    /// Record a **freshly produced** generation.
    ///
    /// Never panics and never blocks a load on failure: an error opening or
    /// writing the local repository is logged at `warn` and swallowed. The
    /// package the caller just produced is still resident in memory for this
    /// session — it simply will not survive the next restart, which is
    /// exactly the pre-existing behaviour for a host that never sets
    /// `ir_repo_root` at all.
    ///
    /// Blocking: performs synchronous filesystem and `sanakirja` I/O. Callers
    /// on an async runtime must run this via `spawn_blocking` (see
    /// `runtime::load::drive_load`'s call site).
    pub(crate) fn record(&self, lineage: &PackageLineageId, version: Option<&str>, view: &IrView) {
        if let Err(error) = self.try_record(lineage, version, view) {
            tracing::warn!(
                package = %lineage,
                %error,
                "local IR persistence: failed to record this generation; it will not survive a restart"
            );
        }
    }

    fn try_record(
        &self,
        lineage: &PackageLineageId,
        version: Option<&str>,
        view: &IrView,
    ) -> Result<(), PersistenceError> {
        let dir = self.package_root(lineage);
        std::fs::create_dir_all(&dir).map_err(|source| PersistenceError::Io {
            path: dir.clone(),
            source,
        })?;
        write_metadata(&dir, lineage, version)?;

        let repo = ir_vcs::IrRepository::open(&dir, lineage.clone(), CHANNEL).map_err(|source| {
            PersistenceError::Repository {
                lineage: lineage.clone(),
                path: dir.clone(),
                source,
            }
        })?;
        let table = ir_vcs::raise::raise_view(view);
        repo.record_generation(&table)
            .map_err(|source| PersistenceError::Repository {
                lineage: lineage.clone(),
                path: dir.clone(),
                source,
            })?;
        Ok(())
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

    fn try_materialize_one(
        &self,
        dir: &Path,
    ) -> Result<Option<(Arc<PackageView>, Option<String>)>, PersistenceError> {
        let Some(metadata) = read_metadata(dir)? else {
            // No sidecar — not a lineage directory this store wrote (or a
            // write that crashed before the sidecar landed). Skip quietly:
            // this is the same "nothing here yet" case as a missing root,
            // just one level down.
            return Ok(None);
        };

        let repo =
            ir_vcs::IrRepository::open(dir, metadata.lineage.clone(), CHANNEL).map_err(|source| {
                PersistenceError::Repository {
                    lineage: metadata.lineage.clone(),
                    path: dir.to_path_buf(),
                    source,
                }
            })?;
        let table = repo
            .materialize()
            .map_err(|source| PersistenceError::Repository {
                lineage: metadata.lineage.clone(),
                path: dir.to_path_buf(),
                source,
            })?;
        if table.is_empty() {
            // A repository was opened here (e.g. `open_or_create_channel`
            // ran) but nothing was ever successfully recorded into it — an
            // interrupted first write, most likely. Nothing to serve.
            return Ok(None);
        }

        let entries = table
            .live_entries_with_parent()
            .map(|(id, payload, parent)| (id, payload.clone(), parent));
        let bodies = std::collections::BTreeMap::default();
        let view = ir_vcs::lower::build_ir_view(metadata.lineage.clone(), entries, &bodies);
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
