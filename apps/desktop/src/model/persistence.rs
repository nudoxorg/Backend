//! Durable shelf/settings/session schema with crash-safe publication.

use super::snapshot::{AppSnapshot, SessionState, SettingsState};
use crate::core::ids::LocalProjectId;
use crate::navigation::{Coordinate, Overlay, PackageLane, Route, SettingsPage};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCHEMA: u32 = 1;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Persistent shelf entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedShelfItem {
    /// Local project path retained for cold reload.
    pub local_path: String,
    /// Stable user-facing label.
    pub label: String,
}

/// Versioned, forward-compatible desktop state file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersistedDesktopState {
    /// Schema version for migrations.
    pub schema: u32,
    /// Durable shelf rows.
    pub shelf: Vec<PersistedShelfItem>,
    /// Reduced-motion preference.
    pub reduced_motion: bool,
    /// Shelf visibility preference.
    pub shelf_open: bool,
    /// Context visibility preference.
    pub context_open: bool,
    /// Last route, encoded by a small typed tag.
    pub route: PersistedRoute,
    /// Last settings page.
    pub settings_page: Option<String>,
}

impl Default for PersistedDesktopState {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            shelf: Vec::new(),
            reduced_motion: false,
            shelf_open: true,
            context_open: true,
            route: PersistedRoute::Home,
            settings_page: None,
        }
    }
}

/// Closed route schema used only at the persistence edge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedRoute {
    /// Home route.
    Home,
    /// A producer project selected at the orbit level.
    Project {
        /// Producer project identity.
        project: u64,
    },
    /// A package route with its closed lane.
    Package {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Package lane.
        lane: PersistedPackageLane,
    },
    /// A declaration page route.
    Page {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        coordinate: String,
    },
    /// A source route.
    Source {
        /// Optional producer project.
        project: Option<u64>,
        /// Canonical package spelling.
        package: String,
        /// Declaration coordinate.
        page: String,
        /// Selected source line.
        line: u32,
    },
    /// Settings route; the page is validated on load.
    Settings,
}

/// Serializable package lane vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PersistedPackageLane {
    /// Package summary.
    Overview,
    /// Dependency graph.
    Dependencies,
    /// Reverse dependency graph.
    Dependents,
    /// Release history.
    Releases,
    /// Advisory surface.
    Security,
}

impl From<PackageLane> for PersistedPackageLane {
    fn from(value: PackageLane) -> Self {
        match value {
            PackageLane::Overview => Self::Overview,
            PackageLane::Dependencies => Self::Dependencies,
            PackageLane::Dependents => Self::Dependents,
            PackageLane::Releases => Self::Releases,
            PackageLane::Security => Self::Security,
        }
    }
}

impl From<PersistedPackageLane> for PackageLane {
    fn from(value: PersistedPackageLane) -> Self {
        match value {
            PersistedPackageLane::Overview => Self::Overview,
            PersistedPackageLane::Dependencies => Self::Dependencies,
            PersistedPackageLane::Dependents => Self::Dependents,
            PersistedPackageLane::Releases => Self::Releases,
            PersistedPackageLane::Security => Self::Security,
        }
    }
}

/// A persistence error that keeps the target path visible to diagnostics.
#[derive(Debug)]
pub struct PersistenceError {
    /// Path involved in the operation.
    pub path: PathBuf,
    /// Underlying filesystem or codec error.
    pub source: io::Error,
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for PersistenceError {}

/// Atomic state-file owner.
#[derive(Clone, Debug)]
pub struct PersistentState {
    path: PathBuf,
}

impl PersistentState {
    /// Opens a state file path without touching disk.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the exact path used for persistence.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a complete state or defaults when no file exists.
    pub fn load(&self) -> Result<PersistedDesktopState, PersistenceError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(PersistedDesktopState::default());
            }
            Err(source) => {
                return Err(PersistenceError {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let mut value: PersistedDesktopState =
            serde_json::from_slice(&bytes).map_err(|error| PersistenceError {
                path: self.path.clone(),
                source: io::Error::new(io::ErrorKind::InvalidData, error),
            })?;
        if value.schema != SCHEMA {
            value = PersistedDesktopState::default();
        }
        Ok(value)
    }

    /// Publishes one complete state via a sibling temporary and atomic rename.
    pub fn save(&self, value: &PersistedDesktopState) -> Result<(), PersistenceError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| PersistenceError {
            path: self.path.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })?;
        atomic_write(&self.path, &bytes).map_err(|source| PersistenceError {
            path: self.path.clone(),
            source,
        })
    }

    /// Encodes the snapshot's durable shelf/settings/session branches.
    #[must_use]
    pub fn project(snapshot: &AppSnapshot) -> PersistedDesktopState {
        let shelf = snapshot
            .shelf()
            .items
            .iter()
            .filter_map(|item| match &item.identity {
                crate::core::ResourceIdentity::Local(id) => Some(PersistedShelfItem {
                    local_path: id.as_str().to_owned(),
                    label: item.label.to_string(),
                }),
                crate::core::ResourceIdentity::Package(_)
                | crate::core::ResourceIdentity::Project(_) => None,
            })
            .collect();
        Self::project_session(snapshot, shelf)
    }

    fn project_session(
        snapshot: &AppSnapshot,
        shelf: Vec<PersistedShelfItem>,
    ) -> PersistedDesktopState {
        PersistedDesktopState {
            schema: SCHEMA,
            shelf,
            reduced_motion: snapshot.settings().reduced_motion,
            shelf_open: snapshot.settings().shelf_open,
            context_open: snapshot.settings().context_open,
            route: match snapshot.overlay() {
                Some(Overlay::Settings(_)) => PersistedRoute::Settings,
                Some(Overlay::CommandPalette) | None => match snapshot.route() {
                    Route::Orbit(crate::navigation::OrbitRoute::Home) => PersistedRoute::Home,
                    Route::Orbit(crate::navigation::OrbitRoute::Project(project)) => {
                        PersistedRoute::Project {
                            project: project.get().get(),
                        }
                    }
                    Route::Package(route) => PersistedRoute::Package {
                        project: route.project.as_ref().map(|project| project.get().get()),
                        package: route.package.as_str().to_owned(),
                        lane: route.lane.into(),
                    },
                    Route::Page(route) => PersistedRoute::Page {
                        project: route.project.as_ref().map(|project| project.get().get()),
                        package: route.package.as_str().to_owned(),
                        coordinate: route.coordinate.as_str().to_owned(),
                    },
                    Route::Source(route) => PersistedRoute::Source {
                        project: route.project.as_ref().map(|project| project.get().get()),
                        package: route.package.as_str().to_owned(),
                        page: route.page.as_str().to_owned(),
                        line: route.line,
                    },
                },
            },
            settings_page: match snapshot.overlay() {
                Some(Overlay::Settings(page)) => Some(page.as_str().to_owned()),
                Some(Overlay::CommandPalette) | None => None,
            },
        }
    }

    /// Restores only durable session information.  Engine-owned content is
    /// still fetched after the root is admitted.
    pub fn cold_reload(&self, state: &PersistedDesktopState) -> SessionState {
        let settings_page = state
            .settings_page
            .as_deref()
            .and_then(SettingsPage::parse)
            .unwrap_or_default();
        let overlay = match &state.route {
            PersistedRoute::Settings => Some(Overlay::Settings(settings_page)),
            PersistedRoute::Home
            | PersistedRoute::Project { .. }
            | PersistedRoute::Package { .. }
            | PersistedRoute::Page { .. }
            | PersistedRoute::Source { .. } => None,
        };
        let route = match &state.route {
            PersistedRoute::Settings | PersistedRoute::Home => {
                Route::Orbit(crate::navigation::OrbitRoute::Home)
            }
            PersistedRoute::Project { project } => std::num::NonZeroU64::new(*project)
                .map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                })
                .map(crate::navigation::OrbitRoute::Project)
                .map(Route::Orbit)
                .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home)),
            PersistedRoute::Package {
                project,
                package,
                lane,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .map(|package| {
                        Route::Package(crate::navigation::PackageRoute {
                            project,
                            package,
                            lane: (*lane).into(),
                            selected: None,
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::Page {
                project,
                package,
                coordinate,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .zip(Coordinate::new(coordinate).ok())
                    .map(|(package, coordinate)| {
                        Route::Page(crate::navigation::PageRoute {
                            project,
                            package,
                            coordinate,
                            selected: None,
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
            PersistedRoute::Source {
                project,
                package,
                page,
                line,
            } => {
                let project = project.and_then(std::num::NonZeroU64::new).map(|project| {
                    crate::core::ProjectId::from_backend(backend_library::ProjectId::new(project))
                });
                crate::core::PackageId::new(package)
                    .ok()
                    .zip(Coordinate::new(page).ok())
                    .map(|(package, page)| {
                        Route::Source(crate::navigation::SourceRoute {
                            project,
                            package,
                            page,
                            line: *line,
                            selected: None,
                        })
                    })
                    .unwrap_or_else(|| Route::Orbit(crate::navigation::OrbitRoute::Home))
            }
        };
        SessionState {
            route,
            overlay,
            ..SessionState::default()
        }
    }

    /// Restores persistent settings into the immutable snapshot builder.
    #[must_use]
    pub fn cold_settings(&self, state: &PersistedDesktopState) -> SettingsState {
        SettingsState {
            reduced_motion: state.reduced_motion,
            shelf_open: state.shelf_open,
            context_open: state.context_open,
        }
    }

    /// Parses one local shelf identity at the persistence boundary.
    pub fn local_identity(item: &PersistedShelfItem) -> Option<LocalProjectId> {
        LocalProjectId::new(&item.local_path).ok()
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("desktop-state");
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("nudox-v3-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("fixture directory");
        path
    }

    #[test]
    fn cold_reload_reads_a_complete_atomic_state() {
        let root = fixture("persistence");
        let store = PersistentState::at(root.join("desktop.json"));
        let value = PersistedDesktopState {
            shelf: vec![PersistedShelfItem {
                local_path: "/tmp/project".to_owned(),
                label: "project".to_owned(),
            }],
            route: PersistedRoute::Settings,
            settings_page: Some("appearance".to_owned()),
            ..PersistedDesktopState::default()
        };
        store.save(&value).expect("save");
        assert_eq!(store.load().expect("load"), value);
        let session = store.cold_reload(&value);
        assert_eq!(
            session.route,
            Route::Orbit(crate::navigation::OrbitRoute::Home)
        );
        assert_eq!(
            session.overlay,
            Some(Overlay::Settings(SettingsPage::Appearance))
        );
        let settings = store.cold_settings(&value);
        assert!(settings.shelf_open);
        assert!(settings.context_open);
        assert!(PersistentState::local_identity(&value.shelf[0]).is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn command_palette_persists_the_content_route_without_overlay_history() {
        let snapshot = AppSnapshot::empty(crate::core::VersionedRoot::new(
            backend_library::view_state_root(&[("persistence".to_owned(), "route".to_owned())]),
            1,
        ));
        let package = crate::core::PackageId::new("pkg").expect("package");
        let session = SessionState {
            route: Route::Page(crate::navigation::PageRoute {
                project: None,
                package,
                coordinate: Coordinate::new("pkg::Item").expect("coordinate"),
                selected: None,
            }),
            overlay: Some(Overlay::CommandPalette),
            ..SessionState::default()
        };
        let snapshot = snapshot.with_session(session);
        let value = PersistentState::project(&snapshot);
        assert!(matches!(value.route, PersistedRoute::Page { .. }));
        assert_eq!(value.settings_page, None);
        let restored = PersistentState::at("unused").cold_reload(&value);
        assert!(matches!(restored.route, Route::Page(_)));
        assert_eq!(restored.overlay, None);
    }
}
