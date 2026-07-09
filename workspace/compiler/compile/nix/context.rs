//! The Nix producer's orchestrator: parse the static layer, evaluate the
//! dynamic layer best-effort, fuse the two, fold in the builtins reference,
//! and assemble the [`Index`].
//!
//! Degradation is total and silent-but-logged: a flake with no lock, a flake
//! that fails to evaluate, or a plain non-flake `.nix` tree all still produce
//! the full static surface. The dynamic layer only ever *adds* resolution
//! (real output tree, aliases, required flags, packages, options).

use std::path::Path;

use ir::entry::{Index, NudoxPath};
use ir::kind::Entry;
use rustc_hash::FxHashMap as HashMap;

use super::error::Result;
use super::{builtins, eval, item, package, syntax};

/// Discover, statically analyse, evaluate, and lower the Nix flake (or plain
/// `.nix` tree) rooted at `root` in one call — the producer's one-shot entry
/// point, mirroring the Go/Python producers.
pub fn lower_package(root: &Path) -> Result<Index> {
    // 1. Static layer — always runs, needs no inputs, already beats nixdoc.
    let table = syntax::parse_tree(root)?;
    let meta = package::flake_metadata(&table, root);

    // 2. Baseline IR from the static table (private-by-default; the dynamic
    //    pass promotes anything reachable from outputs to public).
    let mut by_path: HashMap<NudoxPath, Entry> = HashMap::default();
    let mut roots: Vec<NudoxPath> = Vec::new();

    let (static_entries, static_roots) = item::lower_static(&table, &meta);
    for (path, entry) in static_entries {
        by_path.insert(path, entry);
    }
    roots.extend(static_roots);

    // 3. Dynamic layer — best-effort. Fused entries override their static
    //    counterparts (same NudoxPath) and carry alias groups + runtime facts.
    match eval::produce(root, &table, &meta) {
        Ok(Some(surface)) => {
            for (path, entry) in surface.entries {
                by_path.insert(path, entry);
            }
            for root_path in surface.roots {
                if !roots.contains(&root_path) {
                    roots.push(root_path);
                }
            }
        }
        Ok(None) => {
            tracing::info!("nix: static-only mode (no evaluable flake outputs at root)");
        }
        Err(error) => {
            tracing::warn!(%error, "nix: dynamic evaluation failed; falling back to static layer");
        }
    }

    // 4. Builtins — a standing synthetic package, the best builtins reference
    //    anywhere, free from `pure_builtins()`.
    let (builtin_entries, builtin_root) = builtins::synthesize();
    for (path, entry) in builtin_entries {
        by_path.insert(path, entry);
    }
    if let Some(r) = builtin_root {
        if !roots.contains(&r) {
            roots.push(r);
        }
    }

    // 5. Wire module membership: every entry becomes a member of its nearest
    //    ancestor module by attrpath prefix.
    wire_members(&mut by_path);

    Ok(Index { root_ids: roots, entries_by_path: by_path })
}

/// The dynamic layer's contribution: fully-lowered IR entries plus any new
/// root paths (output-category modules). Produced by [`eval::produce`].
#[derive(Debug, Default)]
pub struct Surface {
    pub entries: Vec<(NudoxPath, Entry)>,
    pub roots:   Vec<NudoxPath>,
}

/// Populate each `Entry::Module`'s `members` from the set of entries whose
/// path is an immediate child of the module's path.
fn wire_members(by_path: &mut HashMap<NudoxPath, Entry>) {
    use ir::module::Module;

    // Collect (parent, child) relations by path-prefix.
    let paths: Vec<NudoxPath> = by_path.keys().cloned().collect();
    let mut members: HashMap<NudoxPath, Vec<NudoxPath>> = HashMap::default();
    for child in &paths {
        if let Some(parent) = parent_path(child) {
            if by_path.contains_key(&parent) {
                members.entry(parent).or_default().push(child.clone());
            }
        }
    }
    for (parent, mut children) in members {
        children.sort_by(|a, b| path_str(a).cmp(&path_str(b)));
        if let Some(Entry::Module(sym)) = by_path.get_mut(&parent) {
            sym.inner = Module { members: Some(children) };
        }
    }
}

/// The parent path of a local attrpath-based path (`lib/attrsets/mapAttrs` →
/// `lib/attrsets`). Returns `None` for a single-segment or external path.
fn parent_path(path: &NudoxPath) -> Option<NudoxPath> {
    match path {
        NudoxPath::Local(p) => {
            let parent = p.parent()?;
            if parent.as_os_str().is_empty() {
                None
            } else {
                Some(NudoxPath::Local(parent.to_path_buf()))
            }
        }
        NudoxPath::External { .. } => None,
    }
}

fn path_str(path: &NudoxPath) -> String {
    match path {
        NudoxPath::Local(p) => p.display().to_string(),
        NudoxPath::External { path, dependency } => format!("{dependency}:{}", path.display()),
    }
}
