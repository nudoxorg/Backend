//! A venv constrains selection; only a frozen independent host path permits execution.

use super::CheckerError;
use super::project::{PythonProjectControl, checkpoint, project_error};
use super::project_environment::{controlled_directory_io, hash_image, normal_absolute};
use backend_semantic::vocabulary::PythonVersion;
use std::path::{Path, PathBuf};

/// The complete supported Python profile vocabulary, in ascending version order.
pub const SUPPORTED_PYTHON_VERSIONS: [PythonVersion; 5] = [
    PythonVersion::Python310,
    PythonVersion::Python311,
    PythonVersion::Python312,
    PythonVersion::Python313,
    PythonVersion::Python314,
];

/// Exact major/minor numbers represented by a supported language profile.
pub const fn python_version_numbers(profile: PythonVersion) -> [u32; 2] {
    [
        3,
        match profile {
            PythonVersion::Python310 => 10,
            PythonVersion::Python311 => 11,
            PythonVersion::Python312 => 12,
            PythonVersion::Python313 => 13,
            PythonVersion::Python314 => 14,
        },
    ]
}

pub(super) fn profile_for_interpreter_version(
    version: [u32; 3],
) -> Result<PythonVersion, CheckerError> {
    SUPPORTED_PYTHON_VERSIONS
        .into_iter()
        .find(|profile| python_version_numbers(*profile) == version[..2])
        .ok_or(CheckerError::UnsupportedPythonEnvironmentVersion {
            major: version[0],
            minor: version[1],
        })
}

fn configured_version(configuration: &str) -> Result<PythonVersion, CheckerError> {
    let mut versions = configuration.lines().filter_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == "version").then_some(value.trim())
    });
    let version = versions
        .next()
        .ok_or_else(|| project_error("pyvenv.cfg", "selected venv has no exact Python version"))?;
    if versions.next().is_some() || version.len() > 32 {
        return Err(project_error(
            "pyvenv.cfg",
            "selected venv has an ambiguous Python version",
        ));
    }
    let mut parts = version.split('.');
    let mut number = || {
        parts
            .next()
            .filter(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|part| part.parse::<u32>().ok())
            .ok_or_else(|| {
                project_error(
                    "pyvenv.cfg",
                    "selected venv version is not an exact major.minor.patch",
                )
            })
    };
    let major = number()?;
    let minor = number()?;
    let patch = number()?;
    if parts.next().is_some() {
        return Err(project_error(
            "pyvenv.cfg",
            "selected venv version has extra components",
        ));
    }
    profile_for_interpreter_version([major, minor, patch])
}

/// Inspect-only constraint built from the exact captured venv configuration and image.
/// It cannot mint a host candidate, a native fact, or permission to execute a project path.
#[derive(Clone, Debug)]
pub struct PythonVenvInterpreterRequirement {
    project: PathBuf,
    selected: PathBuf,
    image: PathBuf,
    digest: [u8; 32],
    home_selected: PathBuf,
    home_directory: PathBuf,
    profile: PythonVersion,
}

impl PythonVenvInterpreterRequirement {
    pub(super) fn from_captured(
        project: &Path,
        venv: &Path,
        configuration: &str,
        control: PythonProjectControl<'_>,
    ) -> Result<Self, CheckerError> {
        checkpoint(control)?;
        let profile = configured_version(configuration)?;
        let mut homes = configuration.lines().filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == "home").then_some(value.trim())
        });
        let home_selected = PathBuf::from(homes.next().ok_or_else(|| {
            project_error("pyvenv.cfg", "selected venv has no installation home")
        })?);
        if homes.next().is_some() {
            return Err(project_error(
                "pyvenv.cfg",
                "selected venv has ambiguous installation homes",
            ));
        }
        normal_absolute(project)?;
        normal_absolute(&home_selected)?;
        let home_directory =
            controlled_directory_io(|| home_selected.canonicalize(), || checkpoint(control))?;
        let selected = if cfg!(windows) {
            venv.join("Scripts/python.exe")
        } else {
            venv.join("bin/python")
        };
        let image = controlled_directory_io(|| selected.canonicalize(), || checkpoint(control))?;
        normal_absolute(&image)?;
        let digest = hash_image(&image, control)?;
        Ok(Self {
            project: project.into(),
            selected,
            image,
            digest,
            home_selected,
            home_directory,
            profile,
        })
    }

    /// Requested profile; trusted interpreter metadata must still prove these numbers.
    pub const fn profile(&self) -> PythonVersion {
        self.profile
    }

    /// Check one independently admitted host path without running that path or the venv.
    /// Unix copies are allowed by image digest; Windows launchers are admitted by the
    /// existing trusted-installation template check after their home is matched here.
    pub fn matches_host_candidate(
        &self,
        candidate: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<bool, CheckerError> {
        self.validate_current(control)?;
        normal_absolute(candidate)?;
        let Some(image) =
            canonical_optional_with_io(|| candidate.canonicalize(), || checkpoint(control))?
        else {
            checkpoint(control)?;
            return Ok(false);
        };
        if candidate.starts_with(&self.project) || image.starts_with(&self.project) {
            checkpoint(control)?;
            return Ok(false);
        }
        // This is a configuration witness comparison, never candidate discovery.
        // Framework installations may resolve bin/python to Resources/.../Python.
        // Compare only the finite ordinary names inside the captured home; no
        // path found here becomes an execution capability.
        let names = ["python", "python3", "python.exe", "python3.exe"]
            .into_iter()
            .map(str::to_owned)
            .chain(SUPPORTED_PYTHON_VERSIONS.into_iter().flat_map(|profile| {
                let [major, minor] = python_version_numbers(profile);
                [
                    format!("python{major}.{minor}"),
                    format!("python{major}.{minor}.exe"),
                ]
            }));
        let mut home_matches = false;
        for name in names {
            checkpoint(control)?;
            let home_image = canonical_optional_with_io(
                || self.home_directory.join(name).canonicalize(),
                || checkpoint(control),
            )?;
            if home_image.as_ref() == Some(&image) {
                home_matches = true;
                break;
            }
        }
        checkpoint(control)?;
        if !home_matches {
            return Ok(false);
        }
        let matches = cfg!(windows) || hash_image(&image, control)? == self.digest;
        checkpoint(control)?;
        Ok(matches)
    }

    /// Prove the inspected venv image and home have not changed while selecting a host.
    pub fn validate_current(&self, control: PythonProjectControl<'_>) -> Result<(), CheckerError> {
        checkpoint(control)?;
        let changed =
            controlled_directory_io(|| self.selected.canonicalize(), || checkpoint(control))?
                != self.image
                || controlled_directory_io(
                    || self.home_selected.canonicalize(),
                    || checkpoint(control),
                )? != self.home_directory
                || hash_image(&self.image, control)? != self.digest;
        checkpoint(control)?;
        if changed {
            return Err(project_error(
                "pyvenv.cfg",
                "selected venv changed during host interpreter admission",
            ));
        }
        checkpoint(control)
    }

    pub(super) fn validate_version(&self, version: [u32; 3]) -> Result<(), CheckerError> {
        profile_for_interpreter_version(version)?;
        let [expected_major, expected_minor] = python_version_numbers(self.profile);
        if version[..2] != [expected_major, expected_minor] {
            return Err(CheckerError::PythonEnvironmentVersionMismatch {
                expected_major,
                expected_minor,
                actual_major: version[0],
                actual_minor: version[1],
            });
        }
        Ok(())
    }
}

// Absence is optional only after the shared post-I/O checkpoint has succeeded.
fn canonical_optional_with_io(
    operation: impl FnOnce() -> std::io::Result<PathBuf>,
    check: impl FnMut() -> Result<(), CheckerError>,
) -> Result<Option<PathBuf>, CheckerError> {
    match controlled_directory_io(operation, check) {
        Ok(path) => Ok(Some(path)),
        Err(CheckerError::Workspace { source })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_candidate_io_never_turns_a_stop_into_absence() -> Result<(), CheckerError> {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let cancelled = AtomicBool::new(false);
        let root = super::super::Workspace::create()?;
        for missing in [false, true] {
            cancelled.store(false, Ordering::Release);
            let path = if missing {
                root.path.join("absent-interpreter")
            } else {
                root.path.clone()
            };
            let control = PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            };
            let result = canonical_optional_with_io(
                || {
                    let result = path.canonicalize();
                    cancelled.store(true, Ordering::Release);
                    result
                },
                || checkpoint(control),
            );
            assert!(matches!(result, Err(CheckerError::Cancelled { .. })));
            cancelled.store(false, Ordering::Release);
            // Advance the actual checkpoint clock at the I/O boundary, without sleeping.
            let mut deadline = Instant::now() + Duration::from_secs(30);
            let expired = Instant::now();
            let mut checks = 0;
            let result = canonical_optional_with_io(
                || path.canonicalize(),
                || {
                    checks += 1;
                    if checks == 2 {
                        deadline = expired;
                    }
                    checkpoint(PythonProjectControl {
                        cancelled: &cancelled,
                        deadline,
                    })
                },
            );
            assert!(matches!(result, Err(CheckerError::Deadline { .. })));
            assert_eq!(checks, 2);
            assert_eq!(
                canonical_optional_with_io(|| path.canonicalize(), || Ok(()))?.is_none(),
                missing
            );
        }
        Ok(())
    }

    #[test]
    fn every_supported_minor_has_one_exact_profile_and_patch_is_not_a_dialect() {
        for profile in SUPPORTED_PYTHON_VERSIONS {
            let [major, minor] = python_version_numbers(profile);
            assert_eq!(
                configured_version(&format!("version = {major}.{minor}.7\n")).unwrap(),
                profile
            );
            assert_eq!(
                profile_for_interpreter_version([major, minor, 9]).unwrap(),
                profile
            );
        }
        for [major, minor] in [[2, 7], [3, 9], [3, 15], [4, 12]] {
            assert!(
                matches!(profile_for_interpreter_version([major, minor, 0]), Err(CheckerError::UnsupportedPythonEnvironmentVersion { major: actual_major, minor: actual_minor }) if major == actual_major && minor == actual_minor)
            );
        }
    }

    #[test]
    fn missing_conflicting_or_nonliteral_versions_cannot_choose_a_profile() {
        for configuration in [
            "",
            "version = 3.12\n",
            "version = 3.12.3\nversion = 3.14.0\n",
            "version = python3.12.3\n",
            "version = 3.12.3.0\n",
        ] {
            assert!(
                configured_version(configuration).is_err(),
                "{configuration:?}"
            );
        }
    }
}
