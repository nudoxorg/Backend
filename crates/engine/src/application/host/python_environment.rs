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
};
use backend_semantic::vocabulary::NativeTool;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct FrozenInterpreterEnvironment {
    explicit: Option<OsString>,
    search_path: Option<OsString>,
}
impl LocalHostEnvironment for FrozenInterpreterEnvironment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxPython)
            .then(|| self.explicit.clone())
            .flatten()
    }
    fn search_path(&self) -> Option<OsString> {
        self.search_path.clone()
    }
}

/// Independent frozen host policy used only by an installed Python request.
/// Construction copies path inputs and finite candidates; it executes nothing.
#[derive(Debug)]
pub struct PythonInterpreterHostAdmission {
    host: LocalCompilerHost<FrozenInterpreterEnvironment>,
    candidates: ArrayVec<PathBuf, PLATFORM_PATH_CAPACITY>,
}
impl PythonInterpreterHostAdmission {
    pub(super) fn capture<Environment: LocalHostEnvironment>(
        host: &LocalCompilerHost<Environment>,
        home: Option<&Path>,
    ) -> Self {
        // Ordinary installed dependencies use the documented installed-tool
        // policy, independently of whether the checker is native or external.
        // Explicit closed snapshots retain their exact no-discovery contract.
        let discovery = match host.discovery {
            LocalHostDiscovery::ClosedSnapshot => LocalHostDiscovery::ClosedSnapshot,
            LocalHostDiscovery::ExplicitOnly | LocalHostDiscovery::InstalledTools => {
                LocalHostDiscovery::InstalledTools
            }
            LocalHostDiscovery::PlatformDefaults => LocalHostDiscovery::PlatformDefaults,
        };
        Self {
            host: LocalCompilerHost::new(
                FrozenInterpreterEnvironment {
                    explicit: host.environment.value(LocalHostVariable::NudoxPython),
                    search_path: if discovery == LocalHostDiscovery::InstalledTools {
                        host.environment.search_path()
                    } else {
                        None
                    },
                },
                discovery,
            ),
            candidates: host.executable_candidates(home, NativeTool::Python),
        }
    }
    fn resolve(
        &self,
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
        let selected = self
            .host
            .executable(
                LocalHostVariable::NudoxPython,
                LocalHostPathRole::Native(NativeTool::Python),
                self.candidates.clone(),
            )
            .map_err(|error: LocalCompilerHostError| CheckerError::Workspace {
                source: std::io::Error::other(error.to_string()),
            })?;
        checkpoint()?;
        selected
            .map(PythonInterpreterSelection::from_host_policy)
            .transpose()
    }
    /// Capture the exact installed cohort; source-only requests never resolve
    /// this policy or run interpreter metadata probes.
    pub fn capture_project(
        &self,
        project: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<PythonEnvironmentCapture, CheckerError> {
        PythonEnvironmentCapture::capture_with_interpreter_provider(
            project,
            || self.resolve(control),
            control,
        )
    }
}
