//! Request-time interpreter admission for captured installed environments.
//! Host inputs are frozen at owner startup; no project path or later ambient
//! PATH value participates. Native checker admission stays independent.

use super::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostPathRole, LocalHostVariable, PLATFORM_PATH_CAPACITY,
};
use arrayvec::ArrayVec;
use backend_frontend_python::legacy::checker::{
    CheckerError, PythonEnvironmentCapture, PythonInterpreterSelection, PythonProjectControl,
    PythonVenvInterpreterRequirement, SUPPORTED_PYTHON_VERSIONS, python_version_numbers,
};
use backend_semantic::vocabulary::NativeTool;
use std::path::{Path, PathBuf};

/// Candidate roles preserve independent startup selection and its origin. Older closed
/// snapshots containing only NUDOX_PYTHON keep their strict singleton meaning.
pub(super) const INSTALLED_PYTHON_ROLES: [LocalHostVariable; PLATFORM_PATH_CAPACITY] = [
    LocalHostVariable::NudoxPythonDefault,
    LocalHostVariable::NudoxPythonCandidate1,
    LocalHostVariable::NudoxPythonCandidate2,
    LocalHostVariable::NudoxPythonCandidate3,
    LocalHostVariable::NudoxPythonCandidate4,
    LocalHostVariable::NudoxPythonCandidate5,
    LocalHostVariable::NudoxPythonCandidate6,
    LocalHostVariable::NudoxPythonCandidate7,
    LocalHostVariable::NudoxPythonCandidate8,
    LocalHostVariable::NudoxPythonCandidate9,
    LocalHostVariable::NudoxPythonCandidate10,
    LocalHostVariable::NudoxPythonCandidate11,
];

impl<Environment: LocalHostEnvironment> LocalCompilerHost<Environment> {
    /// Freezes independent interpreter candidates without starting an owner or probe.
    ///
    /// # Errors
    /// Rejects a relative installation home. Selected-path failures remain deferred
    /// until a project actually requests an installed environment.
    pub fn capture_python_interpreter_admission(
        &self,
    ) -> Result<PythonInterpreterHostAdmission, LocalCompilerHostError> {
        let frozen = LocalCompilerHost::new(
            super::CapturedLocalHostEnvironment::capture(&self.environment),
            self.discovery,
        );
        let home = frozen.optional_absolute_path(LocalHostVariable::Home)?;
        Ok(PythonInterpreterHostAdmission::capture(
            &frozen,
            home.as_deref(),
        ))
    }

    fn frozen_python_paths(
        &self,
        home: Option<&Path>,
    ) -> Result<ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>, LocalCompilerHostError> {
        let mut selected = ArrayVec::new();
        if let Some(path) = self.optional_absolute_path(LocalHostVariable::NudoxPython)? {
            // A configured spelling is a frozen constraint, not an executed image.
            // Negative/source-only projects must not inspect an unused explicit path.
            selected.push(path);
            return Ok(selected);
        }
        if self.discovery == LocalHostDiscovery::ClosedSnapshot {
            for role in INSTALLED_PYTHON_ROLES {
                if let Some(path) = self.optional_absolute_path(role)? {
                    selected.push(path);
                }
            }
            return Ok(selected);
        }
        if self.discovery == LocalHostDiscovery::ExplicitOnly {
            return Ok(selected);
        }
        // Frozen process policy supplies these directories, never pyvenv.cfg or project paths.
        // Walk each bounded PATH entry so Windows installations named python.exe are retained.
        let names: Vec<String> = [
            if cfg!(windows) {
                "python.exe"
            } else {
                "python3"
            },
            if cfg!(windows) {
                "python3.exe"
            } else {
                "python"
            },
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(SUPPORTED_PYTHON_VERSIONS.into_iter().map(|version| {
            let [major, minor] = python_version_numbers(version);
            format!(
                "python{major}.{minor}{}",
                if cfg!(windows) { ".exe" } else { "" }
            )
        }))
        .collect();
        if self.discovery == LocalHostDiscovery::InstalledTools
            && let Some(search) = self
                .environment
                .search_path()
                .filter(|search| search.len() <= 64 * 1024)
        {
            for directory in std::env::split_paths(&search)
                .take(256)
                .filter(|directory| directory.is_absolute())
            {
                for name in &names {
                    admit_candidate(&mut selected, directory.join(name))?;
                }
            }
        }
        let defaults = self.executable_candidates(home, NativeTool::Python);
        for path in defaults {
            if let Some(directory) = path.parent() {
                for name in &names {
                    admit_candidate(&mut selected, directory.join(name))?;
                }
            }
        }
        Ok(selected)
    }

    pub(super) fn capture_installed_python_paths(
        &self,
        paths: &mut Vec<(LocalHostVariable, PathBuf)>,
        home: Option<&Path>,
    ) -> Result<(), LocalCompilerHostError> {
        let explicit = self
            .environment
            .value(LocalHostVariable::NudoxPython)
            .is_some();
        let selected = self.frozen_python_paths(home)?;
        if explicit {
            if let Some(path) = selected.into_iter().next() {
                paths.push((
                    LocalHostVariable::NudoxPython,
                    self.canonical_configured_selection_path(LocalHostVariable::NudoxPython, path)?,
                ));
            }
        } else {
            paths.extend(INSTALLED_PYTHON_ROLES.into_iter().zip(selected));
        }
        Ok(())
    }
}

fn admit_candidate(
    selected: &mut ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
    path: PathBuf,
) -> Result<(), LocalCompilerHostError> {
    let role = LocalHostPathRole::Native(NativeTool::Python);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(LocalCompilerHostError::Canonicalize {
                role,
                path: path.into_boxed_path(),
                source,
            });
        }
    }
    let path = super::paths::canonicalize_executable_existing(role, &path)?;
    if !selected.contains(&path) {
        selected
            .try_push(path)
            .map_err(|_| LocalCompilerHostError::PythonCandidateCapacity {
                maximum: PLATFORM_PATH_CAPACITY,
            })?;
    }
    Ok(())
}

/// An independently frozen, finite set of host paths. Construction executes nothing.
#[derive(Debug)]
pub struct PythonInterpreterHostAdmission {
    paths: Result<ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>, LocalCompilerHostError>,
    strict: bool,
}
impl PythonInterpreterHostAdmission {
    pub(super) fn capture<Environment: LocalHostEnvironment>(
        host: &LocalCompilerHost<Environment>,
        home: Option<&Path>,
    ) -> Self {
        // Interpreter SDK discovery is independent of native-checker selection.
        // Preserve the existing ordinary installed-environment policy for native
        // ExplicitOnly callers; closed snapshots never rediscover ambient paths.
        let discovery = match host.discovery {
            LocalHostDiscovery::ExplicitOnly => LocalHostDiscovery::InstalledTools,
            other => other,
        };
        let frozen = LocalCompilerHost::new(
            super::CapturedLocalHostEnvironment::capture(&host.environment),
            discovery,
        );
        Self {
            strict: frozen
                .environment
                .value(LocalHostVariable::NudoxPython)
                .is_some(),
            paths: frozen.frozen_python_paths(home),
        }
    }
    fn resolve(
        &self,
        requirement: &PythonVenvInterpreterRequirement,
        control: PythonProjectControl<'_>,
    ) -> Result<Option<PythonInterpreterSelection>, CheckerError> {
        let checkpoint = || {
            if control.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(CheckerError::Cancelled {
                    phase: "host-interpreter-selection",
                });
            }
            if std::time::Instant::now() >= control.deadline {
                return Err(CheckerError::Deadline {
                    phase: "host-interpreter-selection",
                });
            }
            Ok(())
        };
        checkpoint()?;
        let paths = self
            .paths
            .as_ref()
            .map_err(|error| CheckerError::Workspace {
                source: std::io::Error::other(error.to_string()),
            })?;
        for path in paths {
            if requirement.matches_host_candidate(path, control)? {
                let result = PythonInterpreterSelection::from_host_policy(path.clone());
                checkpoint()?;
                return result.map(Some);
            }
            if self.strict {
                let [major, minor] = python_version_numbers(requirement.profile());
                checkpoint()?;
                return Err(CheckerError::PythonInterpreterSelectionMismatch {
                    path: path.clone(),
                    major,
                    minor,
                });
            }
        }
        checkpoint()?;
        let [major, minor] = python_version_numbers(requirement.profile());
        Err(CheckerError::PythonInterpreterSelectionUnavailable { major, minor })
    }
    /// Negative/source-only capture never invokes the matching provider or metadata probe.
    pub fn capture_project(
        &self,
        project: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<PythonEnvironmentCapture, CheckerError> {
        PythonEnvironmentCapture::capture_with_matching_interpreter_provider(
            project,
            |requirement| self.resolve(requirement, control),
            control,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    struct Configured {
        selected: PathBuf,
    }
    impl LocalHostEnvironment for Configured {
        fn value(&self, role: LocalHostVariable) -> Option<OsString> {
            (role == LocalHostVariable::NudoxPython)
                .then(|| self.selected.as_os_str().to_os_string())
        }
    }
    #[test]
    fn unused_configured_path_is_frozen_without_filesystem_admission() {
        let path = if cfg!(windows) {
            PathBuf::from("C:/absent-host/python.exe")
        } else {
            PathBuf::from("/absent-host/python3")
        };
        for discovery in [
            LocalHostDiscovery::ExplicitOnly,
            LocalHostDiscovery::ClosedSnapshot,
        ] {
            let host = LocalCompilerHost::new(
                Configured {
                    selected: path.clone(),
                },
                discovery,
            );
            let admission = host.capture_python_interpreter_admission().unwrap();
            assert!(admission.strict);
            assert_eq!(admission.paths.unwrap().as_slice(), &[path.clone()]);
        }
    }
}
