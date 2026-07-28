//! Workspace load via `ra_ap_load_cargo`.
//!
//! Ported almost verbatim from
//! `workspace/compiler/compile/rust/ra/load.rs`.  The only structural change is
//! that the output carries `document_private` so that `lower_workspace` can
//! pass it to `LowerCtx` without threading it through another parameter.

use std::{path::Path, thread};

use ra_ap_ide_db::{RootDatabase, prime_caches};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_paths::AbsPathBuf;
use ra_ap_proc_macro_api::ProcMacroClient;
use ra_ap_project_model::{CargoConfig, ProjectManifest, ProjectWorkspace, RustLibSource};
use ra_ap_vfs::Vfs;
use tracing::{debug, warn};

use crate::error::RustProducerError;

// ── Policy types ──────────────────────────────────────────────────────────────

/// Configuration for a single workspace load.
#[derive(Debug, Clone)]
pub struct ExtractConfig {
    pub offline: bool,
    pub run_build_scripts: bool,
    pub num_threads: usize,
}

impl ExtractConfig {
    /// Defaults matched to the rustdoc path (offline, build scripts on).
    pub fn for_extract() -> Self {
        Self {
            offline: true,
            run_build_scripts: true,
            num_threads: thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(1),
        }
    }
}

// ── LoadedWorkspace ───────────────────────────────────────────────────────────

/// One loaded analysis session.
///
/// Owns the salsa `RootDatabase`, the `Vfs`, the `ProjectWorkspace` (for
/// package/target metadata), and the proc-macro client handle.
///
/// # Oracle lifetime contract
///
/// `nudox_producer::produce` keeps this value alive for the entire duration of
/// `Producer::lower`, so it is safe for `lower` to borrow HIR out of `db`.
/// The producer does **not** need a self-referential struct here because the
/// borrow occurs inside the `lower()` call, not stored anywhere.
pub struct LoadedWorkspace {
    pub db: RootDatabase,
    pub vfs: Vfs,
    /// Kept for package/target metadata.
    pub ws: ProjectWorkspace,
    /// Whether private items should be lowered.
    pub document_private: bool,
    /// The name of the package this workspace was loaded for.
    ///
    /// Carried here rather than on the producer value because `Producer::lower`
    /// receives only the oracle — see the note on [`super::load`].
    pub package_name: String,
    _proc_macro: Option<ProcMacroClient>,
}

// ── Public load function ──────────────────────────────────────────────────────

/// Load the Cargo workspace at `root` into HIR.
///
/// - Discovers the manifest (or workspace root).
/// - Optionally runs build scripts for `OUT_DIR` items.
/// - Primes type caches in parallel before returning.
pub(crate) fn load(
    root: &Path,
    package_name: &str,
    document_private: bool,
) -> Result<LoadedWorkspace, RustProducerError> {
    let cfg = ExtractConfig::for_extract();

    let cargo_config = build_cargo_config(&cfg);
    let load_config = build_load_config(&cfg);
    let abs = abs_path(root)?;

    let manifest = ProjectManifest::discover_single(abs.as_ref())
        .map_err(|e| RustProducerError::Load(e.to_string()))?;

    let mut ws = ProjectWorkspace::load(manifest, &cargo_config, &|msg| {
        debug!(target: "ra_load", "{msg}");
    })
    .map_err(|e| RustProducerError::Load(e.to_string()))?;

    if cfg.run_build_scripts {
        match ws.run_build_scripts(&cargo_config, &|msg| {
            debug!(target: "ra_load", "{msg}");
        }) {
            Ok(scripts) => {
                if let Some(err) = scripts.error() {
                    warn!(error = %err, "build scripts reported errors; OUT_DIR items may be missing");
                }
                ws.set_build_scripts(scripts);
            }
            Err(e) => {
                warn!(error = %e, "build scripts failed; continuing without OUT_DIR");
            }
        }
    }

    // Clone so we keep `ws` for package metadata after `load_workspace` consumes a copy.
    let (db, vfs, proc_macro) =
        load_workspace(ws.clone(), &cargo_config.extra_env, &load_config)
            .map_err(|e| RustProducerError::Load(e.to_string()))?;

    if proc_macro.is_none() {
        warn!("proc-macro server unavailable; macro-generated items will be missing");
    }

    // Explicit prime (prefill_caches is false so we can catch Cancelled cleanly).
    ra_ap_base_db::salsa::Cancelled::catch(|| {
        prime_caches::parallel_prime_caches(&db, cfg.num_threads, &|_| {});
    })
    .map_err(|_| RustProducerError::Cancelled)?;

    Ok(LoadedWorkspace {
        db,
        vfs,
        ws,
        document_private,
        package_name: package_name.to_owned(),
        _proc_macro: proc_macro,
    })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn build_cargo_config(cfg: &ExtractConfig) -> CargoConfig {
    let mut cargo = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        all_targets: false,
        set_test: false,
        no_deps: false,
        ..CargoConfig::default()
    };
    if cfg.offline {
        cargo.extra_env.insert("CARGO_NET_OFFLINE".into(), Some("true".into()));
        cargo.extra_args.push("--offline".into());
    }
    cargo
}

fn build_load_config(cfg: &ExtractConfig) -> LoadCargoConfig {
    LoadCargoConfig {
        load_out_dirs_from_check: cfg.run_build_scripts,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: cfg.num_threads.max(1),
        proc_macro_processes: 1,
    }
}

fn abs_path(root: &Path) -> Result<AbsPathBuf, RustProducerError> {
    let path = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| RustProducerError::Load(e.to_string()))?
            .join(root)
    };
    Ok(AbsPathBuf::assert_utf8(path))
}
