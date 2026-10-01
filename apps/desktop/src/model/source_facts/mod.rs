//! Facts read from a package's source on disk: who wrote it, what edition
//! it is, whether it runs code when you build it, where it names the
//! network, files, programs, the environment and C, how many lines it has
//! and how many everything beneath it has, what each feature switches on,
//! when each release was published, and what each module says about itself.
//!
//! **These are not index facts.** The index knows a package's names and
//! relations; this reads the unpacked crate next to it (a registry crate in
//! `~/.cargo/registry/src`, or a local project's own tree), with the rules
//! of the design board's extractors, and everything the page shows from it
//! is labelled "read from its source". A package with no source on disk, or
//! one that is not a Cargo package, reads as [`Reading::Absent`] and the
//! page says so; nothing is invented.
//!
//! The read happens off the UI thread the first time a page asks
//! ([`reading`]), is cached per package for the process, and redraws the
//! windows once when it lands.

pub mod docs;
pub mod manifest;
pub mod registry;
pub mod scan;

pub use facet::folio::berg::Basis;
use crate::model::pages::PackageRef;
use gpui::{App, Global, SharedString};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One package in the weight iceberg.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// The crate's name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Its lines of code.
    pub sloc: usize,
    /// How many steps below the package it first appears.
    pub layer: usize,
    /// The block it was first reached through (an index into the blocks).
    pub parent: Option<usize>,
    /// What it depends on, among the blocks (indices).
    pub deps: Vec<usize>,
}

/// The weight of a package: its own lines above the waterline, everything
/// it pulls in beneath, one layer per step down.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Berg {
    /// The package's own lines of code.
    pub own: usize,
    /// Everything beneath, by first reach (breadth first).
    pub blocks: Vec<Block>,
    /// Their total lines.
    pub below: usize,
    /// Dependencies whose source is not on this machine, so are not counted.
    pub missing: usize,
    /// What the weight was measured under.
    pub basis: Basis,
}

impl Berg {
    /// Every block under `index` (its keel), the block itself excluded.
    #[must_use]
    pub fn keel(&self, index: usize) -> Vec<usize> {
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![index];
        let mut out = Vec::new();
        while let Some(at) = stack.pop() {
            for dep in &self.blocks[at].deps {
                if !seen[*dep] && *dep != index {
                    seen[*dep] = true;
                    out.push(*dep);
                    stack.push(*dep);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// Everything read from one package's source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFacts {
    /// The directory it was read from.
    pub root: PathBuf,
    /// Its `Cargo.toml`.
    pub manifest: manifest::Manifest,
    /// What its source does.
    pub scan: scan::Scan,
    /// Every published release the index cache knows (empty for a local project).
    pub releases: Vec<registry::Published>,
    /// The weight.
    pub berg: Berg,
    /// What each optional dependency weighs, in lines (`None`: its source is not here).
    pub dependency_lines: Vec<(String, Option<usize>)>,
    /// Its public names, by module, as the source spells them.
    pub modules: Vec<docs::Module>,
    /// What each module says about itself (by module key).
    pub docs: HashMap<String, docs::Doc>,
}

impl SourceFacts {
    /// The public names of one module, when the source scan found it.
    #[must_use]
    pub fn module(&self, path: &str) -> Option<&docs::Module> {
        self.modules.iter().find(|m| m.path == path)
    }
}

/// What a page asking for a package's source facts gets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reading {
    /// The read is under way.
    Reading,
    /// There is nothing to read, and why.
    Absent(SharedString),
    /// Read.
    Ready(Arc<SourceFacts>),
}

/// The Cargo package's directory: a local project's own root, or the
/// unpacked registry release.
fn root_of(package: &PackageRef) -> Option<PathBuf> {
    if package.is_local() {
        let path = PathBuf::from(package.as_str());
        return path.join("Cargo.toml").is_file().then_some(path);
    }
    let text = package.as_str();
    let rest = text.strip_prefix("pkg:cargo/")?;
    let (name, version) = rest.split_once('@')?;
    let version = version.split(['?', '#']).next().unwrap_or(version);
    let name = name.rsplit('/').next().unwrap_or(name);
    registry::source_of(name, version)
}

/// Whether `root` is a crate unpacked in a cargo registry.
fn in_registry(root: &Path) -> bool {
    let parts: Vec<_> = root.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    parts.windows(2).any(|w| w[0] == "registry" && w[1] == "src")
}

/// The lock file above a project (within four levels), when it has one.
fn lock_above(root: &Path) -> Option<toml::Table> {
    let mut at = root.to_path_buf();
    for _ in 0..5 {
        if let Ok(text) = std::fs::read_to_string(at.join("Cargo.lock"))
            && let Ok(table) = text.parse::<toml::Table>()
        {
            return Some(table);
        }
        if !at.pop() {
            break;
        }
    }
    None
}

/// One package of a lock file.
#[derive(Clone, Debug)]
struct LockPackage {
    version: String,
    /// `"name version"` or `"name"` entries.
    deps: Vec<String>,
    /// No `source`: a crate of the project itself.
    origin: manifest::Origin,
}

/// `name ->` its entries in a lock file.
type Lock = HashMap<String, Vec<LockPackage>>;

fn lock_table(lock: &toml::Table) -> Lock {
    let mut out: Lock = HashMap::new();
    for package in lock.get("package").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_table) {
        let (Some(name), Some(version)) = (package.get("name").and_then(toml::Value::as_str), package.get("version").and_then(toml::Value::as_str)) else {
            continue;
        };
        let deps = package.get("dependencies").and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).map(str::to_owned).collect();
        out.entry(name.to_owned()).or_default().push(LockPackage { version: version.to_owned(), deps, origin: if package.get("source").is_none() { manifest::Origin::Path } else { manifest::Origin::Registry } });
    }
    out
}

/// What a package needs.
#[derive(Clone, Debug)]
struct Want {
    /// The package.
    package: String,
    /// The version requirement (`=1.2.3` when a lock pins it).
    requirement: String,
    /// Whether its own default features are on.
    defaults: manifest::DefaultFeatures,
}

/// What the lock says `name` at `version` builds (exact: every release is
/// pinned), path crates left out; `None` when the lock has no such entry.
fn locked_wants(lock: &Lock, name: &str, version: &str) -> Option<Vec<Want>> {
    let entry = lock.get(name)?.iter().find(|p| p.version == version)?;
    Some(
        entry
            .deps
            .iter()
            .filter_map(|dep| {
                let mut parts = dep.split_whitespace();
                let dep_name = parts.next()?;
                let candidates = lock.get(dep_name)?;
                let chosen = match parts.next() {
                    Some(v) => candidates.iter().find(|p| p.version == v)?,
                    None => candidates.first()?,
                };
                (chosen.origin == manifest::Origin::Registry).then(|| Want { package: dep_name.to_owned(), requirement: format!("={}", chosen.version), defaults: manifest::DefaultFeatures::On })
            })
            .collect(),
    )
}

/// What a manifest compiles on this machine (see [`manifest::Manifest::compiled`]).
fn manifest_wants(manifest: &manifest::Manifest, defaults: manifest::DefaultFeatures) -> Vec<Want> {
    manifest.compiled(defaults).into_iter().filter(|d| d.origin == manifest::Origin::Registry).map(|d| Want { package: d.package.clone(), requirement: d.req.clone(), defaults: d.default_features }).collect()
}

/// The weight iceberg of the crate `name` at `version` whose direct needs
/// are `direct`: breadth first, each package once. What a package depends
/// on is what the lock says it builds (exact) when there is one; without,
/// what its own manifest compiles here with default features (never
/// test-only or other-target dependencies). Which release of each: a
/// resolver's choice in `hints`, else the lock, else the newest unpacked.
fn berg(name: &str, version: &str, own: usize, direct: &[Want], hints: &HashMap<String, String>, lock: Option<&Lock>, basis: Basis) -> Berg {
    const LAYERS: usize = 7;
    const LIMIT: usize = 600;
    let mut blocks: Vec<Block> = Vec::new();
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut missing = 0usize;
    // Each frontier entry: (block index or root, its dependency needs).
    let mut frontier: Vec<(Option<usize>, Vec<Want>)> = vec![(None, direct.to_vec())];
    let mut layer = 0;
    let resolve = |dep: &str, requirement: &str, hint: Option<&str>| -> Option<String> {
        if let Some(packages) = lock.and_then(|lock| lock.get(dep)) {
            let mut fits: Vec<&str> = packages.iter().map(|p| p.version.as_str()).filter(|v| registry::satisfies(requirement, v)).collect();
            fits.sort_by(|a, b| facet::marks::semver::cmp(a, b));
            if let Some(best) = fits.last() {
                return Some((*best).to_owned());
            }
        }
        registry::pick(dep, requirement, hint)
    };
    let root_key = (name.to_owned(), version.to_owned());
    while !frontier.is_empty() && layer < LAYERS && blocks.len() < LIMIT {
        let mut next: Vec<(Option<usize>, Vec<Want>)> = Vec::new();
        for (parent, deps) in std::mem::take(&mut frontier) {
            for Want { package: dep_name, requirement, defaults } in deps {
                let hint = hints.get(&dep_name).map(String::as_str).filter(|_| parent.is_none());
                let Some(dep_version) = resolve(&dep_name, &requirement, hint) else {
                    missing += 1;
                    continue;
                };
                let key = (dep_name.clone(), dep_version.clone());
                if key == root_key {
                    continue;
                }
                let at = if let Some(at) = index.get(&key) {
                    *at
                } else {
                    let Some(sloc) = registry::sloc_of(&dep_name, &dep_version) else {
                        missing += 1;
                        continue;
                    };
                    let at = blocks.len();
                    blocks.push(Block { name: dep_name.clone(), version: dep_version.clone(), sloc, layer, parent, deps: Vec::new() });
                    index.insert(key, at);
                    // What it depends on, for the next layer.
                    let below = lock.and_then(|lock| locked_wants(lock, &dep_name, &dep_version)).unwrap_or_else(|| {
                        registry::source_of(&dep_name, &dep_version).and_then(|dir| manifest::read(&dir)).map(|m| manifest_wants(&m, defaults)).unwrap_or_default()
                    });
                    next.push((Some(at), below));
                    at
                };
                if let Some(parent) = parent
                    && parent != at
                    && !blocks[parent].deps.contains(&at)
                {
                    blocks[parent].deps.push(at);
                }
            }
        }
        frontier = next;
        layer += 1;
    }
    let below = blocks.iter().map(|b| b.sloc).sum();
    Berg { own, blocks, below, missing, basis }
}

/// Reads the package at `root` (its name and version are the manifest's).
/// `hints` are versions a resolver already chose for its direct dependencies.
#[must_use]
pub fn read(root: &Path, hints: &HashMap<String, String>, project: Option<&Path>) -> Option<SourceFacts> {
    let manifest = manifest::read(root)?;
    let scan = scan::scan(root, &manifest.lib);
    let releases = registry::releases(&manifest.name);
    // A project's lock file is exact about what it builds. A registry crate
    // ships a lock of its own (its tests' world), which says nothing about
    // what a consumer builds, so a crate is read against the active
    // project's lock instead.
    let registry_crate = in_registry(root);
    let lock = if registry_crate { project.and_then(lock_above) } else { lock_above(root).or_else(|| project.and_then(lock_above)) }.map(|lock| lock_table(&lock));
    let locked = lock.as_ref().filter(|_| registry_crate).and_then(|lock| locked_wants(lock, &manifest.name, &manifest.version));
    let basis = if lock.is_some() && (locked.is_some() || !registry_crate) { Basis::Lock } else { Basis::Defaults };
    let direct: Vec<Want> = locked.unwrap_or_else(|| manifest_wants(&manifest, manifest::DefaultFeatures::On));
    let berg = berg(&manifest.name, &manifest.version, scan.sloc, &direct, hints, lock.as_ref(), basis);
    let dependency_lines = manifest
        .dependencies
        .iter()
                .filter(|d| d.need == manifest::Need::Optional)
        .map(|d| {
            let version = registry::pick(&d.package, &d.req, hints.get(&d.package).map(String::as_str));
            (d.key.clone(), version.and_then(|v| registry::sloc_of(&d.package, &v)))
        })
        .collect();
    let lib_root = {
        let dir = root.join(&manifest.lib).parent().map(Path::to_path_buf).unwrap_or_else(|| root.join("src"));
        if dir.is_dir() { dir } else { root.join("src") }
    };
    let modules = docs::items(&lib_root, root);
    let root_file = {
        let lib = root.join(&manifest.lib);
        if lib.is_file() { lib } else { root.join("src").join("main.rs") }
    };
    let docs = modules
        .iter()
        .filter_map(|m| docs::module_doc(&lib_root, &root_file, &m.path).map(|doc| (m.path.clone(), doc)))
        .collect();
    Some(SourceFacts { root: root.to_path_buf(), manifest, scan, releases, berg, dependency_lines, modules, docs })
}

enum Entry {
    Reading,
    Done(Reading),
}

/// One package's entry, and the project it was read against (`None`:
/// installed by a test or capture harness, good for any project).
struct Slot {
    project: Option<Option<PathBuf>>,
    entry: Entry,
}

#[derive(Default)]
struct Service {
    entries: HashMap<PackageRef, Slot>,
}

impl Global for Service {}

/// What the source on disk says about `package`, read against `project`'s
/// lock file (the reader's active project). The first ask starts the read
/// (off the UI thread) and returns [`Reading::Reading`]; the windows redraw
/// when it lands. Asking again with another project reads again.
pub fn reading(package: &PackageRef, hints: &HashMap<String, String>, project: Option<&Path>, cx: &mut App) -> Reading {
    let wanted = project.map(Path::to_path_buf);
    if let Some(slot) = cx.default_global::<Service>().entries.get(package)
        && slot.project.as_ref().is_none_or(|read| *read == wanted)
    {
        return match &slot.entry {
            Entry::Reading => Reading::Reading,
            Entry::Done(done) => done.clone(),
        };
    }
    let Some(root) = root_of(package) else {
        let why: SharedString = if package.is_local() || package.as_str().starts_with("pkg:cargo/") {
            "Its source is not on this machine.".into()
        } else {
            "Source facts are read for Cargo packages.".into()
        };
        let done = Reading::Absent(why);
        cx.default_global::<Service>().entries.insert(package.clone(), Slot { project: Some(wanted), entry: Entry::Done(done.clone()) });
        return done;
    };
    cx.default_global::<Service>().entries.insert(package.clone(), Slot { project: Some(wanted.clone()), entry: Entry::Reading });
    #[cfg(test)]
    {
        let _ = &root;
        let _ = hints;
    }
    #[cfg(not(test))]
    {
        let (key, hints) = (package.clone(), hints.clone());
        let read_for = wanted.clone();
        let work = cx.background_executor().spawn(async move { read(&root, &hints, read_for.as_deref()) });
        cx.spawn(async move |cx| {
            let facts = work.await;
            let _ = cx.update(|cx| {
                let done = facts.map_or_else(|| Reading::Absent("Its manifest could not be read.".into()), |facts| Reading::Ready(Arc::new(facts)));
                cx.default_global::<Service>().entries.insert(key, Slot { project: Some(wanted), entry: Entry::Done(done) });
                cx.refresh_windows();
            });
        })
        .detach();
    }
    Reading::Reading
}

/// Installs what a test or a capture harness has already read for a
/// package, so the page draws it without starting a read.
#[doc(hidden)]
pub fn install(package: &PackageRef, reading: Reading, cx: &mut App) {
    cx.default_global::<Service>().entries.insert(package.clone(), Slot { project: None, entry: Entry::Done(reading) });
}

#[cfg(test)]
mod tests;
