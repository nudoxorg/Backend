//! In-process rust-analyzer producer (default).
//!
//! This is the primary Rust path. The rustdoc producer remains as a temporary
//! fallback until P3 (`NUDOX_RUST_PRODUCER=rustdoc`).

#[allow(dead_code)] // walk/item/ty stubs land fully in later P1 agents
mod ctx;
#[allow(dead_code)]
mod docs;
#[allow(dead_code)]
mod function;
#[allow(dead_code)]
mod generics;
#[allow(dead_code)]
mod item;
mod load;
#[allow(dead_code)]
mod source;
#[allow(dead_code)]
mod ty;
mod walk;

use std::{collections::VecDeque, path::Path};

use ir::{entry::Index, pipeline::Ir};
use ra_ap_hir::{Crate, attach_db};
use ra_ap_project_model::{CargoWorkspace, ProjectWorkspaceKind, TargetKind};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use tracing::{debug, info, warn};

use super::error::Package;

// Re-exports for later P1 agents.
#[allow(unused_imports)]
pub(crate) use self::load::{ExtractConfig, LoadedWorkspace, ProbeTier, ProcMacroPolicy, load};

use self::ctx::LowerCtx;
use self::walk::lower_crate;

/// Env flag for the temporary dual-path switch (removed at P3).
pub const PRODUCER_ENV: &str = "NUDOX_RUST_PRODUCER";

/// True when the legacy rustdoc path is requested (`NUDOX_RUST_PRODUCER=rustdoc`,
/// case-insensitive). Unset or any other value keeps the default RA producer.
pub fn rustdoc_selected() -> bool {
	std::env::var_os(PRODUCER_ENV)
		.map(|v| v.eq_ignore_ascii_case("rustdoc"))
		.unwrap_or(false)
}

/// RA entry point — same signature as the rustdoc `generate_ir`.
///
/// One workspace load, then for each documented local package: find `hir::Crate`
/// → walk → merge into a single `Index` + source map.
pub fn generate_ir(
	root: &Path,
	name: &str,
	version: &Version,
	document_private: bool,
) -> std::result::Result<(Index, HashMap<String, String>), Package> {
	let cfg = ExtractConfig::for_extract(document_private);
	let loaded = load(root, &cfg)?;

	let package_names = documented_package_names(&loaded.ws, name, document_private);
	if package_names.is_empty() {
		return Err(Package::LoadFailed(format!(
			"no documented packages found for root package `{name}`"
		)));
	}
	debug!(?package_names, "documented package set");

	// Lowering may touch next-solver TLS via HirDisplay / type probes.
	let (entries, source_map) = attach_db(&loaded.db, || {
		lower_all_packages(&loaded.db, &package_names, version, document_private)
	})?;

	info!(
		entries = entries.len(),
		sources = source_map.len(),
		packages = package_names.len(),
		"RA IR generation complete"
	);

	Ok((Ir::from_entries(entries).index().into_index(), source_map))
}

fn lower_all_packages(
	db: &ra_ap_ide_db::RootDatabase,
	package_names: &[String],
	version: &Version,
	document_private: bool,
) -> Result<(Vec<ir::kind::Entry>, HashMap<String, String>), Package> {
	let mut all_entries = Vec::new();
	let mut source_map = HashMap::default();

	let crates = find_local_crates(db, package_names, version);
	if crates.is_empty() {
		return Err(Package::LoadFailed(format!(
			"no local hir::Crate matching packages {package_names:?}"
		)));
	}

	for krate in crates {
		let crate_name = krate
			.display_name(db)
			.map(|n| n.to_string())
			.unwrap_or_else(|| "<anon>".into());
		debug!(%crate_name, "lowering crate");

		let mut ctx = LowerCtx::new(db, krate, document_private);
		ctx.collect_aliases();
		// Impl index is rebuilt at end of lower_crate (after module impl walk).

		let (entries, sources) = lower_crate(&mut ctx)?;
		all_entries.extend(entries);
		source_map.extend(sources);
	}

	Ok((all_entries, source_map))
}

/// Local `hir::Crate`s whose display / canonical name matches a package in
/// `package_names`. Prefer a version match when the crate reports one.
fn find_local_crates(
	db: &ra_ap_ide_db::RootDatabase,
	package_names: &[String],
	version: &Version,
) -> Vec<Crate> {
	let version_str = version.to_string();
	let mut matched = Vec::new();

	for krate in Crate::all(db) {
		if !krate.origin(db).is_local() {
			continue;
		}
		let Some(dn) = krate.display_name(db) else {
			continue;
		};
		if !package_names.iter().any(|pkg| crate_name_matches(pkg, &dn)) {
			continue;
		}
		if let Some(v) = krate.version(db)
			&& v != version_str
		{
			// Name match is the primary gate; log version drift for fixtures.
			debug!(
				crate = %dn,
				crate_version = %v,
				want = %version_str,
				"local crate version differs; keeping by name match"
			);
		}
		matched.push(krate);
	}

	// Prefer lib targets: a package may also emit a bin crate with the same name.
	// Keep all for now; duplicate root module lowering is rare for fixtures.
	matched
}

fn crate_name_matches(package: &str, dn: &ra_ap_base_db::CrateDisplayName) -> bool {
	// Display form uses underscores (`odd_duck`); canonical keeps hyphens (`odd-duck`).
	let display = dn.to_string();
	let canonical = dn.canonical_name().as_str();
	package == canonical
		|| package == display
		|| package.replace('-', "_") == display
		|| package.replace('_', "-") == canonical
}

/// Mirror of rustdoc `documented_local_packages`: root always; when
/// `direct_repo` and root has no lib, BFS local member deps that do.
fn documented_package_names(
	ws: &ra_ap_project_model::ProjectWorkspace,
	root_package_name: &str,
	direct_repo: bool,
) -> Vec<String> {
	let ProjectWorkspaceKind::Cargo { cargo, .. } = &ws.kind else {
		return vec![root_package_name.to_string()];
	};

	let Some(root_pkg) = cargo
		.packages()
		.find(|&p| cargo[p].name == root_package_name)
	else {
		// Fall back to the requested name so crate matching can still try.
		warn!(%root_package_name, "package not in CargoWorkspace; matching by name only");
		return vec![root_package_name.to_string()];
	};

	let mut documented = vec![cargo[root_pkg].name.clone()];
	if !direct_repo || package_has_library(cargo, root_pkg) {
		return documented;
	}

	// Binary-only root in direct-repo mode: pull in local member libs via deps.
	let mut seen = std::collections::BTreeSet::from([root_pkg]);
	let mut queue = VecDeque::from([root_pkg]);

	while let Some(pkg) = queue.pop_front() {
		for dep in &cargo[pkg].dependencies {
			if !seen.insert(dep.pkg) {
				continue;
			}
			queue.push_back(dep.pkg);

			let data = &cargo[dep.pkg];
			// Path-local workspace members with a lib target (matches package.rs
			// `manifest_path.starts_with(workspace_root)` + library check).
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
