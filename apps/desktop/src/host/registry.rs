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
use crate::model::source_facts::registry as cargo_home;
use facet::marks::semver;
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
        }
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
    unpacked: PathBuf,
}

impl CargoCache {
    /// Cargo's home as the process was given it (`CARGO_HOME`, else
    /// `~/.cargo`), unpacking into `unpacked`.
    pub(crate) fn from_env(unpacked: PathBuf) -> Option<Self> {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo")))?;
        Some(Self::at(home, unpacked))
    }

    /// Cargo's home at `home`, unpacking into `unpacked`.
    pub(crate) const fn at(home: PathBuf, unpacked: PathBuf) -> Self {
        Self { home, unpacked }
    }

    /// Cargo's unpacked sources, one directory per registry index.
    fn source_dirs(&self) -> Vec<PathBuf> {
        index_dirs(&self.home.join("registry").join("src"))
    }

    /// Cargo's downloaded archives, one directory per registry index.
    fn archive_dirs(&self) -> Vec<PathBuf> {
        index_dirs(&self.home.join("registry").join("cache"))
    }

    fn cargo_tree(&self, release: &Release) -> Option<PathBuf> {
        let stem = release.stem();
        self.source_dirs()
            .into_iter()
            .map(|dir| dir.join(&stem))
            .find(|dir| dir.join("Cargo.toml").is_file())
    }

    fn archive(&self, release: &Release) -> Option<PathBuf> {
        let file = format!("{}.crate", release.stem());
        self.archive_dirs()
            .into_iter()
            .map(|dir| dir.join(&file))
            .find(|file| file.is_file())
    }

    /// Where this app unpacks `release`.
    fn own_tree(&self, release: &Release) -> PathBuf {
        let stem = release.stem();
        self.unpacked.join(&stem).join(stem)
    }

    fn locate(&self, release: &Release) -> Availability {
        if let Some(tree) = self.cargo_tree(release) {
            return Availability::Unpacked(tree);
        }
        let own = self.own_tree(release);
        if own.join("Cargo.toml").is_file() {
            return Availability::Unpacked(own);
        }
        self.archive(release)
            .map_or(Availability::Download, Availability::Archive)
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
        let mut out = cargo_home::releases(name.as_str())
            .into_iter()
            .filter_map(|published| {
                let release = Release {
                    name: name.clone(),
                    version: Version::new(&published.version).ok()?,
                };
                Some(Published {
                    availability: self.locate(&release),
                    release,
                    date: published.date.map(Arc::from),
                    yanked: published.standing == facet::folio::state::Standing::Yanked,
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
        match self.locate(release) {
            Availability::Unpacked(tree) => {
                let origin = if tree.starts_with(&self.unpacked) {
                    Origin::Archive(self.archive(release).unwrap_or_default())
                } else {
                    Origin::Cargo
                };
                Ok(SourceTree {
                    release: release.clone(),
                    root: tree.canonicalize().map_err(io)?,
                    origin,
                })
            }
            Availability::Archive(file) => {
                let checksum = cargo_home::releases(release.name.as_str())
                    .into_iter()
                    .find(|published| published.version == release.version.as_str())
                    .and_then(|published| published.checksum);
                let root = archive::unpack(&file, checksum.as_deref(), release, &self.unpacked)?;
                Ok(SourceTree {
                    release: release.clone(),
                    root: root.canonicalize().map_err(io)?,
                    origin: Origin::Archive(file),
                })
            }
            Availability::Download => Err(SourceError::NeedsDownload(release.clone())),
        }
    }
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
