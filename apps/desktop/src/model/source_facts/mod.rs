//! Facts read from a package's source on disk: who wrote it, what edition
//! it is, whether it runs code when you build it, where it names the
//! network, files, programs, the environment and C, how many lines it has
//! and how many everything beneath it has, what each feature switches on,
//! what each module says about itself.
//!
//! **These are not index facts.** The index knows a package's names and
//! relations; this reads the unpacked crate from the selected local registry
//! authority, or a local project's own tree, with the rules
//! of the design board's extractors, and everything the page shows from it
//! is labelled "read from its source". A package with no source on disk, or
//! one that is not a Cargo package, reads as [`Reading::Absent`] with the
//! specific reason the source could not be resolved; nothing is invented.
//!
//! The read happens off the UI thread the first time a page asks
//! ([`reading`]), is cached per package, workspace, resolver hints and
//! registry composition for a short interval so changed source/index facts
//! are re-read, and redraws the windows once when it lands.

pub mod docs;
pub mod manifest;
pub mod registry;
pub mod scan;

use crate::host::registry::CompositionGeneration;
use crate::model::pages::PackageRef;
use crate::runtime::offload::Cancellation;
pub use facet::folio::berg::Basis;
use gpui::{App, Global, SharedString};
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FACTS_TTL: Duration = Duration::from_secs(15);

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
    /// The bounded source-reader pool is full; the visible request will retry
    /// after an obsolete worker exits.
    Waiting,
    /// There is nothing to read, and why.
    Absent(SharedString),
    /// Read.
    Ready(Arc<SourceFacts>),
}

/// Resolve one exact source root using the same authority as acquisition.
/// This runs on the background executor: neither registry discovery nor
/// file access belongs in a render pass.
fn root_of(
    package: &PackageRef,
    composition: Option<&crate::host::registry::Composition>,
) -> Result<PathBuf, SharedString> {
    if package.is_local() {
        let path = PathBuf::from(package.as_str());
        return if path.join("Cargo.toml").is_file() {
            Ok(path)
        } else {
            Err("Its Cargo manifest is not available at this path.".into())
        };
    }
    let release = package
        .release()
        .ok_or_else(|| SharedString::from("Source facts are read for exact Cargo releases."))?;
    let composition = composition.ok_or_else(|| {
        SharedString::from("The selected local Cargo registry authority is not ready yet.")
    })?;
    match composition.source.availability(&release) {
        crate::host::registry::Availability::Unpacked(tree) => Ok(tree),
        crate::host::registry::Availability::Archive(_) => Err(
            "A verified release archive is cached, but its source tree is not unpacked. Use Add to index it.".into(),
        ),
        crate::host::registry::Availability::Download => {
            Err("This release's source is not on this machine.".into())
        }
        crate::host::registry::Availability::Ambiguous { indexes } => Err(format!(
            "This release appears in more than one Cargo registry authority: {}.",
            indexes
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
        crate::host::registry::Availability::UnverifiedArchive(path) => Err(format!(
            "The local archive at {} has no trusted checksum and cannot be read.",
            path.display()
        )
        .into()),
    }
}

fn read_package(
    package: &PackageRef,
    hints: &HashMap<String, String>,
    project: Option<&Path>,
    composition: Option<&crate::host::registry::Composition>,
    cancellation: &Cancellation,
) -> Result<SourceFacts, SharedString> {
    if cancellation.is_cancelled() {
        return Err("This source read was superseded.".into());
    }
    let root = root_of(package, composition)?;
    read_cancellable(&root, hints, project, Some(cancellation))
        .ok_or_else(|| "Its Cargo manifest or source files could not be read.".into())
}

/// Whether `root` is a crate unpacked in a cargo registry.
fn in_registry(root: &Path) -> bool {
    let parts: Vec<_> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts
        .windows(2)
        .any(|w| w[0] == "registry" && w[1] == "src")
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
    for package in lock
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
    {
        let (Some(name), Some(version)) = (
            package.get("name").and_then(toml::Value::as_str),
            package.get("version").and_then(toml::Value::as_str),
        ) else {
            continue;
        };
        let deps = package
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect();
        out.entry(name.to_owned()).or_default().push(LockPackage {
            version: version.to_owned(),
            deps,
            origin: if package.get("source").is_none() {
                manifest::Origin::Path
            } else {
                manifest::Origin::Registry
            },
        });
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
                (chosen.origin == manifest::Origin::Registry).then(|| Want {
                    package: dep_name.to_owned(),
                    requirement: format!("={}", chosen.version),
                    defaults: manifest::DefaultFeatures::On,
                })
            })
            .collect(),
    )
}

/// What a manifest compiles on this machine (see [`manifest::Manifest::compiled`]).
fn manifest_wants(manifest: &manifest::Manifest, defaults: manifest::DefaultFeatures) -> Vec<Want> {
    manifest
        .compiled(defaults)
        .into_iter()
        .filter(|d| d.origin == manifest::Origin::Registry)
        .map(|d| Want {
            package: d.package.clone(),
            requirement: d.req.clone(),
            defaults: d.default_features,
        })
        .collect()
}

/// The weight iceberg of the crate `name` at `version` whose direct needs
/// are `direct`: breadth first, each package once. What a package depends
/// on is what the lock says it builds (exact) when there is one; without,
/// what its own manifest compiles here with default features (never
/// test-only or other-target dependencies). Which release of each: a
/// resolver's choice in `hints`, else the lock, else the newest unpacked.
fn berg(
    name: &str,
    version: &str,
    own: usize,
    direct: &[Want],
    hints: &HashMap<String, String>,
    lock: Option<&Lock>,
    basis: Basis,
    cancelled: &mut impl FnMut() -> bool,
) -> Option<Berg> {
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
            let mut fits: Vec<&str> = packages
                .iter()
                .map(|p| p.version.as_str())
                .filter(|v| registry::satisfies(requirement, v))
                .collect();
            fits.sort_by(|a, b| facet::marks::semver::cmp(a, b));
            if let Some(best) = fits.last() {
                return Some((*best).to_owned());
            }
        }
        registry::pick(dep, requirement, hint)
    };
    let root_key = (name.to_owned(), version.to_owned());
    while !frontier.is_empty() && layer < LAYERS && blocks.len() < LIMIT {
        if cancelled() {
            return None;
        }
        let mut next: Vec<(Option<usize>, Vec<Want>)> = Vec::new();
        for (parent, deps) in std::mem::take(&mut frontier) {
            for Want {
                package: dep_name,
                requirement,
                defaults,
            } in deps
            {
                if cancelled() {
                    return None;
                }
                let hint = hints
                    .get(&dep_name)
                    .map(String::as_str)
                    .filter(|_| parent.is_none());
                let Some(dep_version) = resolve(&dep_name, &requirement, hint) else {
                    missing += 1;
                    continue;
                };
                if cancelled() {
                    return None;
                }
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
                    if cancelled() {
                        return None;
                    }
                    let at = blocks.len();
                    blocks.push(Block {
                        name: dep_name.clone(),
                        version: dep_version.clone(),
                        sloc,
                        layer,
                        parent,
                        deps: Vec::new(),
                    });
                    index.insert(key, at);
                    // What it depends on, for the next layer.
                    let below = lock
                        .and_then(|lock| locked_wants(lock, &dep_name, &dep_version))
                        .unwrap_or_else(|| {
                            registry::source_of(&dep_name, &dep_version)
                                .and_then(|dir| manifest::read(&dir))
                                .map(|m| manifest_wants(&m, defaults))
                                .unwrap_or_default()
                        });
                    if cancelled() {
                        return None;
                    }
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
    Some(Berg {
        own,
        blocks,
        below,
        missing,
        basis,
    })
}

/// Reads the package at `root` (its name and version are the manifest's).
/// `hints` are versions a resolver already chose for its direct dependencies.
#[must_use]
pub fn read(
    root: &Path,
    hints: &HashMap<String, String>,
    project: Option<&Path>,
) -> Option<SourceFacts> {
    read_cancellable(root, hints, project, None)
}

fn read_cancellable(
    root: &Path,
    hints: &HashMap<String, String>,
    project: Option<&Path>,
    cancellation: Option<&Cancellation>,
) -> Option<SourceFacts> {
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return None;
    }
    let manifest = manifest::read(root)?;
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return None;
    }
    let scan = match cancellation {
        Some(cancellation) => {
            scan::scan_cancellable(root, &manifest.lib, || cancellation.is_cancelled())?
        }
        None => scan::scan(root, &manifest.lib),
    };
    // A project's lock file is exact about what it builds. A registry crate
    // ships a lock of its own (its tests' world), which says nothing about
    // what a consumer builds, so a crate is read against the active
    // project's lock instead.
    let registry_crate = in_registry(root);
    let lock = if registry_crate {
        project.and_then(lock_above)
    } else {
        lock_above(root).or_else(|| project.and_then(lock_above))
    }
    .map(|lock| lock_table(&lock));
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return None;
    }
    let locked = lock
        .as_ref()
        .filter(|_| registry_crate)
        .and_then(|lock| locked_wants(lock, &manifest.name, &manifest.version));
    let basis = if lock.is_some() && (locked.is_some() || !registry_crate) {
        Basis::Lock
    } else {
        Basis::Defaults
    };
    let direct: Vec<Want> =
        locked.unwrap_or_else(|| manifest_wants(&manifest, manifest::DefaultFeatures::On));
    let mut is_cancelled = || cancellation.is_some_and(Cancellation::is_cancelled);
    let berg = berg(
        &manifest.name,
        &manifest.version,
        scan.sloc,
        &direct,
        hints,
        lock.as_ref(),
        basis,
        &mut is_cancelled,
    )?;
    let mut dependency_lines = Vec::new();
    for dependency in manifest
        .dependencies
        .iter()
        .filter(|dependency| dependency.need == manifest::Need::Optional)
    {
        if is_cancelled() {
            return None;
        }
        let version = registry::pick(
            &dependency.package,
            &dependency.req,
            hints.get(&dependency.package).map(String::as_str),
        );
        if is_cancelled() {
            return None;
        }
        let lines = version.and_then(|version| registry::sloc_of(&dependency.package, &version));
        if is_cancelled() {
            return None;
        }
        dependency_lines.push((dependency.key.clone(), lines));
    }
    let lib_root = {
        let dir = root
            .join(&manifest.lib)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("src"));
        if dir.is_dir() { dir } else { root.join("src") }
    };
    let modules = match cancellation {
        Some(_) => docs::items_cancellable(&lib_root, root, || is_cancelled())?,
        None => docs::items(&lib_root, root),
    };
    let root_file = {
        let lib = root.join(&manifest.lib);
        if lib.is_file() {
            lib
        } else {
            root.join("src").join("main.rs")
        }
    };
    let mut module_docs = HashMap::new();
    for module in &modules {
        if is_cancelled() {
            return None;
        }
        if let Some(doc) = match cancellation {
            Some(_) => {
                docs::module_doc_cancellable(&lib_root, &root_file, &module.path, || is_cancelled())
            }
            None => docs::module_doc(&lib_root, &root_file, &module.path),
        } {
            module_docs.insert(module.path.clone(), doc);
        }
    }
    if is_cancelled() {
        return None;
    }
    let docs = module_docs;
    Some(SourceFacts {
        root: root.to_path_buf(),
        manifest,
        scan,
        berg,
        dependency_lines,
        modules,
        docs,
    })
}

enum Entry {
    Reading(Cancellation),
    Done(Reading),
}

/// One package's entry, and the project it was read against (`None`:
/// installed by a test or capture harness, good for any project).
struct Slot {
    project: Option<Option<PathBuf>>,
    authority: Option<SourceAuthority>,
    hints: Option<Vec<(String, String)>>,
    expires_at: Option<Instant>,
    flight: Option<u64>,
    entry: Entry,
}

/// Inputs from the active owner/registry composition that affect source facts.
/// Generation distinguishes a replacement provider with the same endpoints
/// and path key without using an allocation address as evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceAuthority {
    endpoint: PathBuf,
    key: Arc<str>,
    generation: CompositionGeneration,
}

const SOURCE_FACTS_CAPACITY: usize = 24;

#[derive(Default)]
struct Service {
    entries: HashMap<PackageRef, Slot>,
    order: VecDeque<PackageRef>,
    next_flight: u64,
}

impl Service {
    fn get(
        &mut self,
        package: &PackageRef,
        project: &Option<PathBuf>,
        authority: &Option<SourceAuthority>,
        hints: &HashMap<String, String>,
    ) -> Option<Reading> {
        let slot = self.entries.get(package)?;
        if let Some(stored) = slot.project.as_ref() {
            if stored != project
                || &slot.authority != authority
                || !slot
                    .hints
                    .as_deref()
                    .is_some_and(|identity| hints_match(identity, hints))
            {
                return None;
            }
            if matches!(&slot.entry, Entry::Done(_))
                && slot
                    .expires_at
                    .is_some_and(|expires| Instant::now() >= expires)
            {
                return None;
            }
        }
        let reading = match &slot.entry {
            Entry::Reading(_) => Reading::Reading,
            Entry::Done(reading) => reading.clone(),
        };
        self.order.retain(|key| key != package);
        self.order.push_back(package.clone());
        Some(reading)
    }

    fn begin(
        &mut self,
        package: PackageRef,
        project: Option<PathBuf>,
        authority: Option<SourceAuthority>,
        hints: Vec<(String, String)>,
    ) -> Option<(u64, Cancellation)> {
        if let Some(slot) = self.entries.get(&package) {
            if let Entry::Reading(cancellation) = &slot.entry {
                cancellation.cancel();
                return None;
            }
        }
        self.remove(&package);
        while self.entries.len() >= SOURCE_FACTS_CAPACITY {
            let completed = self
                .order
                .iter()
                .find(|key| {
                    self.entries
                        .get(*key)
                        .is_some_and(|slot| matches!(&slot.entry, Entry::Done(_)))
                })
                .cloned();
            if let Some(completed) = completed {
                self.remove(&completed);
            } else if let Some(oldest) = self.order.front().cloned() {
                if let Some(Slot {
                    entry: Entry::Reading(cancellation),
                    ..
                }) = self.entries.get(&oldest)
                {
                    cancellation.cancel();
                }
                return None;
            } else {
                return None;
            }
        }
        self.next_flight = self.next_flight.wrapping_add(1).max(1);
        let flight = self.next_flight;
        let cancellation = Cancellation::default();
        self.insert_slot(
            package,
            Slot {
                project: Some(project),
                authority,
                hints: Some(hints),
                expires_at: None,
                flight: Some(flight),
                entry: Entry::Reading(cancellation.clone()),
            },
        );
        Some((flight, cancellation))
    }

    fn complete(&mut self, package: &PackageRef, flight: u64, result: Reading) {
        let is_current = self
            .entries
            .get(package)
            .is_some_and(|slot| slot.flight == Some(flight));
        if !is_current {
            return;
        }
        let cancelled = self.entries.get(package).is_some_and(|slot| {
            matches!(&slot.entry, Entry::Reading(cancellation) if cancellation.is_cancelled())
        });
        if cancelled {
            self.remove(package);
            return;
        }
        if let Some(slot) = self.entries.get_mut(package) {
            slot.entry = Entry::Done(result);
            slot.expires_at = Some(Instant::now() + FACTS_TTL);
            slot.flight = None;
        }
        self.order.retain(|key| key != package);
        self.order.push_back(package.clone());
    }

    fn insert(&mut self, package: PackageRef, slot: Slot) {
        if let Some(Slot {
            entry: Entry::Reading(cancellation),
            ..
        }) = self.entries.get(&package)
        {
            // A manual snapshot cannot replace an active flight: the worker
            // keeps its bounded slot until its exact flight ticket returns.
            cancellation.cancel();
            return;
        }
        self.remove(&package);
        if self.entries.len() >= SOURCE_FACTS_CAPACITY {
            let completed = self
                .order
                .iter()
                .find(|key| {
                    self.entries
                        .get(*key)
                        .is_some_and(|slot| matches!(&slot.entry, Entry::Done(_)))
                })
                .cloned();
            if let Some(completed) = completed {
                self.remove(&completed);
            } else {
                return;
            }
        }
        self.insert_slot(package, slot);
    }

    fn insert_slot(&mut self, package: PackageRef, slot: Slot) {
        self.order.retain(|key| key != &package);
        self.order.push_back(package.clone());
        self.entries.insert(package, slot);
    }

    fn remove(&mut self, package: &PackageRef) {
        if let Some(Slot {
            entry: Entry::Reading(cancellation),
            ..
        }) = self.entries.get(package)
        {
            cancellation.cancel();
        }
        self.entries.remove(package);
        self.order.retain(|key| key != package);
    }
}

impl Global for Service {}

impl Drop for Service {
    fn drop(&mut self) {
        for slot in self.entries.values() {
            if let Entry::Reading(cancellation) = &slot.entry {
                cancellation.cancel();
            }
        }
    }
}

/// What the source on disk says about `package`, read against `project`'s
/// lock file (the reader's active project). The first ask starts the read
/// (off the UI thread) and returns [`Reading::Reading`]; the windows redraw
/// when it lands. Asking again with another project reads again.
pub fn reading(
    package: &PackageRef,
    hints: &HashMap<String, String>,
    project: Option<&Path>,
    cx: &mut App,
) -> Reading {
    let wanted = project.map(Path::to_path_buf);
    let composition = crate::host::registry::composed();
    let authority = composition.as_ref().map(|composition| {
        SourceAuthority {
            endpoint: composition.endpoint.clone(),
            key: composition.authority.clone(),
            generation: composition.generation,
        }
    });
    if let Some(reading) = cx
        .default_global::<Service>()
        .get(package, &wanted, &authority, hints)
    {
        return reading;
    }
    // Canonicalize only when a new flight is needed. Cache hits compare the
    // caller's map against stored pairs without allocating or sorting.
    let hint_identity = hint_identity(hints);
    let Some((flight, cancellation)) =
        cx.default_global::<Service>()
            .begin(package.clone(), wanted.clone(), authority, hint_identity)
    else {
        return Reading::Waiting;
    };
    #[cfg(test)]
    {
        let _ = package;
        let _ = hints;
        let _ = flight;
        let _ = cancellation;
    }
    #[cfg(not(test))]
    {
        let (key, hints) = (package.clone(), hints.clone());
        let worker_package = key.clone();
        let read_for = wanted.clone();
        let worker_composition = composition.clone();
        let work_cancellation = cancellation.clone();
        let work = cx.background_executor().spawn(async move {
            catch_unwind(AssertUnwindSafe(|| {
                read_package(
                    &worker_package,
                    &hints,
                    read_for.as_deref(),
                    worker_composition.as_ref(),
                    &work_cancellation,
                )
            }))
            .unwrap_or_else(|_| Err("The source reader stopped unexpectedly.".into()))
        });
        cx.spawn(async move |cx| {
            let facts = work.await;
            let _ = cx.update(|cx| {
                let done =
                    facts.map_or_else(Reading::Absent, |facts| Reading::Ready(Arc::new(facts)));
                cx.default_global::<Service>().complete(&key, flight, done);
                cx.refresh_windows();
            });
        })
        .detach();
    }
    Reading::Reading
}

fn hint_identity(hints: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut identity: Vec<(String, String)> = hints
        .iter()
        .map(|(name, version)| (name.clone(), version.clone()))
        .collect();
    identity.sort_unstable();
    identity
}

fn hints_match(identity: &[(String, String)], hints: &HashMap<String, String>) -> bool {
    identity.len() == hints.len()
        && identity
            .iter()
            .all(|(name, version)| hints.get(name) == Some(version))
}

/// Installs what a test or a capture harness has already read for a
/// package, so the page draws it without starting a read.
#[doc(hidden)]
pub fn install(package: &PackageRef, reading: Reading, cx: &mut App) {
    cx.default_global::<Service>().insert(
        package.clone(),
        Slot {
            project: None,
            authority: None,
            hints: None,
            expires_at: None,
            flight: None,
            entry: Entry::Done(reading),
        },
    );
}

#[cfg(test)]
mod tests;
