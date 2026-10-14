//! An installed environment is input data, never an ambient native search capability.
//!
//! The coordinator captures the selected ordinary venv before observing semantic
//! freshness. Native execution consumes this immutable payload in a separate private
//! tree. Dependency members never enter the selected project source manifest.

use super::CheckerError;
use super::project::{PythonProjectControl, checkpoint, project_error};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

const MAX_FILES: usize = 131_072;
const MAX_PAYLOAD_BYTES: usize = 512 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
// Digest-only extension resources have a separate scan budget; no binary payload
// enters the capsule or native mirror. The whole scan remains cancellable.
const MAX_COMPILED_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_PROBE_BYTES: usize = 16 * 1024;
const MAX_METADATA_BYTES: usize = 32 * 1024 * 1024;
/// Hard preallocation bound for an encoded environment input object.
pub const PYTHON_ENVIRONMENT_MAX_WIRE_BYTES: usize = MAX_PAYLOAD_BYTES + MAX_METADATA_BYTES;
const MAGIC: &[u8] = b"nudox.python.installed-environment.v1\0";
/// Reserved compiler input member; its typed role, not its name, grants admission.
pub const PYTHON_ENVIRONMENT_INPUT_PATH: &str = ".nudox-python-environment.capture";

/// Native module search precedence is explicit and separate from project membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PythonDependencyRootKind {
    SitePackages,
    StandardLibrary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum MemberPayload {
    /// Exact bytes that the native parser or installed metadata authority may read.
    Source(Vec<u8>),
    /// A native extension is witnessed, but cannot supply fabricated Python facts.
    Compiled { length: u64, digest: [u8; 32] },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Member {
    path: String,
    payload: MemberPayload,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Root {
    kind: PythonDependencyRootKind,
    original: PathBuf,
    directories: Vec<String>,
    members: Vec<Member>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SelectionProbe {
    relative: PathBuf,
    bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Interpreter {
    selected: PathBuf,
    image: PathBuf,
    digest: [u8; 32],
    base_selected: PathBuf,
    base_image: PathBuf,
    base_digest: [u8; 32],
    abi: String,
    implementation: String,
    version: [u32; 3],
    platform: String,
    machine: String,
    stdlib: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Environment {
    project: PathBuf,
    probes: Vec<SelectionProbe>,
    interpreter: Option<Interpreter>,
    roots: Vec<Root>,
}

/// Immutable, content-addressed installed dependency cohort. Clones share bytes.
/// An absent venv is represented by exact negative selection probes, not by an
/// implicit permission to query the host interpreter or its site packages.
#[derive(Clone, Debug)]
pub struct PythonEnvironmentCapture {
    environment: Arc<Environment>,
    identity: [u8; 32],
    encoded_length: u64,
    local_witness: bool,
}

/// A failed environment admission remains a typed compiler input disposition.
/// It is not replaced by an empty dependency list or a successful source report.
#[derive(Clone, Debug)]
pub enum PythonEnvironmentInput {
    Captured(PythonEnvironmentCapture),
    Unavailable(Arc<CheckerError>),
}

impl PythonEnvironmentInput {
    pub fn identity(&self) -> [u8; 32] {
        match self {
            Self::Captured(capture) => capture.identity(),
            Self::Unavailable(error) => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"nudox.python.environment-unavailable.v1\0");
                hasher.update(error.to_string().as_bytes());
                *hasher.finalize().as_bytes()
            }
        }
    }
    pub fn captured(&self) -> Result<&PythonEnvironmentCapture, CheckerError> {
        match self {
            Self::Captured(capture) => Ok(capture),
            Self::Unavailable(source) => Err(CheckerError::EnvironmentCapture {
                source: Arc::clone(source),
            }),
        }
    }
}

/// Private root mapping admitted for one native transaction only.
#[derive(Debug)]
pub(super) struct MaterializedPythonEnvironment {
    pub(super) roots: Vec<(PythonDependencyRootKind, PathBuf, PathBuf)>,
    pub(super) identity: [u8; 32],
    files: Vec<(PathBuf, String, [u8; 32], u64)>,
    compiled: std::collections::BTreeMap<PathBuf, PathBuf>,
}

/// Compact local revalidation witness. It retains digests, never dependency
/// payloads, so keeping a selected-program reuse witness cannot retain a venv.
#[derive(Clone, Debug)]
pub(super) struct PythonEnvironmentWitness {
    project: PathBuf,
    probes: Vec<SelectionProbe>,
    interpreter: Option<Interpreter>,
    roots: Vec<RootWitness>,
    local: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct RootWitness {
    kind: PythonDependencyRootKind,
    original: PathBuf,
    directories: Vec<String>,
    members: Vec<(String, [u8; 32], u64, bool)>,
}

impl RootWitness {
    fn from_root(root: &Root) -> Self {
        Self {
            kind: root.kind,
            original: root.original.clone(),
            directories: root.directories.clone(),
            members: root
                .members
                .iter()
                .map(|member| {
                    let (digest, length, compiled) = match &member.payload {
                        MemberPayload::Source(bytes) => {
                            (*blake3::hash(bytes).as_bytes(), bytes.len() as u64, false)
                        }
                        MemberPayload::Compiled { length, digest } => (*digest, *length, true),
                    };
                    (member.path.clone(), digest, length, compiled)
                })
                .collect(),
        }
    }
}

impl PythonEnvironmentWitness {
    pub(super) fn validate_current(
        &self,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        checkpoint(control)?;
        if !self.local {
            return Ok(());
        }
        validate_selection(
            &self.project,
            &self.probes,
            self.interpreter.as_ref(),
            control,
        )?;
        for root in &self.roots {
            let current = capture_root(&root.original, root.kind, control)?;
            if RootWitness::from_root(&current) != *root {
                return Err(project_error(
                    "",
                    "installed dependency content or membership changed",
                ));
            }
        }
        checkpoint(control)
    }
    pub(super) fn retained_bytes(&self) -> usize {
        let paths = self
            .roots
            .iter()
            .map(|root| {
                root.members
                    .iter()
                    .map(|(path, _, _, _)| path.len() + 128)
                    .sum::<usize>()
                    + root
                        .directories
                        .iter()
                        .map(|path| path.len() + 64)
                        .sum::<usize>()
            })
            .sum::<usize>();
        paths
            .saturating_add(
                self.probes
                    .iter()
                    .map(|probe| probe.bytes.as_ref().map_or(0, Vec::len) + 256)
                    .sum::<usize>(),
            )
            .saturating_mul(2)
    }
}

impl MaterializedPythonEnvironment {
    pub(super) fn compiled_candidate(&self, private_path: &Path) -> Option<&PathBuf> {
        self.compiled.get(private_path)
    }

    pub(super) fn validate_current(
        &self,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        for (root, path, digest, length) in &self.files {
            checkpoint(control)?;
            let bytes = super::project::read_environment_input(
                root,
                Path::new(path),
                control,
                MAX_FILE_BYTES as u64,
            )?
            .ok_or_else(|| project_error("", "private dependency member disappeared"))?;
            if bytes.len() as u64 != *length || blake3::hash(&bytes).as_bytes() != digest {
                return Err(project_error(
                    "",
                    "private dependency member changed during native execution",
                ));
            }
        }
        checkpoint(control)
    }
}

impl PythonEnvironmentCapture {
    /// Capture ordinary project-local `.venv`/`venv`, or the venv explicitly
    /// selected by pyrightconfig.json. No shell, package installer, site startup,
    /// `.pth` execution, network access, or ancestor environment selection occurs.
    pub fn capture(
        project: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<Self, CheckerError> {
        checkpoint(control)?;
        normal_absolute(project)?;
        let mut probes = Vec::new();
        for relative in ["pyrightconfig.json", ".venv/pyvenv.cfg", "venv/pyvenv.cfg"] {
            let relative = PathBuf::from(relative);
            let bytes = super::project::read_configuration_input(project, &relative, control)?;
            probes.push(SelectionProbe { relative, bytes });
        }
        let declared = probes[0]
            .bytes
            .as_ref()
            .map(|bytes| {
                let document: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
                    project_error(
                        "pyrightconfig.json",
                        "cannot admit environment selection from invalid JSON",
                    )
                })?;
                match (document.get("venvPath"), document.get("venv")) {
                    (None, None) => Ok(None),
                    (base, Some(name)) => {
                        let base = base
                            .map_or(Some("."), serde_json::Value::as_str)
                            .ok_or_else(|| {
                                project_error(
                                    "pyrightconfig.json",
                                    "venvPath must be a literal path",
                                )
                            })?;
                        let name = name.as_str().ok_or_else(|| {
                            project_error("pyrightconfig.json", "venv must be a literal name")
                        })?;
                        let base = if base == "." {
                            PathBuf::new()
                        } else {
                            normal_relative(base)?
                        };
                        Ok(Some(base.join(normal_relative(name)?)))
                    }
                    _ => Err(project_error(
                        "pyrightconfig.json",
                        "venvPath without a selected venv is ambiguous",
                    )),
                }
            })
            .transpose()?
            .flatten();
        let selected = if let Some(relative) = declared {
            let cfg = relative.join("pyvenv.cfg");
            let bytes = super::project::read_configuration_input(project, &cfg, control)?
                .ok_or_else(|| {
                    project_error(
                        "pyrightconfig.json",
                        "selected Python venv is not installed (pyvenv.cfg is absent)",
                    )
                })?;
            probes.push(SelectionProbe {
                relative: cfg,
                bytes: Some(bytes),
            });
            Some(project.join(relative))
        } else {
            match (probes[1].bytes.is_some(), probes[2].bytes.is_some()) {
                (true, false) => Some(project.join(".venv")),
                (false, true) => Some(project.join("venv")),
                (false, false) => None,
                (true, true) => {
                    return Err(project_error(
                        "",
                        "both .venv and venv exist; select one environment explicitly",
                    ));
                }
            }
        };
        let mut environment = Environment {
            project: project.into(),
            probes,
            interpreter: None,
            roots: Vec::new(),
        };
        if let Some(venv) = selected {
            let cfg = environment
                .probes
                .iter()
                .find(|probe| project.join(&probe.relative) == venv.join("pyvenv.cfg"))
                .and_then(|probe| probe.bytes.as_deref())
                .expect("selected venv config is captured");
            let text = std::str::from_utf8(cfg)
                .map_err(|_| project_error("", "venv config is not UTF-8"))?;
            if text.lines().any(|line| {
                line.split_once('=').is_some_and(|(name, value)| {
                    name.trim() == "include-system-site-packages" && value.trim() != "false"
                })
            }) {
                return Err(project_error(
                    "",
                    "system-site-packages venv is not an isolated declared dependency cohort",
                ));
            }
            let interpreter = probe_interpreter(&venv, control)?;
            let site = site_packages_path(&venv, interpreter.version);
            let mut budget = EnvironmentScanBudget::default();
            environment.roots.push(capture_root_with_budget(
                &site,
                PythonDependencyRootKind::SitePackages,
                control,
                &mut budget,
            )?);
            environment.roots.push(capture_root_with_budget(
                &interpreter.stdlib,
                PythonDependencyRootKind::StandardLibrary,
                control,
                &mut budget,
            )?);
            environment.interpreter = Some(interpreter);
        }
        let result = Self::admit(environment, control)?;
        result.validate_current(control)?;
        Ok(result)
    }

    fn admit(
        environment: Environment,
        control: PythonProjectControl<'_>,
    ) -> Result<Self, CheckerError> {
        checkpoint(control)?;
        normal_absolute(&environment.project)?;
        if environment.probes.is_empty() || environment.probes.len() > 16 {
            return Err(project_error(
                "",
                "environment selection probes exceed bound",
            ));
        }
        for probe in &environment.probes {
            normal_relative(
                probe
                    .relative
                    .to_str()
                    .ok_or_else(|| project_error("", "environment probe is not UTF-8"))?,
            )?;
            if probe
                .bytes
                .as_ref()
                .is_some_and(|bytes| bytes.len() > 1024 * 1024)
            {
                return Err(project_error(
                    "",
                    "environment selection document exceeds bound",
                ));
            }
        }
        match &environment.interpreter {
            None if !environment.roots.is_empty() => {
                return Err(project_error(
                    "",
                    "dependency roots have no selected interpreter witness",
                ));
            }
            Some(interpreter) => {
                normal_absolute(&interpreter.image)?;
                normal_absolute(&interpreter.base_selected)?;
                normal_absolute(&interpreter.base_image)?;
                normal_absolute(&interpreter.selected)?;
                normal_absolute(&interpreter.stdlib)?;
                if interpreter.implementation != "cpython"
                    || interpreter.version[0] != 3
                    || interpreter.abi.is_empty()
                    || environment.roots.len() != 2
                    || environment.roots[0].kind != PythonDependencyRootKind::SitePackages
                    || environment.roots[1].kind != PythonDependencyRootKind::StandardLibrary
                    || environment.roots[1].original != interpreter.stdlib
                {
                    return Err(project_error(
                        "",
                        "environment roots and interpreter provenance disagree",
                    ));
                }
                let venv = interpreter
                    .selected
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| project_error("", "selected interpreter has no venv owner"))?;
                let site = site_packages_path(venv, interpreter.version);
                if !venv.starts_with(&environment.project)
                    || environment.roots[0].original != site
                    || !environment.probes.iter().any(|probe| {
                        environment.project.join(&probe.relative) == venv.join("pyvenv.cfg")
                            && probe.bytes.is_some()
                    })
                {
                    return Err(project_error(
                        "",
                        "installed site root is not owned by the selected project-local venv",
                    ));
                }
            }
            None => {}
        }
        let mut count = 0usize;
        let mut size = 0usize;
        let mut compiled_size = 0u64;
        let mut metadata_bytes = environment
            .probes
            .iter()
            .map(|probe| {
                probe.bytes.as_ref().map_or(0, Vec::len) + probe.relative.as_os_str().len()
            })
            .sum::<usize>();
        let mut roots = BTreeSet::new();
        for root in &environment.roots {
            normal_absolute(&root.original)?;
            if !roots.insert(&root.original) {
                return Err(project_error("", "duplicate dependency root"));
            }
            count = count
                .checked_add(root.directories.len())
                .ok_or_else(|| project_error("", "dependency frontier count overflow"))?;
            if root
                .members
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
                || root.directories.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(project_error("", "dependency members are not canonical"));
            }
            for member in &root.members {
                checkpoint(control)?;
                normal_relative(&member.path)?;
                metadata_bytes = metadata_bytes.saturating_add(member.path.len() + 128);
                let path = Path::new(&member.path);
                let parent = path.parent().unwrap_or(Path::new(""));
                let parent = parent
                    .to_str()
                    .ok_or_else(|| project_error("", "dependency parent is not UTF-8"))?;
                if root
                    .directories
                    .binary_search_by(|entry| entry.as_str().cmp(parent))
                    .is_err()
                    || root.directories.binary_search(&member.path).is_ok()
                {
                    return Err(project_error(
                        "",
                        "dependency file/directory membership conflicts",
                    ));
                }
                let compiled = matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("so" | "pyd" | "dll")
                );
                if matches!(member.payload, MemberPayload::Compiled { .. }) != compiled {
                    return Err(project_error(
                        "",
                        "dependency payload role disagrees with its native extension kind",
                    ));
                }
                count = count
                    .checked_add(1)
                    .ok_or_else(|| project_error("", "dependency member count overflow"))?;
                if let MemberPayload::Compiled { length, .. } = &member.payload {
                    compiled_size = compiled_size.checked_add(*length).ok_or_else(|| {
                        project_error("", "compiled resource scan length overflow")
                    })?;
                }
                if let MemberPayload::Source(bytes) = &member.payload {
                    if bytes.len() > MAX_FILE_BYTES {
                        return Err(project_error("", "dependency file exceeds capture bound"));
                    }
                    size = size
                        .checked_add(bytes.len())
                        .ok_or_else(|| project_error("", "dependency bytes overflow"))?;
                }
            }
            if root.directories.first().is_none_or(|path| !path.is_empty()) {
                return Err(project_error("", "dependency root directory is absent"));
            }
            for path in &root.directories {
                metadata_bytes = metadata_bytes.saturating_add(path.len() + 64);
                if !path.is_empty() {
                    let relative = normal_relative(path)?;
                    let parent = relative
                        .parent()
                        .unwrap_or(Path::new(""))
                        .to_str()
                        .ok_or_else(|| {
                            project_error("", "dependency directory parent is not UTF-8")
                        })?;
                    if root
                        .directories
                        .binary_search_by(|entry| entry.as_str().cmp(parent))
                        .is_err()
                    {
                        return Err(project_error("", "dependency directory parent is absent"));
                    }
                }
            }
        }
        if count > MAX_FILES
            || size > MAX_PAYLOAD_BYTES
            || metadata_bytes > MAX_METADATA_BYTES
            || compiled_size > MAX_COMPILED_SCAN_BYTES
        {
            return Err(project_error(
                "",
                "installed environment exceeds capture budget",
            ));
        }
        let mut hasher = HashWriter(blake3::Hasher::new(), 0);
        encode_environment(&environment, &mut hasher, control)?;
        let identity = *hasher.0.finalize().as_bytes();
        Ok(Self {
            environment: Arc::new(environment),
            identity,
            encoded_length: hasher.1,
            local_witness: true,
        })
    }

    /// Stable environment and dependency content identity, including selection negatives.
    pub const fn identity(&self) -> [u8; 32] {
        self.identity
    }

    pub(super) fn witness(&self) -> PythonEnvironmentWitness {
        PythonEnvironmentWitness {
            project: self.environment.project.clone(),
            probes: self.environment.probes.clone(),
            interpreter: self.environment.interpreter.clone(),
            roots: self
                .environment
                .roots
                .iter()
                .map(RootWitness::from_root)
                .collect(),
            local: self.local_witness,
        }
    }

    pub(super) fn validate_profile(
        &self,
        profile: backend_semantic::vocabulary::PythonVersion,
    ) -> Result<(), CheckerError> {
        if let Some(interpreter) = &self.environment.interpreter {
            let tag = super::profile_tag(profile);
            let expected = format!("{}.{}", interpreter.version[0], interpreter.version[1]);
            let host = if cfg!(windows) {
                "win32"
            } else if cfg!(target_os = "macos") {
                "darwin"
            } else if cfg!(target_os = "linux") {
                "linux"
            } else {
                std::env::consts::OS
            };
            if tag != expected || interpreter.platform != host || interpreter.machine.is_empty() {
                return Err(project_error(
                    "",
                    "captured interpreter version/platform does not match the selected native profile",
                ));
            }
        }
        Ok(())
    }

    /// Exact project-relative venv owner selected by this immutable cohort.
    /// Dependency source membership must not become selected project coverage.
    pub fn selected_venv_relative(&self) -> Option<&Path> {
        self.environment
            .interpreter
            .as_ref()?
            .selected
            .parent()?
            .parent()?
            .strip_prefix(&self.environment.project)
            .ok()
    }

    /// Installed-environment membership, separate from selected project sources.
    pub fn is_dependency_member(&self, project_relative: &Path) -> bool {
        self.selected_venv_relative()
            .is_some_and(|venv| project_relative.starts_with(venv))
    }

    /// Exact canonical wire length, computed during content-address admission.
    pub fn encoded_length(&self) -> u64 {
        self.encoded_length
    }

    /// Stream canonical input bytes to a bounded CAS payload writer. The caller
    /// controls transport chunking; this does not construct a semantic row.
    pub fn write_encoded(
        &self,
        writer: &mut impl Write,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        checkpoint(control)?;
        encode_environment(&self.environment, writer, control)?;
        checkpoint(control)
    }

    /// Portable immutable payload for a compiler input object. Paths in this payload
    /// describe provenance; they never grant the receiver permission to read them.
    pub fn encode(&self, control: PythonProjectControl<'_>) -> Result<Vec<u8>, CheckerError> {
        checkpoint(control)?;
        let mut bytes = Vec::new();
        encode_environment(&self.environment, &mut bytes, control)?;
        checkpoint(control)?;
        Ok(bytes)
    }

    /// Decode and recompute canonical content identity; no filesystem reads occur.
    pub fn decode(
        bytes: &[u8],
        expected: [u8; 32],
        control: PythonProjectControl<'_>,
    ) -> Result<Self, CheckerError> {
        checkpoint(control)?;
        if bytes.len() > PYTHON_ENVIRONMENT_MAX_WIRE_BYTES
            || !bytes.starts_with(MAGIC)
            || blake3::hash(bytes).as_bytes() != &expected
        {
            return Err(project_error(
                "",
                "dependency capture object identity or bound mismatch",
            ));
        }
        let environment = decode_environment(&bytes[MAGIC.len()..], control)?;
        let mut capture = Self::admit(environment, control)?;
        capture.local_witness = false;
        if capture.identity != expected {
            return Err(project_error("", "noncanonical dependency capture object"));
        }
        Ok(capture)
    }

    /// Reopen the exact selected environment at the coordinator publication fence.
    /// A worker must instead validate the received content object and its input root.
    pub fn validate_current(&self, control: PythonProjectControl<'_>) -> Result<(), CheckerError> {
        checkpoint(control)?;
        if !self.local_witness {
            return Ok(());
        }
        for probe in &self.environment.probes {
            checkpoint(control)?;
            if super::project::read_configuration_input(
                &self.environment.project,
                &probe.relative,
                control,
            )? != probe.bytes
            {
                return Err(project_error(
                    "",
                    "Python environment selection changed during capture",
                ));
            }
        }
        if let Some(interpreter) = &self.environment.interpreter {
            if interpreter.selected.canonicalize().map_err(io_error)? != interpreter.image
                || hash_image(&interpreter.image, control)? != interpreter.digest
                || interpreter.base_selected.canonicalize().map_err(io_error)?
                    != interpreter.base_image
                || hash_image(&interpreter.base_image, control)? != interpreter.base_digest
            {
                return Err(project_error(
                    "",
                    "selected interpreter changed during capture",
                ));
            }
        }
        for root in &self.environment.roots {
            let current = capture_root(&root.original, root.kind, control)?;
            if &current != root {
                return Err(project_error(
                    "",
                    "installed dependency content or membership changed",
                ));
            }
        }
        checkpoint(control)
    }

    pub(super) fn materialize(
        &self,
        parent: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<MaterializedPythonEnvironment, CheckerError> {
        let mut roots = Vec::new();
        let mut files = Vec::new();
        let mut compiled = std::collections::BTreeMap::new();
        for (index, root) in self.environment.roots.iter().enumerate() {
            checkpoint(control)?;
            let mirror = parent.join(format!("dependency-{index}"));
            std::fs::create_dir(&mirror).map_err(io_error)?;
            for directory in &root.directories {
                checkpoint(control)?;
                if !directory.is_empty() {
                    std::fs::create_dir_all(mirror.join(directory)).map_err(io_error)?;
                }
            }
            for member in &root.members {
                checkpoint(control)?;
                let bytes = match &member.payload {
                    MemberPayload::Source(bytes) => bytes.as_slice(),
                    // A compiled-only import remains unavailable to the genuine
                    // checker. Never manufacture an empty extension or stub.
                    MemberPayload::Compiled { .. } => {
                        compiled
                            .insert(mirror.join(&member.path), root.original.join(&member.path));
                        continue;
                    }
                };
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(mirror.join(&member.path))
                    .map_err(io_error)?;
                file.write_all(bytes).map_err(io_error)?;
                files.push((
                    mirror.clone(),
                    member.path.clone(),
                    *blake3::hash(bytes).as_bytes(),
                    bytes.len() as u64,
                ));
            }
            roots.push((root.kind, root.original.clone(), mirror));
        }
        checkpoint(control)?;
        Ok(MaterializedPythonEnvironment {
            roots,
            identity: self.identity,
            files,
            compiled,
        })
    }
}

fn site_packages_path(venv: &Path, version: [u32; 3]) -> PathBuf {
    if cfg!(windows) {
        venv.join("Lib/site-packages")
    } else {
        venv.join(format!(
            "lib/python{}.{}/site-packages",
            version[0], version[1]
        ))
    }
}

fn normal_relative(path: &str) -> Result<PathBuf, CheckerError> {
    let value = Path::new(path);
    if path.is_empty()
        || value.components().count() > 64
        || path.contains('\\')
        || !value
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || value.components().collect::<PathBuf>().as_os_str() != value.as_os_str()
    {
        return Err(project_error(
            "",
            "environment path is not a normal project-relative path",
        ));
    }
    Ok(value.into())
}

fn validate_selection(
    project: &Path,
    probes: &[SelectionProbe],
    interpreter: Option<&Interpreter>,
    control: PythonProjectControl<'_>,
) -> Result<(), CheckerError> {
    for probe in probes {
        checkpoint(control)?;
        if super::project::read_configuration_input(project, &probe.relative, control)?
            != probe.bytes
        {
            return Err(project_error(
                "",
                "Python environment selection changed during capture",
            ));
        }
    }
    if let Some(interpreter) = interpreter {
        if interpreter.selected.canonicalize().map_err(io_error)? != interpreter.image
            || hash_image(&interpreter.image, control)? != interpreter.digest
            || interpreter.base_selected.canonicalize().map_err(io_error)? != interpreter.base_image
            || hash_image(&interpreter.base_image, control)? != interpreter.base_digest
        {
            return Err(project_error(
                "",
                "selected interpreter changed during capture",
            ));
        }
    }
    checkpoint(control)
}

fn normal_absolute(path: &Path) -> Result<(), CheckerError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(CheckerError::UncapturedDependency { path: path.into() });
    }
    Ok(())
}

fn io_error(source: std::io::Error) -> CheckerError {
    CheckerError::Workspace { source }
}

fn capture_root(
    root: &Path,
    kind: PythonDependencyRootKind,
    control: PythonProjectControl<'_>,
) -> Result<Root, CheckerError> {
    capture_root_with_budget(root, kind, control, &mut EnvironmentScanBudget::default())
}

#[derive(Default)]
struct EnvironmentScanBudget {
    payload: usize,
    compiled: u64,
    entries: usize,
    metadata: usize,
}

fn capture_root_with_budget(
    root: &Path,
    kind: PythonDependencyRootKind,
    control: PythonProjectControl<'_>,
    budget: &mut EnvironmentScanBudget,
) -> Result<Root, CheckerError> {
    normal_absolute(root)?;
    let mut pending = vec![PathBuf::new()];
    let mut directories = Vec::new();
    let mut members = Vec::new();
    while let Some(relative) = pending.pop() {
        checkpoint(control)?;
        let entries = directory_entries(root, &relative, control)?;
        directories.push(
            relative
                .to_str()
                .ok_or_else(|| project_error("", "dependency path is not UTF-8"))?
                .to_owned(),
        );
        budget.entries = budget
            .entries
            .checked_add(entries.len() + 1)
            .ok_or_else(|| project_error("", "dependency frontier count overflow"))?;
        if budget.entries > MAX_FILES {
            return Err(project_error(
                "",
                "dependency frontier exceeds capture bound",
            ));
        }
        for (name, is_directory) in entries {
            checkpoint(control)?;
            let child = relative.join(&name);
            budget.metadata = budget
                .metadata
                .saturating_add(child.as_os_str().len() + 128);
            if budget.metadata > MAX_METADATA_BYTES {
                return Err(project_error(
                    "",
                    "dependency frontier metadata exceeds capture bound",
                ));
            }
            if is_directory {
                // Bytecode and venv nesting are not native Python source roots.
                if name != "__pycache__"
                    && !(kind == PythonDependencyRootKind::StandardLibrary
                        && name == "site-packages")
                {
                    pending.push(child);
                }
            } else {
                let spelling = child
                    .to_str()
                    .ok_or_else(|| project_error("", "dependency path is not UTF-8"))?
                    .to_owned();
                let extension = child.extension().and_then(|part| part.to_str());
                let compiled = matches!(extension, Some("so" | "pyd" | "dll"));
                let source = matches!(extension, Some("py" | "pyi" | "pth"))
                    || matches!(
                        name.to_str(),
                        Some(
                            "py.typed"
                                | "METADATA"
                                | "RECORD"
                                | "WHEEL"
                                | "direct_url.json"
                                | "pyproject.toml"
                                | "pyrefly.toml"
                                | ".pyrefly.toml"
                                | "pyrightconfig.json"
                                | "mypy.ini"
                                | "setup.cfg"
                                | "uv.lock"
                                | "poetry.lock"
                        )
                    );
                if !compiled && !source {
                    continue;
                }
                let payload = if compiled {
                    let (length, digest) = super::project::digest_environment_input(
                        root,
                        &child,
                        control,
                        MAX_COMPILED_SCAN_BYTES.saturating_sub(budget.compiled),
                    )?
                    .ok_or_else(|| {
                        project_error("", "compiled dependency disappeared during capture")
                    })?;
                    budget.compiled = budget
                        .compiled
                        .checked_add(length)
                        .ok_or_else(|| project_error("", "compiled dependency scan overflow"))?;
                    MemberPayload::Compiled { length, digest }
                } else {
                    let bytes = super::project::read_environment_input(
                        root,
                        &child,
                        control,
                        MAX_FILE_BYTES.min(MAX_PAYLOAD_BYTES.saturating_sub(budget.payload)) as u64,
                    )?
                    .ok_or_else(|| project_error("", "dependency disappeared during capture"))?;
                    budget.payload = budget
                        .payload
                        .checked_add(bytes.len())
                        .ok_or_else(|| project_error("", "dependency bytes overflow"))?;
                    if budget.payload > MAX_PAYLOAD_BYTES {
                        return Err(project_error(
                            "",
                            "dependency root exceeds capture byte budget",
                        ));
                    }
                    MemberPayload::Source(bytes)
                };
                members.push(Member {
                    path: spelling,
                    payload,
                });
            }
        }
    }
    directories.sort();
    members.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Root {
        kind,
        original: root.into(),
        directories,
        members,
    })
}

#[cfg(unix)]
fn directory_entries(
    root: &Path,
    relative: &Path,
    control: PythonProjectControl<'_>,
) -> Result<Vec<(std::ffi::OsString, bool)>, CheckerError> {
    use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, open, openat, statat};
    use std::os::unix::ffi::OsStrExt;
    let flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut directory = open("/", flags, Mode::empty()).map_err(|error| io_error(error.into()))?;
    for component in root.components().chain(relative.components()) {
        checkpoint(control)?;
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = openat(&directory, name, flags, Mode::empty())
                    .map_err(|error| io_error(error.into()))?
            }
            _ => {
                return Err(CheckerError::UncapturedDependency {
                    path: root.join(relative),
                });
            }
        }
    }
    let mut stream = Dir::read_from(&directory).map_err(|error| io_error(error.into()))?;
    let mut entries = Vec::new();
    for entry in &mut stream {
        checkpoint(control)?;
        let entry = entry.map_err(|error| io_error(error.into()))?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if entries.len() >= MAX_FILES {
            return Err(project_error(
                "",
                "dependency directory exceeds capture member bound",
            ));
        }
        let metadata = statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| io_error(error.into()))?;
        let is_directory = match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => true,
            FileType::RegularFile => false,
            _ => {
                return Err(CheckerError::UncapturedDependency {
                    path: root
                        .join(relative)
                        .join(std::ffi::OsStr::from_bytes(name.to_bytes())),
                });
            }
        };
        entries.push((
            std::ffi::OsStr::from_bytes(name.to_bytes()).to_owned(),
            is_directory,
        ));
    }
    entries.sort();
    Ok(entries)
}

#[cfg(windows)]
fn directory_entries(
    root: &Path,
    relative: &Path,
    control: PythonProjectControl<'_>,
) -> Result<Vec<(std::ffi::OsString, bool)>, CheckerError> {
    use backend_platform::win32::workspace_fs::{EntryKind, WorkspaceRoot};
    checkpoint(control)?;
    let root = WorkspaceRoot::open_read_only_source(root).map_err(io_error)?;
    let parts = relative
        .components()
        .map(|part| {
            part.as_os_str()
                .to_str()
                .ok_or_else(|| project_error("", "dependency path is not UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries = root
        .read_dir_source_checked_limited(&parts, MAX_FILES)
        .map_err(io_error)?
        .into_iter()
        .map(|entry| (entry.name.into(), entry.kind == EntryKind::Directory))
        .collect::<Vec<_>>();
    checkpoint(control)?;
    entries.sort();
    Ok(entries)
}

#[cfg(not(any(unix, windows)))]
fn directory_entries(
    root: &Path,
    relative: &Path,
    _control: PythonProjectControl<'_>,
) -> Result<Vec<(std::ffi::OsString, bool)>, CheckerError> {
    Err(CheckerError::UncapturedDependency {
        path: root.join(relative),
    })
}

fn hash_image(path: &Path, control: PythonProjectControl<'_>) -> Result<[u8; 32], CheckerError> {
    let parent = path
        .parent()
        .ok_or_else(|| project_error("", "interpreter image has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| project_error("", "interpreter image has no filename"))?;
    let bytes = super::project::read_environment_input(
        parent,
        Path::new(name),
        control,
        MAX_FILE_BYTES as u64,
    )?
    .ok_or_else(|| project_error("", "selected interpreter image is absent"))?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

fn probe_interpreter(
    venv: &Path,
    control: PythonProjectControl<'_>,
) -> Result<Interpreter, CheckerError> {
    let control = PythonProjectControl {
        deadline: control
            .deadline
            .min(std::time::Instant::now() + std::time::Duration::from_secs(10)),
        ..control
    };
    let selected = if cfg!(windows) {
        venv.join("Scripts/python.exe")
    } else {
        venv.join("bin/python")
    };
    let image = selected.canonicalize().map_err(io_error)?;
    normal_absolute(&image)?;
    let digest = hash_image(&image, control)?;
    // `-I -S` excludes user/global site startup and executable .pth hooks. The
    // selected image supplies its genuine implementation/platform/stdlib data.
    let mut child = Command::new(&selected).args(["-I", "-S", "-B", "-c", "import sys,sysconfig,json,platform; print(json.dumps({'implementation':sys.implementation.name,'version':list(sys.version_info[:3]),'platform':sys.platform,'machine':platform.machine(),'stdlib':sysconfig.get_path('stdlib'),'base_image':sys._base_executable,'abi':sysconfig.get_config_var('SOABI') or sys.implementation.cache_tag}))"])
        .env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(io_error)?;
    let stdout = child.stdout.take().expect("piped metadata stdout");
    let stderr = child.stderr.take().expect("piped metadata stderr");
    let output = std::thread::scope(|scope| {
        let out = scope.spawn(move || bounded_probe(stdout));
        let err = scope.spawn(move || bounded_probe(stderr));
        let result = loop {
            if let Err(error) = checkpoint(control) {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(_)) => {
                    break Err(project_error("", "selected Python metadata probe failed"));
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(io_error(error));
                }
            }
        };
        let stdout = out.join().map_err(|_| CheckerError::ProjectPanic)??;
        let stderr = err.join().map_err(|_| CheckerError::ProjectPanic)??;
        if let Err(error) = result {
            if matches!(
                &error,
                CheckerError::Cancelled { .. } | CheckerError::Deadline { .. }
            ) {
                return Err(error);
            }
            if !stderr.is_empty() {
                return Err(project_error("", &String::from_utf8_lossy(&stderr)));
            }
            return Err(error);
        }
        Ok::<_, CheckerError>(stdout)
    })?;
    #[derive(Deserialize)]
    struct Probe {
        implementation: String,
        version: [u32; 3],
        platform: String,
        machine: String,
        stdlib: PathBuf,
        base_image: PathBuf,
        abi: String,
    }
    let probe: Probe = serde_json::from_slice(&output)
        .map_err(|_| project_error("", "invalid selected interpreter metadata"))?;
    if probe.implementation != "cpython" || probe.version[0] != 3 {
        return Err(project_error("", "selected environment is not CPython 3"));
    }
    normal_absolute(&probe.stdlib)?;
    let base_image = probe.base_image.canonicalize().map_err(io_error)?;
    let base_digest = hash_image(&base_image, control)?;
    if hash_image(&image, control)? != digest {
        return Err(project_error(
            "",
            "interpreter image changed during metadata probe",
        ));
    }
    Ok(Interpreter {
        selected,
        image,
        digest,
        base_selected: probe.base_image,
        base_image,
        base_digest,
        abi: probe.abi,
        implementation: probe.implementation,
        version: probe.version,
        platform: probe.platform,
        machine: probe.machine,
        stdlib: probe.stdlib,
    })
}

fn bounded_probe(mut stream: impl Read) -> Result<Vec<u8>, CheckerError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    let mut overflow = false;
    loop {
        let count = stream.read(&mut chunk).map_err(io_error)?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_PROBE_BYTES {
            overflow = true;
        } else if !overflow {
            bytes.extend_from_slice(&chunk[..count]);
        }
    }
    if overflow {
        return Err(project_error(
            "",
            "interpreter metadata output exceeds capture bound",
        ));
    }
    Ok(bytes)
}

struct HashWriter(blake3::Hasher, u64);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        self.1 = self
            .1
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("environment wire length overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn field(writer: &mut impl Write, bytes: &[u8]) -> Result<(), CheckerError> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| project_error("", "environment wire field overflow"))?;
    writer.write_all(&length.to_be_bytes()).map_err(io_error)?;
    writer.write_all(bytes).map_err(io_error)
}
fn path_field(writer: &mut impl Write, path: &Path) -> Result<(), CheckerError> {
    field(
        writer,
        path.to_str()
            .ok_or_else(|| project_error("", "environment path is not UTF-8"))?
            .as_bytes(),
    )
}
fn count(writer: &mut impl Write, count: usize) -> Result<(), CheckerError> {
    writer
        .write_all(
            &u32::try_from(count)
                .map_err(|_| project_error("", "environment count overflow"))?
                .to_be_bytes(),
        )
        .map_err(io_error)
}
fn encode_environment(
    environment: &Environment,
    writer: &mut impl Write,
    control: PythonProjectControl<'_>,
) -> Result<(), CheckerError> {
    writer.write_all(MAGIC).map_err(io_error)?;
    path_field(writer, &environment.project)?;
    count(writer, environment.probes.len())?;
    for probe in &environment.probes {
        checkpoint(control)?;
        path_field(writer, &probe.relative)?;
        writer
            .write_all(&[u8::from(probe.bytes.is_some())])
            .map_err(io_error)?;
        if let Some(bytes) = &probe.bytes {
            field(writer, bytes)?;
        }
    }
    field(
        writer,
        &serde_json::to_vec(&environment.interpreter)
            .map_err(|_| project_error("", "interpreter witness encoding failed"))?,
    )?;
    count(writer, environment.roots.len())?;
    for root in &environment.roots {
        checkpoint(control)?;
        writer
            .write_all(&[match root.kind {
                PythonDependencyRootKind::SitePackages => 0,
                PythonDependencyRootKind::StandardLibrary => 1,
            }])
            .map_err(io_error)?;
        path_field(writer, &root.original)?;
        count(writer, root.directories.len())?;
        for directory in &root.directories {
            field(writer, directory.as_bytes())?;
        }
        count(writer, root.members.len())?;
        for member in &root.members {
            checkpoint(control)?;
            field(writer, member.path.as_bytes())?;
            match &member.payload {
                MemberPayload::Source(bytes) => {
                    writer.write_all(&[0]).map_err(io_error)?;
                    field(writer, bytes)?;
                }
                MemberPayload::Compiled { length, digest } => {
                    writer.write_all(&[1]).map_err(io_error)?;
                    writer.write_all(&length.to_be_bytes()).map_err(io_error)?;
                    writer.write_all(digest).map_err(io_error)?;
                }
            }
        }
    }
    checkpoint(control)
}

struct Wire<'a>(&'a [u8], usize);
impl<'a> Wire<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], CheckerError> {
        if count > self.0.len() {
            return Err(project_error("", "truncated environment object"));
        }
        let (bytes, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(bytes)
    }
    fn count(&mut self, maximum: usize) -> Result<usize, CheckerError> {
        let count = u32::from_be_bytes(self.take(4)?.try_into().expect("four bytes")) as usize;
        if count > maximum {
            return Err(project_error("", "environment wire count exceeds bound"));
        }
        Ok(count)
    }
    fn field(&mut self, maximum: usize) -> Result<&'a [u8], CheckerError> {
        let count = self.count(maximum)?;
        self.take(count)
    }
    fn metadata_field(&mut self, maximum: usize) -> Result<&'a [u8], CheckerError> {
        let bytes = self.field(maximum)?;
        self.1 = self
            .1
            .checked_add(bytes.len() + 128)
            .filter(|total| *total <= MAX_METADATA_BYTES)
            .ok_or_else(|| project_error("", "environment wire metadata exceeds bound"))?;
        Ok(bytes)
    }
    fn text(&mut self) -> Result<String, CheckerError> {
        std::str::from_utf8(self.metadata_field(64 * 1024)?)
            .map(str::to_owned)
            .map_err(|_| project_error("", "environment wire path is not UTF-8"))
    }
    fn tag(&mut self) -> Result<u8, CheckerError> {
        Ok(self.take(1)?[0])
    }
}
fn decode_environment(
    bytes: &[u8],
    control: PythonProjectControl<'_>,
) -> Result<Environment, CheckerError> {
    let mut wire = Wire(bytes, 0);
    let project = PathBuf::from(wire.text()?);
    let probe_count = wire.count(16)?;
    let mut probes = Vec::new();
    for _ in 0..probe_count {
        checkpoint(control)?;
        let relative = PathBuf::from(wire.text()?);
        let bytes = match wire.tag()? {
            0 => None,
            1 => Some(wire.metadata_field(1024 * 1024)?.to_vec()),
            _ => return Err(project_error("", "invalid environment probe tag")),
        };
        probes.push(SelectionProbe { relative, bytes });
    }
    let interpreter = serde_json::from_slice(wire.metadata_field(MAX_PROBE_BYTES)?)
        .map_err(|_| project_error("", "invalid interpreter witness"))?;
    let root_count = wire.count(2)?;
    let mut roots = Vec::new();
    let mut member_count = 0usize;
    let mut total_bytes = 0usize;
    for _ in 0..root_count {
        checkpoint(control)?;
        let kind = match wire.tag()? {
            0 => PythonDependencyRootKind::SitePackages,
            1 => PythonDependencyRootKind::StandardLibrary,
            _ => return Err(project_error("", "invalid dependency root kind")),
        };
        let original = PathBuf::from(wire.text()?);
        let directory_count = wire.count(MAX_FILES.saturating_sub(member_count))?;
        member_count += directory_count;
        let mut directories = Vec::new();
        for _ in 0..directory_count {
            checkpoint(control)?;
            directories.push(wire.text()?);
        }
        let count = wire.count(MAX_FILES.saturating_sub(member_count))?;
        member_count += count;
        let mut members = Vec::new();
        for _ in 0..count {
            checkpoint(control)?;
            let path = wire.text()?;
            let payload = match wire.tag()? {
                0 => {
                    let bytes = wire
                        .field(MAX_FILE_BYTES.min(MAX_PAYLOAD_BYTES.saturating_sub(total_bytes)))?;
                    total_bytes += bytes.len();
                    MemberPayload::Source(bytes.to_vec())
                }
                1 => MemberPayload::Compiled {
                    length: u64::from_be_bytes(wire.take(8)?.try_into().expect("eight bytes")),
                    digest: wire.take(32)?.try_into().expect("digest bytes"),
                },
                _ => return Err(project_error("", "invalid dependency member kind")),
            };
            members.push(Member { path, payload });
        }
        roots.push(Root {
            kind,
            original,
            directories,
            members,
        });
    }
    if !wire.0.is_empty() {
        return Err(project_error("", "environment object has trailing bytes"));
    }
    Ok(Environment {
        project,
        probes,
        interpreter,
        roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
        PythonProjectControl {
            cancelled,
            deadline: Instant::now() + Duration::from_secs(180),
        }
    }
    fn fixture() -> Result<super::super::Workspace, CheckerError> {
        let mut workspace = super::super::Workspace::create()?;
        workspace.path = workspace.path.canonicalize().map_err(io_error)?;
        Ok(workspace)
    }

    #[test]
    fn absence_is_content_keyed_and_environment_appearance_invalidates() -> Result<(), CheckerError>
    {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        let first = PythonEnvironmentCapture::capture(&root.path, control(&cancelled))?;
        let repeat = PythonEnvironmentCapture::capture(&root.path, control(&cancelled))?;
        assert_eq!(first.identity(), repeat.identity());
        assert!(first.environment.roots.is_empty());
        std::fs::create_dir(root.path.join(".venv")).map_err(io_error)?;
        std::fs::write(
            root.path.join(".venv/pyvenv.cfg"),
            "include-system-site-packages = false\n",
        )
        .map_err(io_error)?;
        assert!(first.validate_current(control(&cancelled)).is_err());
        Ok(())
    }

    #[test]
    fn selected_missing_and_escaping_venvs_do_not_become_empty_inputs() -> Result<(), CheckerError>
    {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        for config in [
            r#"{"venvPath":"local","venv":"py3"}"#,
            r#"{"venvPath":"../outside","venv":"py3"}"#,
            r#"{"venvPath":"/outside","venv":"py3"}"#,
        ] {
            std::fs::write(root.path.join("pyrightconfig.json"), config).map_err(io_error)?;
            assert!(PythonEnvironmentCapture::capture(&root.path, control(&cancelled)).is_err());
        }
        Ok(())
    }

    #[test]
    fn conflicting_default_environments_require_explicit_selection() -> Result<(), CheckerError> {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        for name in ["venv", ".venv"] {
            std::fs::create_dir(root.path.join(name)).map_err(io_error)?;
            std::fs::write(
                root.path.join(name).join("pyvenv.cfg"),
                "include-system-site-packages = false\n",
            )
            .map_err(io_error)?;
        }
        let error = PythonEnvironmentCapture::capture(&root.path, control(&cancelled)).unwrap_err();
        assert!(error.to_string().contains("both .venv and venv"));
        Ok(())
    }

    #[test]
    fn received_capture_is_offline_and_identity_checked() -> Result<(), CheckerError> {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        let capture = PythonEnvironmentCapture::capture(&root.path, control(&cancelled))?;
        let bytes = capture.encode(control(&cancelled))?;
        let identity = capture.identity();
        drop(root);
        let decoded = PythonEnvironmentCapture::decode(&bytes, identity, control(&cancelled))?;
        decoded.validate_current(control(&cancelled))?;
        assert_eq!(decoded.identity(), identity);
        let mut changed = bytes.clone();
        changed.push(0);
        assert!(PythonEnvironmentCapture::decode(&changed, identity, control(&cancelled)).is_err());
        let changed_identity = *blake3::hash(&changed).as_bytes();
        assert!(
            PythonEnvironmentCapture::decode(&changed, changed_identity, control(&cancelled))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn dependency_payload_and_transitive_membership_changes_are_distinct()
    -> Result<(), CheckerError> {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        std::fs::write(root.path.join("first.py"), "from second import answer\n")
            .map_err(io_error)?;
        std::fs::write(root.path.join("second.py"), "answer: int = 42\n").map_err(io_error)?;
        let first = capture_root(
            &root.path,
            PythonDependencyRootKind::SitePackages,
            control(&cancelled),
        )?;
        assert_eq!(first.members.len(), 2);
        assert_eq!(
            RootWitness::from_root(&first),
            RootWitness::from_root(&capture_root(&root.path, first.kind, control(&cancelled))?)
        );
        std::fs::write(root.path.join("second.py"), "answer: str = 'changed'\n")
            .map_err(io_error)?;
        let changed = capture_root(&root.path, first.kind, control(&cancelled))?;
        assert_ne!(
            RootWitness::from_root(&first),
            RootWitness::from_root(&changed)
        );
        std::fs::remove_file(root.path.join("second.py")).map_err(io_error)?;
        assert_ne!(
            RootWitness::from_root(&changed),
            RootWitness::from_root(&capture_root(&root.path, first.kind, control(&cancelled))?)
        );
        Ok(())
    }

    #[test]
    fn cancellation_precedes_capture_and_wire_admission() -> Result<(), CheckerError> {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        let capture = PythonEnvironmentCapture::capture(&root.path, control(&cancelled))?;
        let bytes = capture.encode(control(&cancelled))?;
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            PythonEnvironmentCapture::capture(&root.path, control(&cancelled)),
            Err(CheckerError::Cancelled { .. })
        ));
        assert!(matches!(
            capture.encode(control(&cancelled)),
            Err(CheckerError::Cancelled { .. })
        ));
        assert!(matches!(
            PythonEnvironmentCapture::decode(&bytes, capture.identity(), control(&cancelled)),
            Err(CheckerError::Cancelled { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dependency_and_intermediate_directory_escape_are_refused()
    -> Result<(), CheckerError> {
        let root = fixture()?;
        let outside = fixture()?;
        let cancelled = AtomicBool::new(false);
        std::fs::write(outside.path.join("outside.py"), "authority = 123\n").map_err(io_error)?;
        std::os::unix::fs::symlink(outside.path.join("outside.py"), root.path.join("alias.py"))
            .map_err(io_error)?;
        assert!(
            capture_root(
                &root.path,
                PythonDependencyRootKind::SitePackages,
                control(&cancelled)
            )
            .is_err()
        );
        std::fs::remove_file(root.path.join("alias.py")).map_err(io_error)?;
        std::os::unix::fs::symlink(&outside.path, root.path.join("escape")).map_err(io_error)?;
        assert!(
            capture_root(
                &root.path.join("escape"),
                PythonDependencyRootKind::SitePackages,
                control(&cancelled)
            )
            .is_err()
        );
        assert!(
            super::super::project::read_environment_input(
                &root.path.join("escape"),
                Path::new("outside.py"),
                control(&cancelled),
                MAX_FILE_BYTES as u64
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn special_dependency_entry_refuses_without_opening_a_fifo() -> Result<(), CheckerError> {
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        rustix::fs::mknodat(
            rustix::fs::CWD,
            root.path.join("blocked.py"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .map_err(|error| io_error(error.into()))?;
        assert!(
            capture_root(
                &root.path,
                PythonDependencyRootKind::SitePackages,
                control(&cancelled)
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn compiled_resources_stream_past_parser_limit_and_changes_invalidate()
    -> Result<(), CheckerError> {
        use std::io::{Seek, SeekFrom};
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        let compiled = root.path.join("native.so");
        let mut file = std::fs::File::create(&compiled).map_err(io_error)?;
        file.set_len(MAX_FILE_BYTES as u64 + 1).map_err(io_error)?;
        file.seek(SeekFrom::End(-1)).map_err(io_error)?;
        file.write_all(b"A").map_err(io_error)?;
        drop(file);
        assert!(
            super::super::project::read_environment_input(
                &root.path,
                Path::new("native.so"),
                control(&cancelled),
                MAX_FILE_BYTES as u64
            )
            .is_err()
        );
        let first = capture_root(
            &root.path,
            PythonDependencyRootKind::SitePackages,
            control(&cancelled),
        )?;
        let MemberPayload::Compiled { length, digest } = &first.members[0].payload else {
            panic!("compiled resource must remain digest-only");
        };
        assert_eq!(*length, MAX_FILE_BYTES as u64 + 1);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&compiled)
            .map_err(io_error)?;
        file.seek(SeekFrom::End(-1)).map_err(io_error)?;
        file.write_all(b"B").map_err(io_error)?;
        drop(file);
        let changed = capture_root(
            &root.path,
            PythonDependencyRootKind::SitePackages,
            control(&cancelled),
        )?;
        let MemberPayload::Compiled {
            digest: changed_digest,
            ..
        } = &changed.members[0].payload
        else {
            panic!("compiled resource must remain digest-only");
        };
        assert_ne!(digest, changed_digest);
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            super::super::project::digest_environment_input(
                &root.path,
                Path::new("native.so"),
                control(&cancelled),
                MAX_COMPILED_SCAN_BYTES
            ),
            Err(CheckerError::Cancelled { .. })
        ));
        Ok(())
    }

    /// Executes ordinary CPython venv creation and a genuine offline wheel
    /// installer, then the pinned native checker. Missing tool inputs fail the
    /// explicitly requested gate; they never silently skip native validation.
    #[test]
    #[ignore = "requires managed genuine CPython 3.14 and uv; creates a private ordinary venv"]
    fn genuine_offline_installed_venv_transitive_types_and_change_invalidation()
    -> Result<(), Box<dyn std::error::Error>> {
        let cpython = std::env::var_os("NUDOX_TEST_ENV_CPYTHON")
            .ok_or("managed gate requires NUDOX_TEST_ENV_CPYTHON")?;
        let uv = std::env::var_os("NUDOX_TEST_ENV_UV")
            .ok_or("managed gate requires NUDOX_TEST_ENV_UV")?;
        let root = fixture()?;
        let cancelled = AtomicBool::new(false);
        let project = root.path.join("project");
        let wheels = root.path.join("wheels");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&wheels)?;
        let output = Command::new(&cpython).args(["-I", "-S", "-B", "-c", r#"
import csv, io, pathlib, sys, zipfile
assert sys.implementation.name == 'cpython' and sys.version_info[:2] == (3, 14)
root = pathlib.Path(sys.argv[1])
for name, source, requirement in [('second_pkg', 'answer: int = 42\n', ''), ('first_pkg', 'from second_pkg import answer\n', 'Requires-Dist: second-pkg==1.0.0\n')]:
    metadata = f'{name}-1.0.0.dist-info'
    files = {f'{name}/__init__.py': source, f'{metadata}/METADATA': f'Metadata-Version: 2.1\nName: {name}\nVersion: 1.0.0\n{requirement}', f'{metadata}/WHEEL': 'Wheel-Version: 1.0\nGenerator: native-environment-control\nRoot-Is-Purelib: true\nTag: py3-none-any\n'}
    record = io.StringIO()
    writer = csv.writer(record)
    for path, text in files.items(): writer.writerow([path, '', len(text.encode())])
    writer.writerow([f'{metadata}/RECORD', '', ''])
    files[f'{metadata}/RECORD'] = record.getvalue()
    with zipfile.ZipFile(root / f'{name}-1.0.0-py3-none-any.whl', 'w') as archive:
        for path, text in files.items(): archive.writestr(path, text)
"#]).arg(&wheels).output()?;
        assert!(
            output.status.success(),
            "genuine wheel creation: {:?}",
            output
        );
        let venv = project.join("venv");
        let output = Command::new(&cpython)
            .args(["-I", "-B", "-m", "venv", "--without-pip"])
            .arg(&venv)
            .output()?;
        assert!(
            output.status.success(),
            "ordinary CPython venv: {:?}",
            output
        );
        let python = if cfg!(windows) {
            venv.join("Scripts/python.exe")
        } else {
            venv.join("bin/python")
        };
        let output = Command::new(&uv)
            .args(["pip", "install", "--no-index", "--find-links"])
            .arg(&wheels)
            .arg("--python")
            .arg(&python)
            .arg("--cache-dir")
            .arg(root.path.join("uv-cache"))
            .arg("first-pkg==1.0.0")
            .output()?;
        assert!(
            output.status.success(),
            "ordinary offline transitive installation: {:?}",
            output
        );
        let source = b"from first_pkg import answer\nvalue = answer\n";
        std::fs::write(project.join("main.py"), source)?;
        let sources = [super::super::PythonProjectBytesSource {
            relative_path: "main.py",
            source,
        }];
        let profile = backend_semantic::vocabulary::PythonVersion::Python314;
        let authority =
            super::super::NativePythonProjectAuthority::admit_controlled(control(&cancelled))?
                .with_timeout(Duration::from_secs(180));
        let default_capture = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        assert_eq!(
            default_capture.selected_venv_relative(),
            Some(Path::new("venv"))
        );
        std::fs::create_dir(project.join("local"))?;
        std::fs::rename(&venv, project.join("local/py3"))?;
        std::fs::write(
            project.join("pyrightconfig.json"),
            br#"{"venvPath":"local","venv":"py3"}"#,
        )?;
        let capture = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        assert!(
            default_capture
                .validate_current(control(&cancelled))
                .is_err()
        );
        assert_eq!(
            capture.selected_venv_relative(),
            Some(Path::new("local/py3"))
        );
        assert!(capture.is_dependency_member(Path::new(
            "local/py3/lib/python3.14/site-packages/first_pkg/__init__.py"
        )));
        assert!(!capture.is_dependency_member(Path::new("local/py30/project.py")));
        let report = authority.analyze_project_bytes_with_environment(
            &project,
            "ordinary-installed",
            &sources,
            Some(&capture),
            profile,
            control(&cancelled),
        )?;
        let module = report
            .module("main.py")
            .ok_or("missing selected native report")?;
        assert!(module.imports.iter().all(|import| import.resolved));
        assert!(
            module
                .inferences
                .iter()
                .any(|inference| inference.observed == super::super::InferredType::Integer)
        );
        assert!(matches!(
            report.source_status("main.py"),
            Some(super::super::PythonProjectSourceStatus::Analyzed)
        ));
        assert!(
            report.module("first_pkg/__init__.py").is_none(),
            "dependencies never become selected project artifacts"
        );
        let repeat = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        assert_eq!(capture.identity(), repeat.identity());
        let site = &capture.environment.roots[0].original;
        std::fs::write(
            site.join("second_pkg/__init__.py"),
            "answer: str = 'changed'\n",
        )?;
        assert!(capture.validate_current(control(&cancelled)).is_err());
        assert!(
            report
                .witness()
                .validate_current(control(&cancelled))
                .is_err()
        );
        let changed = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        assert_ne!(capture.identity(), changed.identity());
        let changed_report = authority.analyze_project_bytes_with_environment(
            &project,
            "ordinary-installed",
            &sources,
            Some(&changed),
            profile,
            control(&cancelled),
        )?;
        assert!(
            changed_report
                .module("main.py")
                .ok_or("missing changed report")?
                .inferences
                .iter()
                .any(|inference| inference.observed == super::super::InferredType::Str)
        );
        let encoded = changed.encode(control(&cancelled))?;
        let received =
            PythonEnvironmentCapture::decode(&encoded, changed.identity(), control(&cancelled))?;
        // Removing the original transitive dependency must not alter an already
        // received immutable input. A fresh local capture must report its gap.
        std::fs::remove_file(site.join("second_pkg/__init__.py"))?;
        let offline = authority.analyze_project_bytes_with_environment(
            &project,
            "ordinary-installed",
            &sources,
            Some(&received),
            profile,
            control(&cancelled),
        )?;
        assert!(
            offline
                .module("main.py")
                .ok_or("missing offline report")?
                .inferences
                .iter()
                .any(|inference| inference.observed == super::super::InferredType::Str)
        );
        let missing = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        let missing_report = authority.analyze_project_bytes_with_environment(
            &project,
            "ordinary-installed",
            &sources,
            Some(&missing),
            profile,
            control(&cancelled),
        )?;
        assert!(matches!(
            missing_report.source_status("main.py"),
            Some(super::super::PythonProjectSourceStatus::UnavailableDependency { .. })
        ));
        // Retained compiled metadata must preserve the genuine native
        // compiled-shadow refusal without materializing fake empty binaries.
        std::fs::write(site.join("second_pkg/__init__.py"), "answer: int = 42\n")?;
        std::fs::write(site.join("first_pkg.so"), b"unavailable compiled resource")?;
        let shadow = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        let mirror_parent = root.path.join("materialization-control");
        std::fs::create_dir(&mirror_parent)?;
        let materialized = shadow.materialize(&mirror_parent, control(&cancelled))?;
        assert!(!materialized.roots[0].2.join("first_pkg.so").exists());
        assert!(
            materialized
                .compiled_candidate(&materialized.roots[0].2.join("first_pkg.so"))
                .is_some()
        );
        let shadow_report = authority.analyze_project_bytes_with_environment(
            &project,
            "ordinary-installed",
            &sources,
            Some(&shadow),
            profile,
            control(&cancelled),
        )?;
        assert!(matches!(
            shadow_report.source_status("main.py"),
            Some(super::super::PythonProjectSourceStatus::UnavailableCompiledImport { .. })
        ));
        std::fs::remove_file(site.join("first_pkg.so"))?;
        let dependency_as_project = [super::super::PythonProjectBytesSource {
            relative_path: "local/py3/lib/python3.14/site-packages/first_pkg/__init__.py",
            source: b"from second_pkg import answer\n",
        }];
        let restored = PythonEnvironmentCapture::capture(&project, control(&cancelled))?;
        assert!(
            authority
                .analyze_project_bytes_with_environment(
                    &project,
                    "ordinary-installed",
                    &dependency_as_project,
                    Some(&restored),
                    profile,
                    control(&cancelled)
                )
                .is_err()
        );
        // An explicit foreign site path is never granted by the installed cohort.
        let outside = root.path.join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(
            project.join("pyrefly.toml"),
            format!(
                "site-package-path = {:?}\n",
                vec![outside.to_str().ok_or("outside path UTF-8")?]
            ),
        )?;
        assert!(matches!(
            authority.analyze_project_bytes_with_environment(
                &project,
                "ordinary-installed",
                &sources,
                Some(&restored),
                profile,
                control(&cancelled)
            ),
            Err(CheckerError::UncapturedDependency { .. })
        ));
        Ok(())
    }
}
