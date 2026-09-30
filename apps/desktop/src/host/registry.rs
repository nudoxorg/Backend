//! Registry releases as source trees the owner indexes (W-Acquire).
//!
//! A registry release, a crate name and one exact version, resolves through a
//! [`RegistrySource`] to an unpacked source tree. The owner indexes that tree
//! as a root with the same command every project uses (`Session::index`), so
//! a release added from browsing and a project added from a folder reach the
//! owner the same way; this module only says *where the source is*.
//!
//! [`CargoCache`] is the source in this build. It never touches the network:
//! a release is either unpacked by cargo already
//! (`$CARGO_HOME/registry/src/<index>/<name>-<version>`), or its registry
//! archive is on this machine (`$CARGO_HOME/registry/cache/<index>/<name>-<version>.crate`,
//! the artifact cargo downloaded and verified), which is checked against the
//! registry index's checksum and unpacked into the desktop's own directory.
//! A release that is only published is [`Availability::Download`]: reading it
//! needs a download, which a network-backed source would perform (in
//! production, the owner's own registry acquisition of `pkg:cargo/NAME@VERSION`);
//! this source says so instead of fetching.

pub(crate) use crate::model::release::{Availability, CrateName, Published, Release, Version};
use facet::marks::semver;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

mod archive;

/// How a resolved tree came to be on this machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// Cargo unpacked it.
    Cargo,
    /// This app unpacked the archive at this path.
    Archive(PathBuf),
    /// This app already unpacked the archive, and its original file is gone.
    AppCache,
}

/// A release's source tree, ready for the owner to index as a root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceTree {
    pub(crate) release: Release,
    /// The tree's root (its `Cargo.toml`), canonical.
    pub(crate) root: PathBuf,
    pub(crate) origin: Origin,
}

/// Why a release could not be resolved to a tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceError {
    /// Its source is not on this machine and this source never downloads.
    NeedsDownload(Release),
    /// The archive on this machine is not the one the registry published.
    Integrity {
        release: Release,
        expected: String,
        actual: String,
    },
    /// The archive could not be read or unpacked.
    Archive { release: Release, reason: String },
    /// The file system refused.
    Io { release: Release, reason: String },
    /// More than one registry authority contains this exact release.
    Ambiguous {
        release: Release,
        paths: Vec<PathBuf>,
    },
    /// The release exists only as an archive or in this app's unpacked cache.
    NotUnpacked(Release),
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsDownload(release) => write!(
                formatter,
                "{release} is not on this machine; reading it needs a download"
            ),
            Self::Integrity {
                release,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "the archive of {release} on this machine is not the published one (sha256 {actual}, the registry says {expected})"
                )
            }
            Self::Archive { release, reason } => write!(
                formatter,
                "the archive of {release} could not be unpacked: {reason}"
            ),
            Self::Io { release, reason } => write!(
                formatter,
                "the source of {release} could not be read: {reason}"
            ),
            Self::Ambiguous { release, paths } => write!(
                formatter,
                "more than one Cargo registry source contains {release}: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NotUnpacked(release) => write!(
                formatter,
                "{release} is not already unpacked in Cargo's registry source cache"
            ),
        }
    }
}

/// Stable identity for the Cargo cache authority used by this desktop.
/// Harness refusal records include this value so a cache switch cannot reuse
/// evidence from another Cargo home or registry index.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CargoAuthorityKey(String);

impl CargoAuthorityKey {
    /// Deterministic escaped representation suitable for a refusal key.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CargoAuthorityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Registry name and version to an unpacked source tree.
pub(crate) trait RegistrySource: Send + Sync {
    /// Every published release of `name`, oldest first, with where its
    /// source is. Releases on this machine the index does not list are kept.
    fn releases(&self, name: &CrateName) -> Vec<Published>;

    /// Crates whose name contains `query` (case-insensitive) that can be read
    /// without the network, each at its newest release here, best first.
    fn offline(&self, query: &str, limit: usize) -> Vec<Release>;

    /// Where `release`'s source is.
    fn availability(&self, release: &Release) -> Availability;

    /// The release `root` is, when it is one of this source's trees.
    fn release_of(&self, root: &Path) -> Option<Release>;

    /// The release's tree, unpacking its archive when that is all there is.
    ///
    /// # Errors
    /// [`SourceError`]; never a download.
    fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError>;

    /// Where `release`'s tree is, or will be once its archive is unpacked
    /// (the root the owner lists it under); `None` when reading it needs a
    /// download. Nothing is unpacked.
    fn tree_of(&self, release: &Release) -> Option<PathBuf> {
        match self.availability(release) {
            Availability::Unpacked(tree) => Some(tree.canonicalize().unwrap_or(tree)),
            Availability::Archive(_) | Availability::Download => None,
        }
    }
}

/// The local cargo cache, and a directory of this app's own for archives it
/// unpacks (`<unpacked>/<name>-<version>/<name>-<version>`, the outer one a
/// generated one-member Cargo workspace so cargo never walks up into another).
#[derive(Clone, Debug)]
pub(crate) struct CargoCache {
    home: PathBuf,
    /// An operator may pin Cargo package sources independently of CARGO_HOME.
    source_root: Option<PathBuf>,
    unpacked: PathBuf,
}

impl CargoCache {
    /// Cargo's home (`NUDOX_CARGO_HOME`, then `CARGO_HOME`, then `~/.cargo`)
    /// and optional explicit package source root, unpacking into `unpacked`.
    pub(crate) fn from_env(unpacked: PathBuf) -> Option<Self> {
        let explicit_home = std::env::var_os("NUDOX_CARGO_HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("CARGO_HOME").filter(|value| !value.is_empty()));
        let source_root = std::env::var_os("NUDOX_CARGO_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        if source_root.as_ref().is_some_and(|path| !path.is_absolute()) {
            return None;
        }
        let home = match explicit_home {
            // An explicit invalid path must not silently select a different cache.
            Some(home) => absolute(PathBuf::from(home))?,
            None => std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .and_then(|home| absolute(PathBuf::from(home)).map(|home| home.join(".cargo")))
                .or_else(|| source_root.as_deref().and_then(infer_cargo_home))?,
        };
        Some(Self {
            home: stable_path(home),
            source_root: source_root.map(stable_path),
            unpacked,
        })
    }

    /// Cargo's home at `home`, unpacking into `unpacked`.
    pub(crate) const fn at(home: PathBuf, unpacked: PathBuf) -> Self {
        Self {
            home,
            source_root: None,
            unpacked,
        }
    }

    /// Effective Cargo home and registry index paths, in stable order.
    pub(crate) fn authority_key(&self) -> CargoAuthorityKey {
        let configured_root = self.source_root.as_ref().map(stable_path);
        CargoAuthorityKey(format!(
            "cargo-home={:?}; cargo-root={:?}; registry-indices={:?}; source-indices={:?}; archive-indices={:?}",
            stable_path(self.home.clone()),
            configured_root,
            self.index_dirs(),
            self.source_dirs(),
            self.archive_dirs(),
        ))
    }

    /// Resolves a source that Cargo itself has already unpacked. The harness
    /// uses this form so fixture setup cannot silently mutate a registry cache.
    pub(crate) fn resolve_unpacked(&self, release: &Release) -> Result<SourceTree, SourceError> {
        let Some(tree) = self.unambiguous(release, self.cargo_trees(release))? else {
            return Err(SourceError::NotUnpacked(release.clone()));
        };
        let root = tree.canonicalize().map_err(|error| SourceError::Io {
            release: release.clone(),
            reason: error.to_string(),
        })?;
        Ok(SourceTree {
            release: release.clone(),
            root,
            origin: Origin::Cargo,
        })
    }

    /// Cargo's unpacked sources, one directory per registry index.
    fn source_dirs(&self) -> Vec<PathBuf> {
        self.index_roots(
            self.source_root
                .as_deref()
                .unwrap_or(&self.home.join("registry").join("src")),
        )
    }

    /// Cargo's downloaded archives, one directory per registry index.
    fn archive_dirs(&self) -> Vec<PathBuf> {
        index_dirs(&self.home.join("registry").join("cache"))
            .into_iter()
            .filter(|dir| is_crates_io_index(dir))
            .map(stable_path)
            .collect()
    }

    fn cargo_tree(&self, release: &Release) -> Option<PathBuf> {
        self.unambiguous(release, self.cargo_trees(release))
            .ok()
            .flatten()
    }

    fn archive(&self, release: &Release) -> Option<PathBuf> {
        self.unambiguous(release, self.archives(release))
            .ok()
            .flatten()
    }

    fn index_roots(&self, root: &Path) -> Vec<PathBuf> {
        if root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("index."))
        {
            return root
                .is_dir()
                .then(|| stable_path(root.to_path_buf()))
                .into_iter()
                .collect();
        }
        let mut dirs = index_dirs(root)
            .into_iter()
            .filter(|dir| is_crates_io_index(dir))
            .map(stable_path)
            .collect::<Vec<_>>();
        // NUDOX_CARGO_ROOT may point directly at one source tree. Treat that
        // explicit root as the selected authority when it has no index children.
        if dirs.is_empty() && self.source_root.as_deref() == Some(root) && root.is_dir() {
            dirs.push(stable_path(root.to_path_buf()));
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }

    fn cargo_trees(&self, release: &Release) -> Vec<PathBuf> {
        let stem = release.stem();
        let mut trees = self
            .source_dirs()
            .into_iter()
            .map(|dir| dir.join(&stem))
            .filter(|dir| dir.join("Cargo.toml").is_file())
            .map(stable_path)
            .collect::<Vec<_>>();
        trees.sort();
        trees.dedup();
        trees
    }

    fn archives(&self, release: &Release) -> Vec<PathBuf> {
        let file = format!("{}.crate", release.stem());
        let mut archives = self
            .archive_dirs()
            .into_iter()
            .map(|dir| dir.join(&file))
            .filter(|file| file.is_file())
            .map(stable_path)
            .collect::<Vec<_>>();
        archives.sort();
        archives.dedup();
        archives
    }

    /// Selects only one release source. A name and version do not identify a
    /// registry: when two indices contain it, only an explicit source root
    /// can disambiguate which one the owner should index.
    fn unambiguous(
        &self,
        release: &Release,
        mut paths: Vec<PathBuf>,
    ) -> Result<Option<PathBuf>, SourceError> {
        if paths.len() <= 1 {
            return Ok(paths.pop());
        }
        Err(SourceError::Ambiguous {
            release: release.clone(),
            paths,
        })
    }

    fn index_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = index_dirs(&self.home.join("registry").join("index"))
            .into_iter()
            .filter(|dir| is_crates_io_index(dir))
            .map(stable_path)
            .collect::<Vec<_>>();
        dirs.sort();
        dirs.dedup();
        dirs
    }

    fn index_records(&self, name: &str) -> Vec<IndexRecord> {
        let relative = cache_relative(name);
        if relative.as_os_str().is_empty() {
            return Vec::new();
        }
        self.index_dirs()
            .into_iter()
            .flat_map(|index| {
                let index_id = index
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                std::fs::read(index.join(".cache").join(&relative))
                    .ok()
                    .map(|bytes| parse_index_records(&bytes, index_id))
                    .unwrap_or_default()
            })
            .collect()
    }

    fn checksum_in_index(&self, index: &str, release: &Release) -> Option<String> {
        let mut checksums = self
            .index_records(release.name.as_str())
            .into_iter()
            .filter(|record| record.index == index && record.version == release.version.as_str())
            .filter_map(|record| record.checksum)
            .collect::<Vec<_>>();
        checksums.sort();
        checksums.dedup();
        (checksums.len() == 1).then(|| checksums.remove(0))
    }

    /// Where this app unpacks `release`.
    fn own_tree(&self, release: &Release) -> PathBuf {
        let stem = release.stem();
        self.unpacked.join(&stem).join(stem)
    }

    fn locate(&self, release: &Release) -> Availability {
        match self.unambiguous(release, self.cargo_trees(release)) {
            Ok(Some(tree)) => return Availability::Unpacked(tree),
            Err(_) => return Availability::Download,
            Ok(None) => {}
        }
        let own = self.own_tree(release);
        if own.join("Cargo.toml").is_file() {
            return Availability::Unpacked(own);
        }
        match self.unambiguous(release, self.archives(release)) {
            Ok(Some(archive)) => Availability::Archive(archive),
            Ok(None) | Err(_) => Availability::Download,
        }
    }

    /// Every `name-version` stem on this machine: unpacked trees and archives.
    fn stems(&self) -> Vec<String> {
        let mut stems = Vec::new();
        for dir in self.source_dirs() {
            stems.extend(
                entries(&dir)
                    .into_iter()
                    .filter(|(_, is_dir)| *is_dir)
                    .map(|(name, _)| name),
            );
        }
        for dir in self.archive_dirs() {
            stems.extend(entries(&dir).into_iter().filter_map(|(name, is_dir)| {
                (!is_dir)
                    .then(|| name.strip_suffix(".crate").map(str::to_owned))
                    .flatten()
            }));
        }
        stems.sort();
        stems.dedup();
        stems
    }
}

impl RegistrySource for CargoCache {
    fn releases(&self, name: &CrateName) -> Vec<Published> {
        let mut versions = BTreeMap::<String, Vec<IndexRecord>>::new();
        for record in self.index_records(name.as_str()) {
            versions
                .entry(record.version.clone())
                .or_default()
                .push(record);
        }
        let mut out = versions
            .into_iter()
            .filter_map(|(version, records)| {
                let release = Release {
                    name: name.clone(),
                    version: Version::new(&version).ok()?,
                };
                let first = records.first();
                let consistent = first.is_some_and(|first| {
                    records.iter().all(|record| {
                        record.checksum == first.checksum
                            && record.date == first.date
                            && record.yanked == first.yanked
                    })
                });
                Some(Published {
                    availability: self.locate(&release),
                    release,
                    date: consistent
                        .then(|| first.and_then(|record| record.date.clone()))
                        .flatten(),
                    yanked: consistent && first.is_some_and(|record| record.yanked),
                })
            })
            .collect::<Vec<_>>();
        let prefix = format!("{name}-");
        for stem in self.stems() {
            let Some(release) = stem
                .strip_prefix(&prefix)
                .and_then(|_| Release::from_stem(&stem))
                .filter(|release| &release.name == name)
            else {
                continue;
            };
            if out.iter().all(|known| known.release != release) {
                out.push(Published {
                    availability: self.locate(&release),
                    release,
                    date: None,
                    yanked: false,
                });
            }
        }
        out.sort_by(|a, b| semver::cmp(a.release.version.as_str(), b.release.version.as_str()));
        out
    }

    fn offline(&self, query: &str, limit: usize) -> Vec<Release> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut newest = std::collections::BTreeMap::<CrateName, Version>::new();
        for release in self
            .stems()
            .iter()
            .filter_map(|stem| Release::from_stem(stem))
        {
            if !release.name.as_str().to_lowercase().contains(&query)
                || !semver::parse(release.version.as_str()).pre.is_empty()
            {
                continue;
            }
            let keep = newest
                .get(&release.name)
                .is_none_or(|known| semver::cmp(known.as_str(), release.version.as_str()).is_lt());
            if keep {
                newest.insert(release.name, release.version);
            }
        }
        let mut found = newest
            .into_iter()
            .map(|(name, version)| Release { name, version })
            .collect::<Vec<_>>();
        // An exact name first, then names that start with the query, then the rest.
        found.sort_by_cached_key(|release| {
            let name = release.name.as_str().to_lowercase();
            (name != query, !name.starts_with(&query), name.len(), name)
        });
        found.truncate(limit);
        found
    }

    fn availability(&self, release: &Release) -> Availability {
        self.locate(release)
    }

    fn tree_of(&self, release: &Release) -> Option<PathBuf> {
        match self.locate(release) {
            Availability::Unpacked(tree) => Some(tree.canonicalize().unwrap_or(tree)),
            // `resolve` unpacks it here, and the owner lists it by this path.
            Availability::Archive(_) => {
                let own = self.own_tree(release);
                Some(self.unpacked.canonicalize().map_or(own, |unpacked| {
                    unpacked.join(release.stem()).join(release.stem())
                }))
            }
            Availability::Download => None,
        }
    }

    fn release_of(&self, root: &Path) -> Option<Release> {
        let root = root.canonicalize().ok()?;
        let parent = root.parent()?;
        let stem = root.file_name()?.to_str()?;
        let cargo = self
            .source_dirs()
            .iter()
            .any(|dir| dir.canonicalize().is_ok_and(|dir| dir == parent));
        let own = self
            .unpacked
            .canonicalize()
            .is_ok_and(|unpacked| parent.parent() == Some(unpacked.as_path()))
            && parent.file_name().and_then(|name| name.to_str()) == Some(stem);
        (cargo || own).then(|| Release::from_stem(stem)).flatten()
    }

    fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> {
        let io = |error: std::io::Error| SourceError::Io {
            release: release.clone(),
            reason: error.to_string(),
        };
        if let Some(tree) = self.unambiguous(release, self.cargo_trees(release))? {
            return Ok(SourceTree {
                release: release.clone(),
                root: tree.canonicalize().map_err(io)?,
                origin: Origin::Cargo,
            });
        }
        let own = self.own_tree(release);
        if own.join("Cargo.toml").is_file() {
            return Ok(SourceTree {
                release: release.clone(),
                root: own.canonicalize().map_err(io)?,
                origin: Origin::AppCache,
            });
        }
        let Some(file) = self.unambiguous(release, self.archives(release))? else {
            return Err(SourceError::NeedsDownload(release.clone()));
        };
        let index = file
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
        let checksum = index.and_then(|index| self.checksum_in_index(&index, release));
        let root = archive::unpack(&file, checksum.as_deref(), release, &self.unpacked)?;
        Ok(SourceTree {
            release: release.clone(),
            root: root.canonicalize().map_err(io)?,
            origin: Origin::Archive(file),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexRecord {
    index: String,
    version: String,
    checksum: Option<String>,
    date: Option<Arc<str>>,
    yanked: bool,
}

fn cache_relative(name: &str) -> PathBuf {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 => Path::new("1").join(name),
        2 => Path::new("2").join(name),
        3 => Path::new("3").join(&name[..1]).join(name),
        4.. => Path::new(&name[..2]).join(&name[2..4]).join(name),
        _ => PathBuf::new(),
    }
}

fn parse_index_records(bytes: &[u8], index: String) -> Vec<IndexRecord> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|part| {
            if part.first() != Some(&b'{') {
                return None;
            }
            let value = serde_json::from_slice::<serde_json::Value>(part).ok()?;
            let version = value.get("vers")?.as_str()?.to_owned();
            let checksum = value
                .get("cksum")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let date = value
                .get("pubtime")
                .and_then(serde_json::Value::as_str)
                .and_then(|time| time.get(..10))
                .map(Arc::<str>::from);
            let yanked = value
                .get("yanked")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            Some(IndexRecord {
                index: index.clone(),
                version,
                checksum,
                date,
                yanked,
            })
        })
        .collect()
}

fn is_crates_io_index(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("index.crates.io-"))
}

fn absolute(path: PathBuf) -> Option<PathBuf> {
    path.is_absolute().then_some(path)
}

fn stable_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn infer_cargo_home(source_root: &Path) -> Option<PathBuf> {
    let root = if source_root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("index."))
    {
        source_root.parent()?
    } else {
        source_root
    };
    (root.file_name()? == "src")
        .then(|| root.parent()?.parent().map(Path::to_path_buf))
        .flatten()
}

/// `dir`'s registry index directories (`index.crates.io-*`), sorted.
fn index_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = entries(dir)
        .into_iter()
        .filter(|(name, is_dir)| *is_dir && name.starts_with("index."))
        .map(|(name, _)| dir.join(name))
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

/// `dir`'s entries by name, and whether each is a directory.
fn entries(dir: &Path) -> Vec<(String, bool)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|entry| {
            Some((
                entry.file_name().into_string().ok()?,
                entry.file_type().ok()?.is_dir(),
            ))
        })
        .collect()
}

/// The owner this process indexes releases through, and the source it
/// resolves them with. Published when an owner starts (the window's own, or
/// the harness's); the latest start wins.
#[derive(Clone)]
pub(crate) struct Composition {
    /// The owner's endpoint.
    pub(crate) endpoint: PathBuf,
    pub(crate) source: Arc<dyn RegistrySource>,
    /// Where the owner's words are kept for releases it listed but could not
    /// compile (`runtime::acquire::work::Refusals`), beside its workspace so
    /// they go wherever the index goes. `None`: not kept (tests).
    pub(crate) refusals: Option<PathBuf>,
}

impl fmt::Debug for Composition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Composition")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

static COMPOSED: RwLock<Option<Composition>> = RwLock::new(None);

/// Composes the cargo cache for the owner at `endpoint` whose workspace is
/// `data` (archives unpack into `data/registry-sources`).
pub(crate) fn publish(endpoint: &Path, data: &Path) {
    let Some(source) = CargoCache::from_env(data.join("registry-sources")) else {
        return;
    };
    let refusals = Some(data.join("registry-sources").join("refusals.json"));
    install(Composition {
        endpoint: endpoint.to_path_buf(),
        source: Arc::new(source),
        refusals,
    });
}

/// Installs a composition (tests compose their own).
pub(crate) fn install(composition: Composition) {
    *COMPOSED.write().unwrap_or_else(PoisonError::into_inner) = Some(composition);
}

/// The composition, when an owner has started in this process.
pub(crate) fn composed() -> Option<Composition> {
    COMPOSED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod owner_tests;
#[cfg(test)]
mod tests;
