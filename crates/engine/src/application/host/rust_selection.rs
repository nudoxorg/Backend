//! Shared finite installed Rust pair selection for every local service surface.

use std::{ffi::OsString, path::PathBuf};

/// Where a person's Rust was found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledToolPlace {
    /// The process named it (`NUDOX_RUSTC`: the development shell, an operator).
    Named,
    /// A directory on the process's `PATH`.
    Path,
    /// rustup's proxies (`$CARGO_HOME/bin`, else `~/.cargo/bin`).
    Rustup,
    /// Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`).
    Homebrew,
    /// A Nix profile.
    Nix,
    /// Exact Rust path supplied by a closed compiler environment, without rediscovery.
    ClosedSnapshot,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    struct RustEnvironment {
        home: PathBuf,
        bin: PathBuf,
        cargo: Option<PathBuf>,
        cargo_home: Option<OsString>,
        typescript: Option<PathBuf>,
        configured: Option<(LocalHostVariable, PathBuf)>,
    }
    impl LocalHostEnvironment for RustEnvironment {
        fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
            match variable {
                LocalHostVariable::Home => Some(self.home.clone().into_os_string()),
                LocalHostVariable::NudoxCargo => self.cargo.clone().map(PathBuf::into_os_string),
                LocalHostVariable::NudoxTypeScriptCompiler => {
                    self.typescript.clone().map(PathBuf::into_os_string)
                }
                _ => None,
            }
            .or_else(|| {
                self.configured
                    .as_ref()
                    .filter(|(key, _)| *key == variable)
                    .map(|(_, path)| path.clone().into_os_string())
            })
        }
        fn search_path(&self) -> Option<OsString> {
            Some(self.bin.clone().into_os_string())
        }
        fn cargo_home(&self) -> Option<OsString> {
            self.cargo_home.clone()
        }
    }

    #[test]
    fn shared_rust_pair_prepares_only_its_default_cache_and_retains_explicit_failures() {
        let root = std::env::temp_dir().join(format!(
            "nudox-shared-rust-{}-{}",
            std::process::id(),
            super::super::NEXT_NATIVE_WORK.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("fixture root");
        let root = std::fs::canonicalize(root).expect("canonical fixture root");
        let home = root.join("home");
        let bin = root.join("bin");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&bin).expect("bin");
        for tool in ["rustc", "cargo"] {
            let path = bin.join(tool);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("tool");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("tool permissions");
        }
        let make = |cargo, cargo_home| {
            LocalCompilerHost::new(
                RustEnvironment {
                    home: home.clone(),
                    bin: bin.clone(),
                    cargo,
                    cargo_home,
                    typescript: None,
                    configured: None,
                },
                super::super::LocalHostDiscovery::InstalledTools,
            )
        };
        let mut paths = Vec::new();
        let invalid_cargo = root.join("configured/missing-cargo");
        paths.push((LocalHostVariable::NudoxCargo, invalid_cargo.clone()));
        assert!(matches!(
            make(Some(invalid_cargo), None).capture_installed_rust_paths(&mut paths, Some(&home)),
            Err(LocalCompilerHostError::ConfiguredPath {
                variable: LocalHostVariable::NudoxCargo,
                ..
            })
        ));
        assert!(
            !home.join(".cargo").exists(),
            "invalid explicit compiler cannot create a cache"
        );
        let mut paths = Vec::new();
        assert!(matches!(
            make(None, Some(OsString::new())).capture_installed_rust_paths(&mut paths, Some(&home)),
            Err(LocalCompilerHostError::RelativeEnvironmentPath {
                variable: LocalHostVariable::NudoxCargoHome,
                ..
            })
        ));
        assert!(!home.join(".cargo").exists());
        let bad_typescript = LocalCompilerHost::new(
            RustEnvironment {
                home: home.clone(),
                bin: bin.clone(),
                cargo: None,
                cargo_home: None,
                typescript: Some(root.join("configured/missing-tsc")),
                configured: None,
            },
            super::super::LocalHostDiscovery::InstalledTools,
        );
        assert!(matches!(
            bad_typescript.capture_installed_selection(),
            Err(LocalCompilerHostError::ConfiguredPath {
                variable: LocalHostVariable::NudoxTypeScriptCompiler,
                ..
            })
        ));
        assert!(
            !home.join(".cargo").exists(),
            "unrelated invalid explicit SDK cannot realize Rust state"
        );
        for variable in [
            LocalHostVariable::NudoxTypeScriptReportProgram,
            LocalHostVariable::NudoxJavaCompiler,
            LocalHostVariable::LibclangPath,
            LocalHostVariable::NudoxNpmRoot,
        ] {
            let host = LocalCompilerHost::new(
                RustEnvironment {
                    home: home.clone(),
                    bin: bin.clone(),
                    cargo: None,
                    cargo_home: None,
                    typescript: None,
                    configured: Some((variable, root.join("configured/missing-object"))),
                },
                super::super::LocalHostDiscovery::InstalledTools,
            );
            assert!(matches!(host.capture_installed_selection(),
                Err(LocalCompilerHostError::ConfiguredPath { variable: failed, .. }) if failed == variable));
            assert!(
                !home.join(".cargo").exists(),
                "invalid named {variable:?} cannot realize cache state"
            );
        }
        let mut paths = Vec::new();
        make(None, None)
            .capture_installed_rust_paths(&mut paths, Some(&home))
            .expect("shared default pair");
        assert!(paths.contains(&(LocalHostVariable::NudoxRustc, bin.join("rustc"))));
        assert!(paths.contains(&(LocalHostVariable::NudoxCargo, bin.join("cargo"))));
        make(None, None)
            .realize_installed_rust_cache(&paths, Some(&home))
            .expect("realize selected default");
        let cache = home.join(".cargo");
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::write(cache.join("owned-marker"), "preserve").expect("user content");
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o755))
            .expect("existing user permissions");
        make(None, None)
            .capture_installed_rust_paths(&mut Vec::new(), Some(&home))
            .expect("repeat capture");
        assert_eq!(
            std::fs::read_to_string(cache.join("owned-marker")).unwrap(),
            "preserve"
        );
        assert_eq!(
            std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_dir_all(root).expect("owned fixture cleanup");
    }
}

impl InstalledToolPlace {
    /// How a person names it.
    pub const fn words(self) -> &'static str {
        match self {
            Self::Named => "as configured",
            Self::Path => "on your PATH",
            Self::Rustup => "from rustup",
            Self::Homebrew => "from Homebrew",
            Self::Nix => "from Nix",
            Self::ClosedSnapshot => "from the closed compiler environment",
        }
    }
}

/// Whether absence was discovered or explicitly sealed by a closed launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledRustSelectionSource {
    /// The installed-tool policy inspected its finite search locations.
    InstalledTools,
    /// The incoming closed environment omitted Rust; no search was performed.
    ClosedSnapshot,
}

/// The Rust selected by this launch. This report does not assert compiler capability readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstalledRustToolchain {
    /// A `rustc` with its `cargo` beside it.
    Found {
        rustc: PathBuf,
        place: InstalledToolPlace,
        /// `rustc --version`'s first line, when it answered.
        version: Option<String>,
    },
    /// No directory it looked in held both; each looked-in directory, in order.
    Missing {
        /// Exact finite directories inspected; empty for a closed launch.
        looked: Vec<PathBuf>,
        /// Whether absence came from discovery or the sealed incoming selection.
        source: InstalledRustSelectionSource,
    },
}

impl InstalledRustToolchain {
    /// Reports only the incoming selection, without filesystem discovery or version probes.
    /// Closed snapshots do not record a Rust version proof, so the version remains absent.
    #[must_use]
    pub fn from_closed_snapshot(snapshot: &super::ClosedLocalHostEnvironmentSnapshot) -> Self {
        match snapshot.path(LocalHostVariable::NudoxRustc) {
            Some(rustc) => Self::Found {
                rustc: rustc.to_path_buf(),
                place: InstalledToolPlace::ClosedSnapshot,
                version: None,
            },
            None => Self::Missing {
                looked: Vec::new(),
                source: InstalledRustSelectionSource::ClosedSnapshot,
            },
        }
    }

    /// What the window says about it.
    pub fn words(&self) -> String {
        match self {
            Self::Found { rustc, place, version } => {
                let name = version.as_deref().and_then(|version| version.split_whitespace().nth(1)).map_or_else(|| "Rust".to_owned(), |number| format!("Rust {number}"));
                format!("{name} at {} ({})", rustc.display(), place.words())
            }
            Self::Missing { source: InstalledRustSelectionSource::ClosedSnapshot, .. } => {
                "Rust is absent from the closed compiler environment.".to_owned()
            }
            Self::Missing { .. } => {
                "No Rust toolchain was found. Install one with rustup (rustup.rs) or Homebrew (brew install rust), then quit and reopen Nudox.".to_owned()
            }
        }
    }
}

/// Directories people install Rust into outside their home, in the order they
/// are tried after the process's `PATH` and rustup's.
pub(crate) const SYSTEM_BINS: [(&str, InstalledToolPlace); 5] = [
    ("/opt/homebrew/bin", InstalledToolPlace::Homebrew),
    ("/usr/local/bin", InstalledToolPlace::Homebrew),
    ("/run/current-system/sw/bin", InstalledToolPlace::Nix),
    ("/nix/var/nix/profiles/default/bin", InstalledToolPlace::Nix),
    ("/etc/profiles/per-user", InstalledToolPlace::Nix),
];

/// Returns the shared finite system locations used after PATH and rustup.
#[must_use]
pub fn installed_rust_system_locations() -> Vec<(PathBuf, InstalledToolPlace)> {
    SYSTEM_BINS
        .iter()
        .map(|(path, place)| (PathBuf::from(path), *place))
        .collect()
}

/// The file name a `tool` executable has on this platform (`rustc.exe` on
/// Windows, `rustc` elsewhere).
fn executable(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

/// Typed launch inputs used to select one installed Rust pair.
#[derive(Clone, Debug)]
pub struct InstalledRustInputs {
    /// Exact NUDOX_RUSTC, including invalid explicitly supplied values.
    pub configured_rustc: Option<OsString>,
    /// The captured native user home.
    pub home: Option<OsString>,
    /// Exact CARGO_HOME when supplied.
    pub cargo_home: Option<OsString>,
    /// Captured launch executable search path; never a child process input.
    pub search_path: Option<OsString>,
}

/// The first directory that holds both `rustc` and `cargo`: an explicit
/// `NUDOX_RUSTC`, else the process's `PATH`, rustup's proxies, Homebrew, Nix.
/// `system` is [`SYSTEM_BINS`] in production (a test gives its own).
pub fn find_installed_rust(
    inputs: &InstalledRustInputs,
    system: &[(PathBuf, InstalledToolPlace)],
) -> InstalledRustToolchain {
    if let Some(rustc) = &inputs.configured_rustc {
        return InstalledRustToolchain::Found {
            rustc: PathBuf::from(rustc),
            place: InstalledToolPlace::Named,
            version: None,
        };
    }
    let home = inputs
        .home
        .as_ref()
        .map(PathBuf::from)
        .filter(|home| home.is_absolute());
    let mut places = Vec::new();
    if let Some(path) = inputs
        .search_path
        .clone()
        .filter(|path| path.len() <= 64 * 1024)
    {
        places.extend(
            std::env::split_paths(&path)
                .take(256)
                .filter(|dir| dir.is_absolute())
                .map(|dir| (dir, InstalledToolPlace::Path)),
        );
    }
    let cargo_home = inputs
        .cargo_home
        .clone()
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".cargo")));
    if let Some(cargo_home) = cargo_home {
        places.push((cargo_home.join("bin"), InstalledToolPlace::Rustup));
    }
    for (dir, place) in system {
        // `/etc/profiles/per-user` is per user: NixOS and nix-darwin's.
        let dir = if dir.ends_with("per-user") {
            match home.as_ref().and_then(|home| home.file_name()) {
                Some(user) => dir.join(user).join("bin"),
                None => continue,
            }
        } else {
            dir.clone()
        };
        places.push((dir, *place));
    }
    if let Some(home) = &home {
        places.push((home.join(".nix-profile/bin"), InstalledToolPlace::Nix));
    }
    let mut looked = Vec::new();
    for (dir, place) in places {
        if looked.contains(&dir) {
            continue;
        }
        let (rustc, cargo) = (dir.join(executable("rustc")), dir.join(executable("cargo")));
        if rustc.is_file() && cargo.is_file() {
            return InstalledRustToolchain::Found {
                rustc,
                place,
                version: None,
            };
        }
        looked.push(dir);
    }
    InstalledRustToolchain::Missing {
        looked,
        source: InstalledRustSelectionSource::InstalledTools,
    }
}

use super::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostEnvironment, LocalHostPathRole,
    LocalHostVariable,
};
use backend_semantic::vocabulary::NativeTool;
use std::path::Path;

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    /// Realize only the inferred Cargo home after every launch path has been admitted.
    pub(super) fn realize_installed_rust_cache(
        &self,
        paths: &[(LocalHostVariable, PathBuf)],
        home: Option<&Path>,
    ) -> Result<(), LocalCompilerHostError> {
        if self
            .environment
            .value(LocalHostVariable::NudoxCargoHome)
            .is_some()
            || self.environment.cargo_home().is_some()
        {
            return Ok(());
        }
        let selected = |variable| {
            paths
                .iter()
                .find(|(key, _)| *key == variable)
                .map(|(_, path)| path.as_path())
        };
        let (Some(rustc), Some(cargo), Some(cache), Some(home)) = (
            selected(LocalHostVariable::NudoxRustc),
            selected(LocalHostVariable::NudoxCargo),
            selected(LocalHostVariable::NudoxCargoHome),
            home,
        ) else {
            return Ok(());
        };
        if cache != home.join(".cargo") || cache.exists() || !rustc.is_file() || !cargo.is_file() {
            return Ok(());
        }
        backend_platform::durable::ensure_private_child_directory(cache).map_err(|source| {
            LocalCompilerHostError::ConfiguredPath {
                role: LocalHostPathRole::CargoHome,
                variable: LocalHostVariable::NudoxCargoHome,
                path: cache.to_path_buf().into_boxed_path(),
                source,
            }
        })
    }

    /// Complete the same installed Rust pair for every local service surface.
    pub(super) fn capture_installed_rust_paths(
        &self,
        paths: &mut Vec<(LocalHostVariable, PathBuf)>,
        home: Option<&Path>,
    ) -> Result<(), LocalCompilerHostError> {
        let inputs = InstalledRustInputs {
            configured_rustc: self.environment.value(LocalHostVariable::NudoxRustc),
            home: home.map(|path| path.as_os_str().to_owned()),
            cargo_home: self.environment.cargo_home(),
            search_path: self.environment.search_path(),
        };
        let system = installed_rust_system_locations();
        let InstalledRustToolchain::Found { rustc, .. } = find_installed_rust(&inputs, &system)
        else {
            return Ok(());
        };
        if !paths
            .iter()
            .any(|(key, _)| *key == LocalHostVariable::NudoxRustc)
        {
            paths.push((LocalHostVariable::NudoxRustc, rustc.clone()));
        }
        let cargo = rustc.with_file_name(executable("cargo"));
        if !paths
            .iter()
            .any(|(key, _)| *key == LocalHostVariable::NudoxCargo)
            && cargo.is_absolute()
            && cargo.is_file()
        {
            paths.push((LocalHostVariable::NudoxCargo, cargo.clone()));
        }
        // Explicit compiler errors remain exact and are handled by normal owner admission.
        // Canonicalize inferred paired paths now so GUI and CLI hand off identical selections.
        for (variable, role) in [
            (
                LocalHostVariable::NudoxRustc,
                LocalHostPathRole::Native(NativeTool::Rustc),
            ),
            (LocalHostVariable::NudoxCargo, LocalHostPathRole::Cargo),
        ] {
            if let Some((_, path)) = paths.iter_mut().find(|(key, _)| *key == variable) {
                *path = match self.executable(variable, role, arrayvec::ArrayVec::new())? {
                    Some(configured) => configured,
                    None => super::paths::canonicalize_executable_existing(role, path)?,
                };
            }
        }
        if !paths
            .iter()
            .any(|(key, _)| *key == LocalHostVariable::NudoxCargoHome)
        {
            let configured = self.environment.cargo_home();
            let cargo_home = configured
                .map(PathBuf::from)
                .or_else(|| home.map(|home| home.join(".cargo")));
            if let Some(cargo_home) = cargo_home {
                if !cargo_home.is_absolute() {
                    return Err(LocalCompilerHostError::RelativeEnvironmentPath {
                        variable: LocalHostVariable::NudoxCargoHome,
                        path: cargo_home.into_boxed_path(),
                    });
                }
                paths.push((LocalHostVariable::NudoxCargoHome, cargo_home));
            }
        }
        Ok(())
    }
}
