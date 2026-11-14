//! Supervised native Stylo worker boundary.
//!
//! This module does not decide which installed artifact is trustworthy. The
//! owner must supply a permit from an admitted product artifact; without one,
//! indexing must report unavailable rather than use an arbitrary executable.
use backend_compile::{
    InputContentSchema, InputContentVersion, ProcessEnvironment, ProcessError, ProcessLimits,
    ProcessReceipt, ProcessStdin, ProcessSupervisor, ProcessTerminal, ProtocolDescriptor,
    SupervisedCommand, ToolchainArtifact, typed_of,
};
use backend_engine::builtin::StyloWorkerPolicy;
use backend_frontend_stylo::{
    StyloError,
    worker_protocol::{self, WorkerError},
};
use backend_semantic::stylo_analysis::{StyloAnalysis, StyloParseConfig};
use backend_version::{ContentId, SourceFactDomain};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

/// Artifact and producer identity supplied by a trusted owner/bootstrap.
/// Capturing a hash of an arbitrary executable is not itself admission.
pub(super) struct StyloWorkerPermit {
    program: PathBuf,
    artifact: ToolchainArtifact,
    expected_support: [u8; 32],
    #[cfg(target_os = "linux")]
    library: Option<LinuxWorkerLibrary>,
}

#[cfg(target_os = "linux")]
struct LinuxWorkerLibrary {
    path: PathBuf,
    digest: backend_compile::ToolchainId,
}
impl StyloWorkerPermit {
    pub(super) fn support(&self) -> [u8; 32] {
        self.expected_support
    }
    pub(super) fn artifact_identity(&self) -> [u8; 32] {
        self.artifact.identity().to_bytes()
    }
    /// Constructs a capability only after the caller has admitted the artifact
    /// and the associated native support identity from its installed product.
    pub(super) fn from_admitted(
        program: PathBuf,
        artifact: ToolchainArtifact,
        expected_support: [u8; 32],
    ) -> Result<Self, StyloWorkerFailure> {
        if !artifact.dependencies().is_empty() {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        Self::from_checked_artifact(program, artifact, expected_support)
    }

    fn from_checked_artifact(
        program: PathBuf,
        artifact: ToolchainArtifact,
        expected_support: [u8; 32],
    ) -> Result<Self, StyloWorkerFailure> {
        if !program.is_absolute() || expected_support == [0; 32] {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        artifact
            .verify_path(&program)
            .map_err(StyloWorkerFailure::Process)?;
        Ok(Self {
            program,
            artifact,
            expected_support,
            #[cfg(target_os = "linux")]
            library: None,
        })
    }

    /// Constructs a macOS capability from a worker identity captured under the
    /// scan deadline and matched to the compiled installed-artifact anchor.
    /// The supervisor revalidates the executable before it runs.
    #[cfg(target_os = "macos")]
    pub(super) fn from_captured_macos_worker(
        program: PathBuf,
        artifact: ToolchainArtifact,
        expected_support: [u8; 32],
    ) -> Result<Self, StyloWorkerFailure> {
        if !program.is_absolute()
            || expected_support == [0; 32]
            || !artifact.dependencies().is_empty()
        {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        Ok(Self {
            program,
            artifact,
            expected_support,
        })
    }

    /// Constructs a permit from identities just captured under a bounded scan.
    /// The caller must compare the combined artifact identity to the compiled
    /// anchor; the supervisor revalidates both images before executing them.
    #[cfg(target_os = "linux")]
    pub(super) fn from_captured_linux_library(
        program: PathBuf,
        artifact: ToolchainArtifact,
        expected_support: [u8; 32],
        library_path: PathBuf,
        library_identity: backend_compile::ExecutableIdentity,
    ) -> Result<Self, StyloWorkerFailure> {
        if !program.is_absolute()
            || expected_support == [0; 32]
            || !library_path.is_absolute()
            || library_path.file_name().and_then(|name| name.to_str()) != Some("libgcc_s.so.1")
            || artifact.dependencies() != [library_identity.digest()]
        {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        Ok(Self {
            program,
            artifact,
            expected_support,
            library: Some(LinuxWorkerLibrary {
                path: library_path,
                digest: library_identity.digest(),
            }),
        })
    }

    /// Admits the fixed bundled Linux runtime dependency as part of the
    /// already trusted artifact identity. Its bytes are checked again and
    /// privately leased for each invocation.
    #[cfg(target_os = "linux")]
    pub(super) fn from_admitted_with_linux_library(
        program: PathBuf,
        artifact: ToolchainArtifact,
        expected_support: [u8; 32],
        library_path: PathBuf,
        library_identity: backend_compile::ExecutableIdentity,
    ) -> Result<Self, StyloWorkerFailure> {
        if !library_path.is_absolute()
            || library_path.file_name().and_then(|name| name.to_str()) != Some("libgcc_s.so.1")
            || artifact.dependencies() != [library_identity.digest()]
        {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        let mut permit = Self::from_checked_artifact(program, artifact, expected_support)?;
        permit.library = Some(LinuxWorkerLibrary {
            path: library_path,
            digest: library_identity.digest(),
        });
        Ok(permit)
    }
}

/// Honest bounded-execution outcome; no failure means a complete empty result.
#[derive(Debug)]
pub(super) enum StyloWorkerFailure {
    UnadmittedArtifact,
    Limit,
    Process(ProcessError),
    Terminal(ProcessTerminal),
    Native(StyloError),
    Protocol,
}

#[cfg(target_os = "linux")]
struct LinuxLibraryLease {
    directory: tempfile::TempDir,
    digest: backend_compile::ToolchainId,
}

fn worker_checkpoint(
    cancellation: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<(), StyloWorkerFailure> {
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        Err(StyloWorkerFailure::Process(ProcessError::Cancelled))
    } else if std::time::Instant::now() >= deadline {
        Err(StyloWorkerFailure::Process(ProcessError::Deadline))
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn read_linux_library(
    path: &std::path::Path,
    cancellation: &AtomicBool,
    deadline: std::time::Instant,
) -> Result<Vec<u8>, StyloWorkerFailure> {
    use rustix::fs::{Mode, OFlags, open};
    use std::io::Read;
    const MAX_LIBRARY_BYTES: usize = 16 * 1024 * 1024;
    let checkpoint = || worker_checkpoint(cancellation, deadline);
    checkpoint()?;
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
    let mut file = std::fs::File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
    if !metadata.is_file() || metadata.len() > MAX_LIBRARY_BYTES as u64 {
        return Err(StyloWorkerFailure::UnadmittedArtifact);
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        checkpoint()?;
        let read = file
            .read(&mut chunk)
            .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > MAX_LIBRARY_BYTES {
            return Err(StyloWorkerFailure::Limit);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    checkpoint()?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
impl LinuxLibraryLease {
    fn new(
        library: &LinuxWorkerLibrary,
        cancellation: &AtomicBool,
        deadline: std::time::Instant,
    ) -> Result<Self, StyloWorkerFailure> {
        use std::io::Write;
        let bytes = read_linux_library(&library.path, cancellation, deadline)?;
        let digest = typed_of::<backend_compile::ToolchainSchema>(&bytes);
        worker_checkpoint(cancellation, deadline)?;
        if digest != library.digest {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        let directory = tempfile::Builder::new()
            .prefix("nudox-stylo-lib-")
            .tempdir()
            .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
        let path = directory.path().join("libgcc_s.so.1");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
        for chunk in bytes.chunks(64 * 1024) {
            if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                return Err(StyloWorkerFailure::Process(ProcessError::Cancelled));
            }
            if std::time::Instant::now() >= deadline {
                return Err(StyloWorkerFailure::Process(ProcessError::Deadline));
            }
            file.write_all(chunk)
                .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
        }
        drop(file);
        let lease = Self {
            directory,
            digest: library.digest,
        };
        lease.verify(cancellation, deadline)?;
        Ok(lease)
    }

    fn verify(
        &self,
        cancellation: &AtomicBool,
        deadline: std::time::Instant,
    ) -> Result<(), StyloWorkerFailure> {
        let bytes = read_linux_library(
            &self.directory.path().join("libgcc_s.so.1"),
            cancellation,
            deadline,
        )?;
        let digest = typed_of::<backend_compile::ToolchainSchema>(&bytes);
        worker_checkpoint(cancellation, deadline)?;
        if digest != self.digest {
            return Err(StyloWorkerFailure::UnadmittedArtifact);
        }
        Ok(())
    }
}

/// Nonforgeable within this module: actual supervised execution and decoded
/// native response tied to the exact source and artifact used for this run.
pub(super) struct VerifiedStyloRun {
    analysis: StyloAnalysis,
    content: InputContentVersion,
    source: ContentId<SourceFactDomain>,
    byte_length: u64,
    request_digest: [u8; 32],
    artifact: ToolchainArtifact,
    receipt: ProcessReceipt,
    policy: StyloWorkerPolicy,
}

impl VerifiedStyloRun {
    /// Immutable native output admitted against the exact supervised frame.
    pub(super) fn analysis(&self) -> &StyloAnalysis {
        &self.analysis
    }
    /// Exact content-domain identity of the input bytes.
    pub(super) fn content(&self) -> &InputContentVersion {
        &self.content
    }
    /// Exact source-fact-domain identity of the same input bytes.
    pub(super) fn source(&self) -> &ContentId<SourceFactDomain> {
        &self.source
    }
    /// Actual captured source length.
    pub(super) const fn byte_length(&self) -> u64 {
        self.byte_length
    }
    /// Commitment to the complete bounded worker request frame.
    pub(super) const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }
    /// Artifact whose exact executable bytes were supervised.
    pub(super) fn artifact(&self) -> &ToolchainArtifact {
        &self.artifact
    }
    /// Nonforgeable supervisor cleanup and output receipt.
    pub(super) fn receipt(&self) -> &ProcessReceipt {
        &self.receipt
    }
    /// Exact process limits used for this attempt.
    pub(super) fn policy(&self) -> &StyloWorkerPolicy {
        &self.policy
    }
}

/// Canonical requested process policy, including unsuccessful attempts.
pub(super) fn stylo_worker_policy(
    wall_time: Duration,
    memory_limit: Option<usize>,
) -> Result<StyloWorkerPolicy, StyloWorkerFailure> {
    let stdout_limit = worker_protocol::MAX_OUTPUT + 12;
    let stderr_limit = 64 * 1024;
    let policy = StyloWorkerPolicy {
        wall_time_nanos: u64::try_from(wall_time.as_nanos())
            .map_err(|_| StyloWorkerFailure::Protocol)?,
        input_bytes: worker_protocol::MAX_INPUT as u64,
        stdout_bytes: stdout_limit as u64,
        stderr_bytes: stderr_limit as u64,
        output_bytes: (stdout_limit + stderr_limit) as u64,
        memory_bytes: memory_limit
            .map(u64::try_from)
            .transpose()
            .map_err(|_| StyloWorkerFailure::Protocol)?,
    };
    policy
        .validate()
        .map_err(|_| StyloWorkerFailure::Protocol)?;
    Ok(policy)
}

/// Executes a single exact request under the existing reaping supervisor.
/// Wall-time and framed input/output are always bounded. A requested hard
/// memory limit is passed to the supervisor and remains an explicit failure on
/// unsupported platforms; `None` never claims a hard memory cap.
pub(super) fn run_stylo_worker(
    permit: &StyloWorkerPermit,
    cwd: PathBuf,
    source: &[u8],
    config: &StyloParseConfig,
    wall_time: Duration,
    memory_limit: Option<usize>,
    cancellation: &AtomicBool,
) -> Result<VerifiedStyloRun, StyloWorkerFailure> {
    if source.len() > worker_protocol::MAX_SOURCE || source.len() as u64 > config.limits.input_bytes
    {
        return Err(StyloWorkerFailure::Limit);
    }
    let deadline = std::time::Instant::now()
        .checked_add(wall_time)
        .ok_or(StyloWorkerFailure::Protocol)?;
    let request = worker_protocol::encode_request(source, config)
        .map_err(|_| StyloWorkerFailure::Protocol)?;
    let request_digest = *blake3::hash(&request).as_bytes();
    let policy = stylo_worker_policy(wall_time, memory_limit)?;
    let stdout_limit = worker_protocol::MAX_OUTPUT + 12;
    let stderr_limit = 64 * 1024;
    let mut limits = ProcessLimits::new(
        stdout_limit,
        stderr_limit,
        wall_time,
        stdout_limit + stderr_limit,
    )
    .map_err(StyloWorkerFailure::Process)?
    .with_input_bytes_limit(worker_protocol::MAX_INPUT)
    .map_err(StyloWorkerFailure::Process)?;
    if let Some(limit) = memory_limit {
        limits = limits
            .with_memory_bytes_limit(limit)
            .map_err(StyloWorkerFailure::Process)?;
    }
    #[cfg(target_os = "linux")]
    let library_lease = permit
        .library
        .as_ref()
        .map(|library| LinuxLibraryLease::new(library, cancellation, deadline))
        .transpose()?;
    #[cfg(target_os = "linux")]
    let environment = ProcessEnvironment::new(match library_lease.as_ref() {
        Some(lease) => {
            let path = lease
                .directory
                .path()
                .to_str()
                .ok_or(StyloWorkerFailure::Protocol)?;
            if path.is_empty() || path.contains(':') {
                return Err(StyloWorkerFailure::Protocol);
            }
            vec![("LD_LIBRARY_PATH".to_owned(), path.to_owned())]
        }
        None => Vec::new(),
    })
    .map_err(StyloWorkerFailure::Process)?;
    #[cfg(not(target_os = "linux"))]
    let environment = ProcessEnvironment::new(Vec::new()).map_err(StyloWorkerFailure::Process)?;
    let command = SupervisedCommand::for_authority_with_artifact_until(
        permit.program.clone(),
        Vec::new(),
        environment,
        cwd,
        ProcessStdin::bytes(request),
        permit.artifact.clone(),
        None,
        ProtocolDescriptor::cold(),
        limits,
        cancellation,
        deadline,
    )
    .map_err(StyloWorkerFailure::Process)?;
    let receipt = ProcessSupervisor::new(command)
        .run_with_observer_until(cancellation, deadline)
        .map_err(StyloWorkerFailure::Process)?;
    if !receipt.reaped() {
        return Err(StyloWorkerFailure::Terminal(receipt.terminal()));
    }
    #[cfg(target_os = "linux")]
    if let Some(lease) = library_lease {
        lease.verify(cancellation, deadline)?;
        lease
            .directory
            .close()
            .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
    }
    if receipt.terminal() != ProcessTerminal::Success {
        return Err(StyloWorkerFailure::Terminal(receipt.terminal()));
    }
    worker_checkpoint(cancellation, deadline)?;
    let analysis =
        worker_protocol::decode_response(receipt.stdout(), source, config, permit.expected_support)
            .map_err(|error| match error {
                WorkerError::Analysis(StyloError::Limit) => StyloWorkerFailure::Limit,
                WorkerError::Analysis(error) => StyloWorkerFailure::Native(error),
                WorkerError::Protocol => StyloWorkerFailure::Protocol,
            })?;
    if analysis.recipe != backend_frontend_stylo::RECIPE
        || analysis.utf8_bom_removed != source.starts_with(&[0xef, 0xbb, 0xbf])
    {
        return Err(StyloWorkerFailure::Protocol);
    }
    let content = typed_of::<InputContentSchema>(source);
    let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
    worker_checkpoint(cancellation, deadline)?;
    Ok(VerifiedStyloRun {
        analysis,
        content,
        source: source_identity,
        byte_length: u64::try_from(source.len()).map_err(|_| StyloWorkerFailure::Protocol)?,
        request_digest,
        artifact: permit.artifact.clone(),
        receipt,
        policy,
    })
}

/// Supervised V2 fragment result bound to one original verified host.
pub(super) struct VerifiedStyloFragmentRun<'a> {
    analysis: worker_protocol::VerifiedFragmentAnalysis<'a>,
    request_digest: [u8; 32],
    artifact: ToolchainArtifact,
    receipt: ProcessReceipt,
    policy: StyloWorkerPolicy,
}
impl<'a> VerifiedStyloFragmentRun<'a> {
    pub(super) fn analysis(&self) -> &worker_protocol::VerifiedFragmentAnalysis<'a> { &self.analysis }
    pub(super) const fn request_digest(&self) -> [u8; 32] { self.request_digest }
    pub(super) fn artifact(&self) -> &ToolchainArtifact { &self.artifact }
    pub(super) fn receipt(&self) -> &ProcessReceipt { &self.receipt }
    pub(super) fn policy(&self) -> &StyloWorkerPolicy { &self.policy }
}
/// Execute a fragment request with the same admitted artifact, process limits,
/// cancellation, deadline, library lease and reaping as standalone Stylo.
pub(super) fn run_stylo_fragment_worker<'a>(
    permit: &StyloWorkerPermit,
    cwd: PathBuf,
    host: &backend_compile::VerifiedSourceExtent<'a>,
    region: std::ops::Range<usize>,
    config: &StyloParseConfig,
    wall_time: Duration,
    memory_limit: Option<usize>,
    cancellation: &AtomicBool,
) -> Result<VerifiedStyloFragmentRun<'a>, StyloWorkerFailure> {
    if host.text().len() > worker_protocol::MAX_SOURCE {
        return Err(StyloWorkerFailure::Limit);
    }
    let deadline = std::time::Instant::now()
        .checked_add(wall_time)
        .ok_or(StyloWorkerFailure::Protocol)?;
    let request = worker_protocol::encode_fragment_request(host, region.clone(), config)
        .map_err(|_| StyloWorkerFailure::Protocol)?;
    let request_digest = *blake3::hash(&request).as_bytes();
    let policy = stylo_worker_policy(wall_time, memory_limit)?;
    let stdout_limit = worker_protocol::MAX_OUTPUT + 12;
    let stderr_limit = 64 * 1024;
    let mut limits = ProcessLimits::new(
        stdout_limit,
        stderr_limit,
        wall_time,
        stdout_limit + stderr_limit,
    )
    .map_err(StyloWorkerFailure::Process)?
    .with_input_bytes_limit(worker_protocol::MAX_INPUT)
    .map_err(StyloWorkerFailure::Process)?;
    if let Some(limit) = memory_limit {
        limits = limits
            .with_memory_bytes_limit(limit)
            .map_err(StyloWorkerFailure::Process)?;
    }
    #[cfg(target_os = "linux")]
    let library_lease = permit
        .library
        .as_ref()
        .map(|library| LinuxLibraryLease::new(library, cancellation, deadline))
        .transpose()?;
    #[cfg(target_os = "linux")]
    let environment = ProcessEnvironment::new(match library_lease.as_ref() {
        Some(lease) => {
            let path = lease
                .directory
                .path()
                .to_str()
                .ok_or(StyloWorkerFailure::Protocol)?;
            if path.is_empty() || path.contains(':') {
                return Err(StyloWorkerFailure::Protocol);
            }
            vec![("LD_LIBRARY_PATH".to_owned(), path.to_owned())]
        }
        None => Vec::new(),
    })
    .map_err(StyloWorkerFailure::Process)?;
    #[cfg(not(target_os = "linux"))]
    let environment = ProcessEnvironment::new(Vec::new()).map_err(StyloWorkerFailure::Process)?;
    let command = SupervisedCommand::for_authority_with_artifact_until(
        permit.program.clone(),
        Vec::new(),
        environment,
        cwd,
        ProcessStdin::bytes(request),
        permit.artifact.clone(),
        None,
        ProtocolDescriptor::cold(),
        limits,
        cancellation,
        deadline,
    )
    .map_err(StyloWorkerFailure::Process)?;
    let receipt = ProcessSupervisor::new(command)
        .run_with_observer_until(cancellation, deadline)
        .map_err(StyloWorkerFailure::Process)?;
    if !receipt.reaped() {
        return Err(StyloWorkerFailure::Terminal(receipt.terminal()));
    }
    #[cfg(target_os = "linux")]
    if let Some(lease) = library_lease {
        lease.verify(cancellation, deadline)?;
        lease
            .directory
            .close()
            .map_err(|_| StyloWorkerFailure::UnadmittedArtifact)?;
    }
    if receipt.terminal() != ProcessTerminal::Success {
        return Err(StyloWorkerFailure::Terminal(receipt.terminal()));
    }
    worker_checkpoint(cancellation, deadline)?;
    let analysis = worker_protocol::decode_fragment_response(
        receipt.stdout(), host, region, config, permit.expected_support,
    ).map_err(|error| match error {
        WorkerError::Analysis(StyloError::Limit) => StyloWorkerFailure::Limit,
        WorkerError::Analysis(error) => StyloWorkerFailure::Native(error),
        WorkerError::Protocol => StyloWorkerFailure::Protocol,
    })?;
    worker_checkpoint(cancellation, deadline)?;
    Ok(VerifiedStyloFragmentRun { analysis, request_digest, artifact: permit.artifact.clone(), receipt, policy })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::stylo_analysis::{StyloLimits, StyloSourceKind};

    #[test]
    fn worker_checkpoint_rejects_cancelled_and_expired_runs() {
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            worker_checkpoint(
                &cancelled,
                std::time::Instant::now() + Duration::from_secs(1)
            ),
            Err(StyloWorkerFailure::Process(ProcessError::Cancelled))
        ));
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            worker_checkpoint(&cancelled, std::time::Instant::now()),
            Err(StyloWorkerFailure::Process(ProcessError::Deadline))
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_library_lease_checks_bytes_cancellation_and_cleanup() {
        let source = tempfile::tempdir().expect("source directory");
        let path = source.path().join("libgcc_s.so.1");
        std::fs::write(&path, b"fixture library").expect("fixture bytes");
        let library = LinuxWorkerLibrary {
            path,
            digest: typed_of::<backend_compile::ToolchainSchema>(b"fixture library"),
        };
        let cancelled = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let lease = LinuxLibraryLease::new(&library, &cancelled, deadline).expect("lease");
        let private = lease.directory.path().to_owned();
        lease.verify(&cancelled, deadline).expect("matching bytes");
        std::fs::write(private.join("libgcc_s.so.1"), b"changed bytes").expect("tamper");
        assert!(matches!(
            lease.verify(&cancelled, deadline),
            Err(StyloWorkerFailure::UnadmittedArtifact)
        ));
        drop(lease);
        assert!(!private.exists(), "private library is removed on drop");
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            LinuxLibraryLease::new(&library, &cancelled, deadline),
            Err(StyloWorkerFailure::Process(ProcessError::Cancelled))
        ));
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        let link = source.path().join("link.so");
        std::os::unix::fs::symlink(&library.path, &link).expect("symlink");
        assert!(matches!(
            read_linux_library(&link, &cancelled, deadline),
            Err(StyloWorkerFailure::UnadmittedArtifact)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires packaged worker and libgcc fixture on Linux"]
    fn linux_supervised_worker_selects_private_library_before_cleanup() {
        let worker = PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").expect("worker"));
        let library_path =
            PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_LIBGCC").expect("libgcc"));
        let identity = backend_compile::ExecutableIdentity::from_path(&library_path)
            .expect("library identity");
        let artifact =
            ToolchainArtifact::from_path(&worker, vec![identity.digest()]).expect("artifact");
        let permit = StyloWorkerPermit::from_admitted_with_linux_library(
            worker.clone(),
            artifact.clone(),
            backend_frontend_stylo::support_fingerprint(),
            library_path,
            identity,
        )
        .expect("admitted fixture closure");
        let cancellation = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let lease = LinuxLibraryLease::new(
            permit.library.as_ref().expect("library"),
            &cancellation,
            deadline,
        )
        .expect("private lease");
        let leased_path = lease.directory.path().join("libgcc_s.so.1");
        let env = ProcessEnvironment::new(vec![
            (
                "LD_LIBRARY_PATH".into(),
                lease.directory.path().to_str().expect("UTF-8 path").into(),
            ),
            ("LD_DEBUG".into(), "libs".into()),
        ])
        .expect("bounded trace environment");
        let config = StyloParseConfig {
            source_kind: StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/site.css".into(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 4096,
                rules: 100,
                depth: 16,
                records: 100,
                diagnostics: 20,
                text_bytes: 4096,
            },
        };
        let source = b".button { --ink: red; }";
        let input = worker_protocol::encode_request(source, &config).expect("request");
        let limits = ProcessLimits::new(
            worker_protocol::MAX_OUTPUT + 12,
            256 * 1024,
            Duration::from_secs(30),
            worker_protocol::MAX_OUTPUT + 12 + 256 * 1024,
        )
        .expect("limits")
        .with_input_bytes_limit(worker_protocol::MAX_INPUT)
        .expect("input limit");
        let command = SupervisedCommand::for_authority_with_artifact_until(
            worker,
            Vec::new(),
            env,
            std::env::temp_dir(),
            ProcessStdin::bytes(input),
            artifact,
            None,
            ProtocolDescriptor::cold(),
            limits,
            &cancellation,
            deadline,
        )
        .expect("checked executable");
        let receipt = ProcessSupervisor::new(command)
            .run_with_observer_until(&cancellation, deadline)
            .expect("supervised worker");
        assert!(receipt.reaped());
        assert_eq!(receipt.terminal(), ProcessTerminal::Success);
        worker_protocol::decode_response(
            receipt.stdout(),
            source,
            &config,
            permit.expected_support,
        )
        .expect("native result");
        let trace = std::str::from_utf8(receipt.stderr()).expect("loader trace");
        let selected: Vec<_> = trace
            .lines()
            .filter(|line| line.contains("calling init:") && line.contains("libgcc_s.so.1"))
            .collect();
        assert_eq!(
            selected.len(),
            1,
            "actual libgcc init must be unique: {trace}"
        );
        assert!(
            selected[0].contains(leased_path.to_str().expect("UTF-8 path")),
            "loader must select the private admitted library: {trace}"
        );
        lease
            .verify(&cancellation, deadline)
            .expect("unchanged lease");
        let directory = lease.directory.path().to_owned();
        lease.directory.close().expect("cleanup after reaping");
        assert!(!directory.exists());
        let production = run_stylo_worker(
            &permit,
            std::env::temp_dir(),
            source,
            &config,
            Duration::from_secs(30),
            None,
            &cancellation,
        )
        .expect("production lease path");
        assert!(production.receipt.reaped());
    }

    #[test]
    #[ignore = "requires NUDOX_TEST_STYLO_WORKER pointing to a separately built fixture artifact"]
    fn supervised_native_worker_binds_source_config_support_and_receipt() {
        let path = PathBuf::from(
            std::env::var_os("NUDOX_TEST_STYLO_WORKER").expect("fixture worker path"),
        );
        let artifact = ToolchainArtifact::from_path(&path, Vec::new()).expect("fixture artifact");
        let support = backend_frontend_stylo::support_fingerprint();
        let declared_dependency = typed_of::<backend_compile::ToolchainSchema>(b"dependency");
        let with_dependency =
            ToolchainArtifact::new(artifact.executable().clone(), vec![declared_dependency]);
        assert!(matches!(
            StyloWorkerPermit::from_admitted(path.clone(), with_dependency, support),
            Err(StyloWorkerFailure::UnadmittedArtifact)
        ));
        let permit = StyloWorkerPermit::from_admitted(path.clone(), artifact.clone(), support)
            .expect("fixture admission");
        let config = StyloParseConfig {
            source_kind: StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/site.css".into(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 4096,
                rules: 100,
                depth: 16,
                records: 100,
                diagnostics: 20,
                text_bytes: 4096,
            },
        };
        let source = b".button { --ink: red; color: var(--ink); }";
        let cancellation = AtomicBool::new(false);
        let result = run_stylo_worker(
            &permit,
            std::env::temp_dir(),
            source,
            &config,
            Duration::from_secs(30),
            None,
            &cancellation,
        )
        .expect("real Stylo worker");
        assert!(result.receipt.reaped());
        assert_eq!(result.receipt.terminal(), ProcessTerminal::Success);
        assert_eq!(
            result.receipt.stdout_observed(),
            result.receipt.stdout().len()
        );
        assert!(result.receipt.stdout_observed() <= worker_protocol::MAX_OUTPUT + 12);
        assert_eq!(
            result.receipt.stderr_observed(),
            result.receipt.stderr().len()
        );
        assert_eq!(result.analysis.support_fingerprint, support);
        assert_eq!(result.analysis.config, config);
        assert_eq!(result.byte_length, source.len() as u64);
        assert_eq!(result.analysis.rules.len(), 1);
        assert_eq!(result.analysis.selectors.len(), 1);
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            run_stylo_worker(
                &permit,
                std::env::temp_dir(),
                source,
                &config,
                Duration::from_secs(30),
                None,
                &cancelled
            ),
            Err(StyloWorkerFailure::Process(ProcessError::Cancelled))
                | Err(StyloWorkerFailure::Terminal(ProcessTerminal::Cancelled))
        ));
        let mut wrong = support;
        wrong[0] ^= 1;
        let permit =
            StyloWorkerPermit::from_admitted(path, artifact, wrong).expect("fixture admission");
        assert!(matches!(
            run_stylo_worker(
                &permit,
                std::env::temp_dir(),
                source,
                &config,
                Duration::from_secs(30),
                None,
                &cancellation
            ),
            Err(StyloWorkerFailure::Protocol)
        ));
    }
}
