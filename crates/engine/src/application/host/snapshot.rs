//! Closed, versioned compiler-host environment snapshots.

use super::LocalHostVariable;
use backend_platform::{NativePath, NativePathError, NativePathWire};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Maximum encoded bytes accepted or emitted by a closed compiler-host snapshot.
///
/// The fixed bound limits parsing and keeps the single inherited environment value finite. Paths
/// use the exact native-unit representation from [`NativePathWire`], so a snapshot never relies
/// on lossy display text.
pub const MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES: usize = 30 * 1024;

const SNAPSHOT_VERSION: u8 = 1;
const SNAPSHOT_PREFIX_BYTES: usize = br#"{"version":1,"paths":["#.len();
const SNAPSHOT_SUFFIX_BYTES: usize = 2; // `]}`
const ENTRY_FIXED_BYTES: usize = br#"{"variable":"","path":}"#.len();

/// How the current local owner obtained its compiler-host paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCompilerHostSelectionSource {
    /// locald captured installed paths from this process before the owner began serving.
    CapturedInstalledTools,
    /// The launching host supplied a complete version-1 closed snapshot.
    IncomingClosedSnapshot,
}

/// Actionable missing-tool states retained with the immutable owner selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCompilerHostSelectionIssue {
    /// No Node runtime was captured for project-local TypeScript.
    MissingTypeScriptNode,
    /// A captured global TypeScript script has no package module root.
    MissingTypeScriptModuleRoot,
    /// No Python interpreter was captured.
    MissingPythonInterpreter,
    /// No Pyrefly checker was captured for Python authority.
    MissingPyreflyChecker,
    /// No Go compiler was captured.
    MissingGoCompiler,
    /// A captured Go compiler has no package module cache.
    MissingGoModuleCache,
}

impl LocalCompilerHostSelectionIssue {
    /// Returns concise setup guidance suitable for an owner status doctor.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::MissingTypeScriptNode => {
                "Install Node.js or configure NUDOX_TYPESCRIPT_NODE, then restart locald."
            }
            Self::MissingTypeScriptModuleRoot => {
                "Install the TypeScript package or configure NUDOX_TYPESCRIPT_MODULE_ROOT, then restart locald."
            }
            Self::MissingPythonInterpreter => {
                "Install Python 3 or configure NUDOX_PYTHON, then restart locald."
            }
            Self::MissingPyreflyChecker => {
                "Install Pyrefly or configure NUDOX_PYREFLY, then restart locald."
            }
            Self::MissingGoCompiler => "Install Go or configure NUDOX_GO, then restart locald.",
            Self::MissingGoModuleCache => {
                "Configure GOMODCACHE, GOPATH, or NUDOX_GO_ROOT, then restart locald."
            }
        }
    }
}

/// Immutable owner-startup selection receipt: a closed path snapshot, its source, and a
/// reproducible fingerprint. The source domain is part of the fingerprint, so an incoming
/// explicit snapshot is distinguishable from an ambient selection even when their paths match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCompilerHostSelection {
    snapshot: ClosedLocalHostEnvironmentSnapshot,
    source: LocalCompilerHostSelectionSource,
    fingerprint: [u8; 32],
    issues: Vec<LocalCompilerHostSelectionIssue>,
}

impl LocalCompilerHostSelection {
    /// Wraps a caller-supplied snapshot as a closed input. This constructor never discovers or
    /// repairs missing paths.
    ///
    /// # Errors
    ///
    /// Returns a snapshot serialization error.
    pub fn from_closed_snapshot(
        snapshot: ClosedLocalHostEnvironmentSnapshot,
    ) -> Result<Self, ClosedLocalHostEnvironmentSnapshotError> {
        Self::new(
            snapshot,
            LocalCompilerHostSelectionSource::IncomingClosedSnapshot,
        )
    }

    pub(super) fn captured_installed_tools(
        snapshot: ClosedLocalHostEnvironmentSnapshot,
    ) -> Result<Self, ClosedLocalHostEnvironmentSnapshotError> {
        Self::new(
            snapshot,
            LocalCompilerHostSelectionSource::CapturedInstalledTools,
        )
    }

    fn new(
        snapshot: ClosedLocalHostEnvironmentSnapshot,
        source: LocalCompilerHostSelectionSource,
    ) -> Result<Self, ClosedLocalHostEnvironmentSnapshotError> {
        let encoded = snapshot.encode()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend-local-compiler-host-selection-v1\0");
        hasher.update(&[match source {
            LocalCompilerHostSelectionSource::CapturedInstalledTools => 1,
            LocalCompilerHostSelectionSource::IncomingClosedSnapshot => 2,
        }]);
        hasher.update(encoded.as_bytes());
        let fingerprint = *hasher.finalize().as_bytes();
        let issues = selection_issues(&snapshot);
        Ok(Self {
            snapshot,
            source,
            fingerprint,
            issues,
        })
    }

    /// Returns the exact closed path set supplied to the owner.
    #[must_use]
    pub fn snapshot(&self) -> &ClosedLocalHostEnvironmentSnapshot {
        &self.snapshot
    }

    /// Returns whether paths were captured from the locald process or supplied by its launcher.
    #[must_use]
    pub const fn source(&self) -> LocalCompilerHostSelectionSource {
        self.source
    }

    /// Returns the deterministic BLAKE3 witness over source mode and canonical snapshot bytes.
    #[must_use]
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// Returns the lower-case hexadecimal fingerprint for a status receipt.
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        hex_fingerprint(&self.fingerprint)
    }

    /// Returns setup issues found without changing or filling the closed snapshot.
    #[must_use]
    pub fn issues(&self) -> &[LocalCompilerHostSelectionIssue] {
        &self.issues
    }

    /// Encodes a bounded receipt suitable for private locald state and a setup doctor.
    ///
    /// The nested snapshot keeps the receipt's native path encoding lossless. It is data, not a
    /// second input source: only the already selected paths are recorded.
    ///
    /// # Errors
    ///
    /// Returns a snapshot serialization error.
    pub fn encode_receipt(&self) -> Result<String, ClosedLocalHostEnvironmentSnapshotError> {
        #[derive(Serialize)]
        struct Receipt<'a> {
            version: u8,
            source: LocalCompilerHostSelectionSource,
            fingerprint: String,
            snapshot: String,
            issues: &'a [LocalCompilerHostSelectionIssue],
        }

        let receipt = Receipt {
            version: 1,
            source: self.source,
            fingerprint: self.fingerprint_hex(),
            snapshot: self.snapshot.encode()?,
            issues: &self.issues,
        };
        serde_json::to_string(&receipt)
            .map_err(|_| ClosedLocalHostEnvironmentSnapshotError::InvalidEncoding)
    }
}

fn selection_issues(
    snapshot: &ClosedLocalHostEnvironmentSnapshot,
) -> Vec<LocalCompilerHostSelectionIssue> {
    let has = |variable| snapshot.path(variable).is_some();
    let mut issues = Vec::new();
    if !has(LocalHostVariable::NudoxTypeScriptNode) {
        issues.push(LocalCompilerHostSelectionIssue::MissingTypeScriptNode);
    }
    if has(LocalHostVariable::NudoxTypeScriptCompiler)
        && !has(LocalHostVariable::NudoxTypeScriptModuleRoot)
    {
        issues.push(LocalCompilerHostSelectionIssue::MissingTypeScriptModuleRoot);
    }
    if !has(LocalHostVariable::NudoxPython) {
        issues.push(LocalCompilerHostSelectionIssue::MissingPythonInterpreter);
    }
    if !has(LocalHostVariable::NudoxPyrefly) {
        issues.push(LocalCompilerHostSelectionIssue::MissingPyreflyChecker);
    }
    if !has(LocalHostVariable::NudoxGo) {
        issues.push(LocalCompilerHostSelectionIssue::MissingGoCompiler);
    } else if !has(LocalHostVariable::NudoxGoRoot) {
        issues.push(LocalCompilerHostSelectionIssue::MissingGoModuleCache);
    }
    issues
}

fn hex_fingerprint(fingerprint: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in fingerprint {
        use fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// A complete snapshot of the compiler-host variables selected by one owner.
///
/// The snapshot is closed: roles omitted from it are absent. The containing owner configuration
/// distinguishes this value from `None`, which retains the standalone process-environment mode.
/// The workspace data root is deliberately excluded because the embedded owner supplies it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedLocalHostEnvironmentSnapshot {
    paths: Vec<(LocalHostVariable, PathBuf)>,
}

impl ClosedLocalHostEnvironmentSnapshot {
    /// Builds a snapshot from selected role/path pairs.
    ///
    /// The input may be in any order. The stored and encoded roles use the central
    /// [`LocalHostVariable::closed_environment_snapshot_roles`] order. Duplicate roles, relative
    /// paths, the workspace-owned data root, and values that exceed the protocol bound are
    /// rejected before copying path units into the wire representation.
    ///
    /// # Errors
    ///
    /// Returns a value-free error for an unsupported or repeated role, an invalid path, or an
    /// input that exceeds the bounded snapshot size.
    pub fn from_paths(
        paths: impl IntoIterator<Item = (LocalHostVariable, PathBuf)>,
    ) -> Result<Self, ClosedLocalHostEnvironmentSnapshotError> {
        let mut selected =
            Vec::with_capacity(LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT);
        let mut encoded_size = SNAPSHOT_PREFIX_BYTES + SNAPSHOT_SUFFIX_BYTES;

        for (variable, path) in paths {
            if variable == LocalHostVariable::NudoxDataRoot {
                return Err(ClosedLocalHostEnvironmentSnapshotError::WorkspaceOwnedRole);
            }
            if !is_snapshot_role(variable) {
                return Err(ClosedLocalHostEnvironmentSnapshotError::UnsupportedRole);
            }
            if selected.iter().any(|(existing, _)| *existing == variable) {
                return Err(ClosedLocalHostEnvironmentSnapshotError::DuplicateRole);
            }
            if selected.len() >= LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT {
                return Err(ClosedLocalHostEnvironmentSnapshotError::TooManyRoles);
            }
            if !path.is_absolute() {
                return Err(ClosedLocalHostEnvironmentSnapshotError::RelativePath);
            }

            let path_size = estimated_entry_size(variable, &path)?;
            let comma_bytes = if selected.is_empty() { 0 } else { 1 };
            encoded_size = encoded_size
                .checked_add(path_size)
                .and_then(|size| size.checked_add(comma_bytes))
                .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)?;
            if encoded_size > MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES {
                return Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge);
            }

            NativePath::from_path(&path)
                .map_err(ClosedLocalHostEnvironmentSnapshotError::invalid_path)?;
            selected.push((variable, path));
        }

        selected.sort_by_key(|(variable, _)| role_rank(*variable));
        Ok(Self { paths: selected })
    }

    /// Returns the selected path for a role; an absent path means that role is sealed absent.
    #[must_use]
    pub fn path(&self, variable: LocalHostVariable) -> Option<&Path> {
        self.paths
            .iter()
            .find(|(selected, _)| *selected == variable)
            .map(|(_, path)| path.as_path())
    }

    /// Iterates selected roles in the snapshot's canonical order.
    #[must_use]
    pub fn paths(&self) -> impl Iterator<Item = (LocalHostVariable, &Path)> {
        self.paths
            .iter()
            .map(|(variable, path)| (*variable, path.as_path()))
    }

    /// Encodes the snapshot as canonical bounded version-1 JSON.
    ///
    /// # Errors
    ///
    /// Returns a value-free error if a path is no longer valid or the encoded snapshot exceeds
    /// its size limit.
    pub fn encode(&self) -> Result<String, ClosedLocalHostEnvironmentSnapshotError> {
        let mut paths = Vec::with_capacity(self.paths.len());
        for (variable, path) in &self.paths {
            let native = NativePath::from_path(path)
                .map_err(ClosedLocalHostEnvironmentSnapshotError::invalid_path)?;
            paths.push(SnapshotPathWire {
                variable: variable.environment_name().to_owned(),
                path: native
                    .to_wire()
                    .map_err(ClosedLocalHostEnvironmentSnapshotError::invalid_path)?,
            });
        }
        let wire = SnapshotWire {
            version: SNAPSHOT_VERSION,
            paths,
        };
        let encoded = serde_json::to_string(&wire)
            .map_err(|_| ClosedLocalHostEnvironmentSnapshotError::InvalidEncoding)?;
        if encoded.len() > MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES {
            return Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge);
        }
        Ok(encoded)
    }

    /// Parses a canonical bounded version-1 snapshot.
    ///
    /// # Errors
    ///
    /// Returns a value-free error for malformed, unsupported, noncanonical, or oversized input.
    pub fn parse(input: &str) -> Result<Self, ClosedLocalHostEnvironmentSnapshotError> {
        if input.len() > MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES {
            return Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge);
        }
        let wire: SnapshotWire = serde_json::from_str(input)
            .map_err(|_| ClosedLocalHostEnvironmentSnapshotError::InvalidEncoding)?;
        if wire.version != SNAPSHOT_VERSION {
            return Err(ClosedLocalHostEnvironmentSnapshotError::UnsupportedVersion);
        }
        if wire.paths.len() > LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT {
            return Err(ClosedLocalHostEnvironmentSnapshotError::TooManyRoles);
        }

        let mut paths = Vec::with_capacity(
            wire.paths
                .len()
                .min(LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT),
        );
        for entry in wire.paths {
            if entry.variable == LocalHostVariable::NudoxDataRoot.environment_name() {
                return Err(ClosedLocalHostEnvironmentSnapshotError::WorkspaceOwnedRole);
            }
            let variable = LocalHostVariable::closed_environment_snapshot_roles()
                .find(|variable| variable.environment_name() == entry.variable)
                .ok_or(ClosedLocalHostEnvironmentSnapshotError::UnsupportedRole)?;
            let native = NativePath::from_wire(&entry.path)
                .map_err(ClosedLocalHostEnvironmentSnapshotError::invalid_path)?;
            let path = native.as_path().to_path_buf();
            if !path.is_absolute() {
                return Err(ClosedLocalHostEnvironmentSnapshotError::RelativePath);
            }
            paths.push((variable, path));
        }

        let snapshot = Self::from_paths(paths)?;
        if snapshot.encode()?.as_bytes() != input.as_bytes() {
            return Err(ClosedLocalHostEnvironmentSnapshotError::NonCanonical);
        }
        Ok(snapshot)
    }
}

/// Safe failures from closed compiler-host snapshot admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosedLocalHostEnvironmentSnapshotError {
    /// The encoded data is not a valid version-1 snapshot.
    InvalidEncoding,
    /// The encoded version is not supported.
    UnsupportedVersion,
    /// A variable name is not in the closed role vocabulary.
    UnsupportedRole,
    /// The workspace data root is owned by the local workspace, not this snapshot.
    WorkspaceOwnedRole,
    /// A role occurs more than once.
    DuplicateRole,
    /// The snapshot contains more roles than the vocabulary permits.
    TooManyRoles,
    /// A selected value is not an absolute path.
    RelativePath,
    /// A path is malformed or belongs to another platform.
    InvalidPath,
    /// The snapshot exceeds the protocol byte bound.
    TooLarge,
    /// The input is valid JSON but is not the canonical encoding.
    NonCanonical,
}

impl ClosedLocalHostEnvironmentSnapshotError {
    fn invalid_path(_: NativePathError) -> Self {
        Self::InvalidPath
    }
}

impl fmt::Display for ClosedLocalHostEnvironmentSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidEncoding => "compiler environment snapshot is invalid",
            Self::UnsupportedVersion => "compiler environment snapshot version is unsupported",
            Self::UnsupportedRole => "compiler environment snapshot role is unsupported",
            Self::WorkspaceOwnedRole => {
                "compiler environment snapshot contains a workspace-owned role"
            }
            Self::DuplicateRole => "compiler environment snapshot repeats a role",
            Self::TooManyRoles => "compiler environment snapshot has too many roles",
            Self::RelativePath => "compiler environment snapshot contains a relative path",
            Self::InvalidPath => "compiler environment snapshot contains an invalid native path",
            Self::TooLarge => "compiler environment snapshot exceeds its size limit",
            Self::NonCanonical => "compiler environment snapshot is not canonical",
        })
    }
}

impl std::error::Error for ClosedLocalHostEnvironmentSnapshotError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotWire {
    version: u8,
    paths: Vec<SnapshotPathWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPathWire {
    variable: String,
    path: NativePathWire,
}

fn is_snapshot_role(variable: LocalHostVariable) -> bool {
    LocalHostVariable::closed_environment_snapshot_roles().any(|role| role == variable)
}

fn role_rank(variable: LocalHostVariable) -> usize {
    LocalHostVariable::closed_environment_snapshot_roles()
        .position(|role| role == variable)
        .unwrap_or(usize::MAX)
}

fn estimated_entry_size(
    variable: LocalHostVariable,
    path: &Path,
) -> Result<usize, ClosedLocalHostEnvironmentSnapshotError> {
    let path_bytes = native_path_wire_json_size(path)?;
    ENTRY_FIXED_BYTES
        .checked_add(variable.environment_name().len())
        .and_then(|bytes| bytes.checked_add(path_bytes))
        .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)
}

#[cfg(unix)]
fn native_path_wire_json_size(
    path: &Path,
) -> Result<usize, ClosedLocalHostEnvironmentSnapshotError> {
    use std::os::unix::ffi::OsStrExt as _;

    let units = path.as_os_str().as_bytes();
    if units.len() > MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES {
        return Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge);
    }
    let digits = units.iter().try_fold(0usize, |sum, unit| {
        sum.checked_add(decimal_digits(usize::from(*unit)))
    });
    let payload = digits
        .and_then(|digits| digits.checked_add(units.len().saturating_sub(1)))
        .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)?;
    br#"{"encoding":"unix","units":[]}"#
        .len()
        .checked_add(payload)
        .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)
}

#[cfg(windows)]
fn native_path_wire_json_size(
    path: &Path,
) -> Result<usize, ClosedLocalHostEnvironmentSnapshotError> {
    use std::os::windows::ffi::OsStrExt as _;

    let mut count = 0usize;
    let mut digits = 0usize;
    for unit in path
        .as_os_str()
        .encode_wide()
        .take(MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES + 1)
    {
        count = count
            .checked_add(1)
            .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)?;
        if count > MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES {
            return Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge);
        }
        digits = digits
            .checked_add(decimal_digits(usize::from(unit)))
            .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)?;
    }
    let payload = digits
        .checked_add(count.saturating_sub(1))
        .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)?;
    br#"{"encoding":"windows","units":[]}"#
        .len()
        .checked_add(payload)
        .ok_or(ClosedLocalHostEnvironmentSnapshotError::TooLarge)
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::{
        ClosedLocalHostEnvironmentSnapshot, ClosedLocalHostEnvironmentSnapshotError,
        MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES, SnapshotPathWire, SnapshotWire,
    };
    use crate::application::LocalHostVariable;
    use backend_platform::{NativePath, NativePathWire};
    use std::path::PathBuf;

    #[test]
    fn snapshot_roles_and_environment_names_form_one_closed_vocabulary() {
        let roles = LocalHostVariable::closed_environment_snapshot_roles().collect::<Vec<_>>();
        assert_eq!(
            roles.len(),
            LocalHostVariable::CLOSED_ENVIRONMENT_SNAPSHOT_ROLE_COUNT
        );
        assert!(!roles.contains(&LocalHostVariable::NudoxDataRoot));
        let mut names = roles
            .iter()
            .map(|role| role.environment_name())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), roles.len(), "wire role names must be unique");
    }

    #[test]
    fn canonical_snapshot_sorts_roles_and_round_trips_paths() {
        let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths([
            (
                LocalHostVariable::NudoxCargo,
                std::env::temp_dir().join("bin/cargo"),
            ),
            (LocalHostVariable::Home, std::env::temp_dir()),
        ])
        .expect("valid snapshot");
        let roles = snapshot.paths().map(|(role, _)| role).collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![LocalHostVariable::Home, LocalHostVariable::NudoxCargo]
        );

        let encoded = snapshot.encode().expect("encode snapshot");
        let decoded = ClosedLocalHostEnvironmentSnapshot::parse(&encoded).expect("parse snapshot");
        assert_eq!(decoded, snapshot);
        assert_eq!(decoded.encode().expect("re-encode"), encoded);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_round_trips_non_utf8_native_paths_without_loss() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(b"/tmp/compiler-home-\xff".to_vec()));
        let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths([(
            LocalHostVariable::Home,
            path.clone(),
        )])
        .expect("valid native path snapshot");
        let encoded = snapshot.encode().expect("encode native path");
        let decoded = ClosedLocalHostEnvironmentSnapshot::parse(&encoded).expect("decode path");
        assert_eq!(decoded.path(LocalHostVariable::Home), Some(path.as_path()));
    }

    #[test]
    fn empty_snapshot_is_a_valid_closed_value_distinct_from_no_snapshot() {
        let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths(std::iter::empty::<(
            LocalHostVariable,
            PathBuf,
        )>())
        .expect("empty snapshot");
        assert_eq!(snapshot.paths().count(), 0);
        assert_eq!(
            snapshot.encode().expect("encode"),
            r#"{"version":1,"paths":[]}"#
        );
    }

    #[test]
    fn snapshot_rejects_bad_roles_paths_versions_and_noncanonical_json() {
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::from_paths([(
                LocalHostVariable::NudoxDataRoot,
                std::env::temp_dir(),
            )]),
            Err(ClosedLocalHostEnvironmentSnapshotError::WorkspaceOwnedRole)
        );
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::from_paths([(
                LocalHostVariable::Home,
                PathBuf::new(),
            )]),
            Err(ClosedLocalHostEnvironmentSnapshotError::RelativePath)
        );

        let duplicate = ClosedLocalHostEnvironmentSnapshot::from_paths([
            (LocalHostVariable::Home, std::env::temp_dir()),
            (LocalHostVariable::NudoxCargo, std::env::temp_dir()),
        ])
        .expect("base fixture");
        let duplicate_error = ClosedLocalHostEnvironmentSnapshot::from_paths([
            (LocalHostVariable::Home, std::env::temp_dir()),
            (LocalHostVariable::Home, std::env::temp_dir()),
        ])
        .expect_err("duplicate roles fail");
        assert_eq!(
            duplicate_error,
            ClosedLocalHostEnvironmentSnapshotError::DuplicateRole
        );

        let encoded = duplicate.encode().expect("encode fixture");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&format!("{encoded} ")),
            Err(ClosedLocalHostEnvironmentSnapshotError::NonCanonical)
        );
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(r#"{"version":2,"paths":[]}"#),
            Err(ClosedLocalHostEnvironmentSnapshotError::UnsupportedVersion)
        );
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(
                r#"{"version":1,"paths":[{"variable":"SECRET_PATH","path":{"encoding":"unix","units":[47]}}]}"#
            ),
            Err(ClosedLocalHostEnvironmentSnapshotError::UnsupportedRole)
        );
        let data_root_path = NativePath::from_path(&std::env::temp_dir())
            .expect("native temp path")
            .to_wire()
            .expect("temp path wire");
        let workspace_role = SnapshotWire {
            version: 1,
            paths: vec![SnapshotPathWire {
                variable: LocalHostVariable::NudoxDataRoot
                    .environment_name()
                    .to_owned(),
                path: data_root_path,
            }],
        };
        let workspace_role = serde_json::to_string(&workspace_role).expect("workspace role JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&workspace_role),
            Err(ClosedLocalHostEnvironmentSnapshotError::WorkspaceOwnedRole)
        );
        let out_of_order = SnapshotWire {
            version: 1,
            paths: vec![
                SnapshotPathWire {
                    variable: LocalHostVariable::NudoxCargo.environment_name().to_owned(),
                    path: native_path_wire(),
                },
                SnapshotPathWire {
                    variable: LocalHostVariable::Home.environment_name().to_owned(),
                    path: native_path_wire(),
                },
            ],
        };
        let out_of_order = serde_json::to_string(&out_of_order).expect("unordered role JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&out_of_order),
            Err(ClosedLocalHostEnvironmentSnapshotError::NonCanonical)
        );
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(r#"{"version":1,"extra":true,"paths":[]}"#),
            Err(ClosedLocalHostEnvironmentSnapshotError::InvalidEncoding)
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_roles_and_wrong_platform_path_units_from_wire() {
        let duplicate = SnapshotWire {
            version: 1,
            paths: vec![
                SnapshotPathWire {
                    variable: LocalHostVariable::Home.environment_name().to_owned(),
                    path: native_path_wire(),
                },
                SnapshotPathWire {
                    variable: LocalHostVariable::Home.environment_name().to_owned(),
                    path: native_path_wire(),
                },
            ],
        };
        let duplicate = serde_json::to_string(&duplicate).expect("duplicate JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&duplicate),
            Err(ClosedLocalHostEnvironmentSnapshotError::DuplicateRole)
        );

        let wrong_platform = if cfg!(unix) {
            NativePathWire::Windows(vec![b'C'.into(), b':'.into(), b'\\'.into()])
        } else {
            NativePathWire::Unix(b"/tmp/nudox".to_vec())
        };
        let wrong_platform = SnapshotWire {
            version: 1,
            paths: vec![SnapshotPathWire {
                variable: LocalHostVariable::Home.environment_name().to_owned(),
                path: wrong_platform,
            }],
        };
        let wrong_platform = serde_json::to_string(&wrong_platform).expect("wrong platform JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&wrong_platform),
            Err(ClosedLocalHostEnvironmentSnapshotError::InvalidPath)
        );

        let nul_path = if cfg!(unix) {
            NativePathWire::Unix(vec![b'/', 0])
        } else {
            NativePathWire::Windows(vec![b'C'.into(), b':'.into(), 0])
        };
        let nul_path = SnapshotWire {
            version: 1,
            paths: vec![SnapshotPathWire {
                variable: LocalHostVariable::Home.environment_name().to_owned(),
                path: nul_path,
            }],
        };
        let nul_path = serde_json::to_string(&nul_path).expect("NUL path JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&nul_path),
            Err(ClosedLocalHostEnvironmentSnapshotError::InvalidPath)
        );
    }

    #[test]
    fn snapshot_rejects_more_roles_than_the_closed_vocabulary() {
        let mut paths = LocalHostVariable::closed_environment_snapshot_roles()
            .map(|variable| SnapshotPathWire {
                variable: variable.environment_name().to_owned(),
                path: native_path_wire(),
            })
            .collect::<Vec<_>>();
        paths.push(SnapshotPathWire {
            variable: "UNKNOWN_EXTRA_ROLE".to_owned(),
            path: native_path_wire(),
        });
        let encoded = serde_json::to_string(&SnapshotWire { version: 1, paths })
            .expect("too many roles JSON");
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::parse(&encoded),
            Err(ClosedLocalHostEnvironmentSnapshotError::TooManyRoles)
        );
    }

    #[test]
    fn snapshot_rejects_oversized_paths_before_encoding() {
        let long_absolute_path =
            std::env::temp_dir().join("x".repeat(MAX_CLOSED_LOCAL_HOST_ENVIRONMENT_BYTES));
        assert_eq!(
            ClosedLocalHostEnvironmentSnapshot::from_paths([(
                LocalHostVariable::Home,
                long_absolute_path,
            )]),
            Err(ClosedLocalHostEnvironmentSnapshotError::TooLarge)
        );
    }

    #[cfg(unix)]
    fn native_path_wire() -> NativePathWire {
        NativePathWire::Unix(b"/".to_vec())
    }

    #[cfg(windows)]
    fn native_path_wire() -> NativePathWire {
        NativePathWire::Windows(vec![b'C'.into(), b':'.into(), b'\\'.into()])
    }
}
