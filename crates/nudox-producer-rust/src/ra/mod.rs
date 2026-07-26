//! rust-analyzer in-process oracle: workspace loading + HIR walk.

pub mod ctx;
pub mod docs;
pub mod function;
pub mod generics;
pub mod item;
pub mod loaded;
pub mod source;
pub mod ty;
pub mod walk;

use std::collections::VecDeque;

use ra_ap_base_db::salsa::Cancelled;
use ra_ap_hir::{Crate, attach_db};
use ra_ap_project_model::{CargoWorkspace, ProjectWorkspaceKind, TargetKind};
use tracing::{debug, info, warn};

use nudox_ir::lower::Lowering;
use nudox_producer::{PackageSource, ProducerError};

use crate::RaId;
use crate::error::RustProducerError;
use self::ctx::LowerCtx;
use self::loaded::LoadedWorkspace;

// ── Public entry points ───────────────────────────────────────────────────────

/// Load the Cargo workspace at `src.root` into a [`LoadedWorkspace`].
pub(crate) fn load(
    src: &PackageSource,
    document_private: bool,
) -> Result<LoadedWorkspace, RustProducerError> {
    loaded::load(src.root(), document_private)
}

/// Walk every documented crate in the loaded workspace and emit declarations
/// into `out`.
///
/// This is the bridge between the in-process oracle result and the flat
/// `Lowering` sink.  It mirrors the structure of the old `generate_ir` /
/// `lower_all_packages` in `workspace/compiler/compile/rust/ra/mod.rs` but
/// emits into `Lowering<RaId>` instead of building a `Vec<ir::kind::Entry>`.
pub(crate) fn lower_workspace(
    oracle: &LoadedWorkspace,
    root_package_name: &str,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let package_names = documented_package_names(
        &oracle.ws,
        root_package_name,
        oracle.document_private,
    );
    if package_names.is_empty() {
        return Err(RustProducerError::Load(format!(
            "no documented packages found for `{root_package_name}`"
        )));
    }
    debug!(?package_names, "documented package set");

    // `attach_db` installs the salsa TLS slot required for HIR display / type
    // probing.  Everything inside the closure is single-threaded.
    let result = attach_db(&oracle.db, || {
        lower_all_packages_into(&oracle.db, &package_names, oracle.document_private, out)
    });

    // `attach_db` returns `Result<T, E>` where `E = Box<dyn Any>` (salsa
    // `Cancelled`); it re-panics non-Cancelled payloads.
    result.map_err(|_| RustProducerError::Cancelled)
}

// ── Package enumeration ───────────────────────────────────────────────────────

/// Enumerate documented packages: root always; when `direct_repo` and root
/// has no lib target, BFS local member deps that do.
///
/// Mirrors `documented_package_names` from the old `ra/mod.rs`.
fn documented_package_names(
    ws: &ra_ap_project_model::ProjectWorkspace,
    root_package_name: &str,
    direct_repo: bool,
) -> Vec<String> {
    let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind else {
        return vec![root_package_name.to_owned()];
    };

    let Some(root_pkg) = cargo.packages().find(|&p| cargo[p].name == root_package_name) else {
        warn!(%root_package_name, "package not in CargoWorkspace; matching by name only");
        return vec![root_package_name.to_owned()];
    };

    let mut documented = vec![cargo[root_pkg].name.clone()];
    if !direct_repo || package_has_library(cargo, root_pkg) {
        return documented;
    }

    // Binary-only root in direct-repo mode: BFS local member libs.
    let mut seen = std::collections::BTreeSet::from([root_pkg]);
    let mut queue = VecDeque::from([root_pkg]);

    while let Some(pkg) = queue.pop_front() {
        for dep in &cargo[pkg].dependencies {
            if !seen.insert(dep.pkg) {
                continue;
            }
            queue.push_back(dep.pkg);
            let data = &cargo[dep.pkg];
            if (data.is_member || data.is_local) && package_has_library(cargo, dep.pkg) {
                documented.push(data.name.clone());
            }
        }
    }
    documented
}

fn package_has_library(cargo: &CargoWorkspace, pkg: ra_ap_project_model::Package) -> bool {
    cargo[pkg]
        .targets
        .iter()
        .any(|&t| matches!(cargo[t].kind, TargetKind::Lib { .. }))
}

// ── Crate enumeration ─────────────────────────────────────────────────────────

/// Find local `hir::Crate`s whose display/canonical name matches a package in
/// `package_names`.
///
/// Mirrors `find_local_crates` from the old `ra/mod.rs`.
fn find_local_crates(
    db: &ra_ap_ide_db::RootDatabase,
    package_names: &[String],
) -> Vec<Crate> {
    let mut matched = Vec::new();
    for krate in Crate::all(db) {
        if !krate.origin(db).is_local() {
            continue;
        }
        let Some(dn) = krate.display_name(db) else {
            continue;
        };
        if package_names.iter().any(|pkg| crate_name_matches(pkg, &dn)) {
            matched.push(krate);
        }
    }
    matched
}

fn crate_name_matches(package: &str, dn: &ra_ap_base_db::CrateDisplayName) -> bool {
    let display = dn.to_string();
    let canonical = dn.canonical_name().as_str();
    package == canonical
        || package == display
        || package.replace('-', "_") == display
        || package.replace('_', "-") == canonical
}

// ── Per-package lowering ──────────────────────────────────────────────────────

fn lower_all_packages_into(
    db: &ra_ap_ide_db::RootDatabase,
    package_names: &[String],
    document_private: bool,
    out: &mut Lowering<RaId>,
) -> Result<(), RustProducerError> {
    let crates = find_local_crates(db, package_names);
    if crates.is_empty() {
        return Err(RustProducerError::Load(format!(
            "no local hir::Crate matching packages {package_names:?}"
        )));
    }

    for krate in crates {
        let crate_name = krate
            .display_name(db)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "<anon>".into());
        debug!(%crate_name, "lowering crate");

        // Catch Cancelled so it propagates out of attach_db correctly.
        // Non-Cancelled panics are caught per-item in `walk`.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut ctx = LowerCtx::new(db, krate, document_private);
            ctx.collect_aliases();
            walk::lower_crate(&mut ctx, out)
        }));

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(payload) => {
                if payload.downcast_ref::<Cancelled>().is_some() {
                    return Err(RustProducerError::Cancelled);
                }
                // A non-Cancelled panic at the crate level is a producer bug;
                // log and continue rather than aborting the whole package.
                warn!(%crate_name, "crate lowering panicked unexpectedly; skipping crate");
            }
        }
    }

    info!(packages = package_names.len(), "RA lowering complete");
    Ok(())
}
