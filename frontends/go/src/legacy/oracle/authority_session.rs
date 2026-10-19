//! One package request's Go authority: admission once, one loader per directory.
//!
//! Loading and type-checking a Go package dominates the cost of one authority
//! image, and every source file of a directory is described by the same load.
//! Running the oracle (and re-proving the toolchain and dependency closure)
//! once per source file made a package's indexing time grow with its file count
//! times the cost of hashing the whole Go installation. A session instead
//! proves the admitted inputs once before its first loader child and once after
//! its last one, and runs a single child for each bounded batch of same-directory
//! sources.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use super::{
    ConfiguredGoOracle, GoCgoPolicy, GoOracle, GoOracleConfiguration, GoOracleControl,
    GoPackageAuthorityWitness, OracleError, UNSUPPORTED_CGO_SENTINEL, tail, transcript_prefix,
};

const MAX_PLANNED_SOURCES: usize = 16_384;
// Bound command arguments rather than repeatedly loading one directory every
// sixteen sources. The unchanged transcript and per-image caps bound output;
// oversized transcripts still split through `run` below. Windows needs room
// for UTF-16 expansion plus the executable, mode and module arguments.
const MAX_BATCH_ARGUMENT_BYTES: usize = if cfg!(windows) {
    8 * 1024
} else {
    64 * 1024
};
/// Largest transcript one batch child may write.
const BATCH_OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
/// Batch child mode: `<mode> <module> <source>...`, one frame per source.
const BATCH_MODE: &str = "--authority-image-package-batch";
const FRAME_HEADER_BYTES: usize = 9;
const FRAME_IMAGE: u8 = 0;
const FRAME_REFUSAL: u8 = 1;

type SourceImage = Result<Vec<u8>, OracleError>;

/// Package-scoped producer of Go authority images over one admitted witness.
///
/// Created by [`ConfiguredGoOracle::begin_package_authority`]; every image is
/// byte-identical to what [`ConfiguredGoOracle::authority_image_for_package`]
/// returns for the same source. [`Self::finish`] must succeed before any image
/// may be published: it proves the witnessed inputs did not change while the
/// children ran.
pub struct GoPackageAuthoritySession<'run> {
    oracle: &'run ConfiguredGoOracle,
    package_root: PathBuf,
    witness: Cow<'run, GoPackageAuthorityWitness>,
    control: GoOracleControl<'run>,
    helper: PathBuf,
    /// Not-yet-run batches, each holding sources of exactly one directory.
    pending: Vec<Vec<PathBuf>>,
    /// Produced images and refusals not yet taken by [`Self::image`].
    ready: BTreeMap<PathBuf, SourceImage>,
    #[cfg(test)]
    batches_started: usize,
}

impl ConfiguredGoOracle {
    /// Admits one package's Go authority and plans its oracle runs.
    ///
    /// The captured witness (workspace, manifests, local targets, the Go-selected
    /// dependency files and the toolchain identity) is re-proved once here,
    /// before the first child, and once by [`GoPackageAuthoritySession::finish`].
    /// `sources` are the files whose images will be requested, in request order.
    ///
    /// # Errors
    ///
    /// Returns the typed refusal when the oracle has no isolated environment,
    /// the dependency closure is unavailable or changed, the helper cannot be
    /// prepared, or the request is cancelled.
    pub fn begin_package_authority<'run>(
        &'run self,
        package_root: &Path,
        witness: Cow<'run, GoPackageAuthorityWitness>,
        sources: &[PathBuf],
        cancelled: Option<&'run AtomicBool>,
    ) -> Result<GoPackageAuthoritySession<'run>, OracleError> {
        self.begin_package_authority_controlled(
            package_root,
            witness,
            sources,
            GoOracleControl::cancellation_only(cancelled),
        )
    }

    /// Shares the caller's absolute deadline across witness, helper and every batch.
    pub fn begin_package_authority_controlled<'run>(
        &'run self,
        package_root: &Path,
        witness: Cow<'run, GoPackageAuthorityWitness>,
        sources: &[PathBuf],
        control: GoOracleControl<'run>,
    ) -> Result<GoPackageAuthoritySession<'run>, OracleError> {
        control.check("authority_admission")?;
        if sources.is_empty()
            || sources.len() > MAX_PLANNED_SOURCES
            || sources.iter().collect::<BTreeSet<_>>().len() != sources.len()
            || sources.iter().any(|source| source.strip_prefix(package_root).ok().is_none_or(|relative| {
                relative.as_os_str().is_empty()
                    || relative.components().any(|part| !matches!(part, std::path::Component::Normal(_)))
            }))
        {
            return Err(OracleError::Decode {
                message: "Go authority plan requires distinct bounded package-relative sources".into(),
                transcript: String::new(),
            });
        }
        let environment = self
            .child_environment
            .as_ref()
            .ok_or(OracleError::MissingChildEnvironment)?;
        if matches!(&self.configuration, GoOracleConfiguration::OracleBinary(_))
            && environment.cgo_policy() == GoCgoPolicy::Disabled
        {
            return Err(OracleError::UnsupportedCgoOracleBinary);
        }
        if let Some(failure) = witness.dependency_closure_failure() {
            return Err(OracleError::DependencyClosureUnavailable { failure });
        }
        Self::prove_inputs(environment, &witness, package_root, control)?;
        // The helper is built (and the toolchain re-proved) only when the
        // content-addressed cache has no entry for this toolchain and source.
        let (helper, _) = self.cached_helper_binary_controlled(control)?;
        let helper = helper.ok_or(OracleError::MissingChildEnvironment)?;
        Ok(GoPackageAuthoritySession {
            oracle: self,
            package_root: package_root.to_path_buf(),
            witness,
            control,
            helper,
            pending: plan_batches_controlled(sources, MAX_PLANNED_SOURCES, control)?,
            ready: BTreeMap::new(),
            #[cfg(test)]
            batches_started: 0,
        })
    }

    /// Re-proves the witnessed inputs and the toolchain identity exactly once.
    fn prove_inputs(
        environment: &super::GoOracleChildEnvironment,
        witness: &GoPackageAuthorityWitness,
        package_root: &Path,
        control: GoOracleControl<'_>,
    ) -> Result<(), OracleError> {
        let current = witness.matches_current_controlled(package_root, control)?;
        control.check("witness_reproof")?;
        if !current {
            return Err(OracleError::PackageAuthorityWitnessChanged);
        }
        // A recapture that lists the dependency closure already re-proved the
        // toolchain; only a witness without one needs its own proof.
        if !witness.recapture_proves_toolchain() {
            environment.revalidate_toolchain_controlled(control)?;
        }
        control.check("witness_reproof")
    }
}

impl GoPackageAuthoritySession<'_> {
    /// Returns the authority image of one planned source, running the batch
    /// that contains it when no earlier request already did.
    ///
    /// A refusal of one source (or of its directory's load) is that source's
    /// error; sibling sources keep their images. A failure of the child itself
    /// is shared by every source of its batch without running it again.
    ///
    /// # Errors
    ///
    /// Returns the oracle's typed refusal for exactly this source, or
    /// [`OracleError::Cancelled`] once the request is cancelled.
    pub fn image(&mut self, source: &Path) -> Result<Vec<u8>, OracleError> {
        self.control.check("batch_image")?;
        if !self.ready.contains_key(source) {
            let batch = match self.pending.iter().position(|batch| batch.iter().any(|path| path == source)) {
                Some(index) => self.pending.remove(index),
                None => return Err(OracleError::Decode {
                    message: "Go authority source is unplanned or was already consumed".into(),
                    transcript: String::new(),
                }),
            };
            self.run(batch)?;
        }
        self.control.check("batch_image")?;
        self.ready.remove(source).unwrap_or_else(|| {
            Err(OracleError::Exit {
                status: "missing batch frame".to_owned(),
                stderr: format!("the Go oracle produced no image for {}", source.display()),
            })
        })
    }

    /// Proves the witnessed inputs and the toolchain are unchanged after every
    /// child ran. No image of this session may be used when this fails.
    ///
    /// # Errors
    ///
    /// Returns the typed change or cancellation.
    pub fn finish(self) -> Result<(), OracleError> {
        let environment = self
            .oracle
            .child_environment
            .as_ref()
            .ok_or(OracleError::MissingChildEnvironment)?;
        ConfiguredGoOracle::prove_inputs(
            environment,
            &self.witness,
            &self.package_root,
            self.control,
        )
    }

    /// Runs one batch, splitting it when its transcript outgrows the bound.
    fn run(&mut self, batch: Vec<PathBuf>) -> Result<(), OracleError> {
        self.control.check("package_batch")?;
        let outcome = self.spawn(&batch);
        let results = match outcome {
            Err(stopped)
                if matches!(stopped, OracleError::Cancelled)
                    || GoOracleControl::deadline_phase(&stopped).is_some() =>
            {
                return Err(stopped);
            }
            Err(OracleError::OutputLimit { .. }) if batch.len() > 1 => {
                // These images are large: later batches start at the size that fits.
                let half = batch.len() / 2;
                let remaining = self.pending.drain(..).flatten().collect::<Vec<_>>();
                self.pending = plan_batches_controlled(&remaining, half, self.control)?;
                let (first, second) = batch.split_at(half);
                self.run(first.to_vec())?;
                return self.run(second.to_vec());
            }
            Err(error) => batch
                .iter()
                .map(|_| Err(replicate(&error)))
                .collect::<Vec<_>>(),
            Ok(results) => results,
        };
        self.ready.extend(batch.into_iter().zip(results));
        Ok(())
    }

    fn spawn(&mut self, batch: &[PathBuf]) -> Result<Vec<SourceImage>, OracleError> {
        #[cfg(test)]
        {
            self.batches_started += 1;
        }
        let environment = self
            .oracle
            .child_environment
            .as_ref()
            .ok_or(OracleError::MissingChildEnvironment)?;
        let mut command = std::process::Command::new(&self.helper);
        command.arg(BATCH_MODE).arg(&self.package_root).args(batch);
        environment.apply_to(
            &mut command,
            self.witness.go_work_witness(),
            matches!(&self.oracle.configuration, GoOracleConfiguration::GoToolchain(_)),
        );
        let image_limit = self.oracle.oracle.output_limit;
        let started = std::time::Instant::now();
        let transcript = GoOracle {
            output_limit: BATCH_OUTPUT_LIMIT.max(image_limit),
            ..self.oracle.oracle
        }
        .execute_with_origin_controlled(
            &mut command,
            true,
            self.control,
            "package_batch_child",
        )?;
        tracing::info!(
            target: "compiler::go_authority_session",
            sources = batch.len(),
            transcript_bytes = transcript.len(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "Go authority batch produced"
        );
        let result = decode_frames_controlled(&transcript, batch.len(), image_limit, self.control);
        self.control.check("batch_decode")?;
        result
    }
}

/// Groups `sources` by directory, keeping first-appearance order, and cuts each
/// group into batches of at most `limit` sources.
#[cfg(test)]
fn plan_batches(sources: &[PathBuf], limit: usize) -> Vec<Vec<PathBuf>> {
    plan_batches_controlled(sources, limit, GoOracleControl::default())
        .expect("unbounded planning has no deadline or cancellation")
}

fn plan_batches_controlled(
    sources: &[PathBuf],
    limit: usize,
    control: GoOracleControl<'_>,
) -> Result<Vec<Vec<PathBuf>>, OracleError> {
    control.check("batch_plan")?;
    let mut groups: Vec<(Option<&Path>, Vec<PathBuf>)> = Vec::new();
    for source in sources {
        control.check("batch_plan")?;
        let directory = source.parent();
        match groups.iter_mut().find(|(owner, _)| *owner == directory) {
            Some((_, group)) => group.push(source.clone()),
            None => groups.push((directory, vec![source.clone()])),
        }
    }
    let mut batches = Vec::new();
    for (_, group) in groups {
        control.check("batch_plan")?;
        let mut batch = Vec::new();
        let mut argument_bytes = 0usize;
        for source in group {
            control.check("batch_plan")?;
            let bytes = source
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .saturating_add(1);
            if bytes > MAX_BATCH_ARGUMENT_BYTES {
                return Err(OracleError::Decode {
                    message: "Go authority source argument exceeds the bounded child command"
                        .into(),
                    transcript: String::new(),
                });
            }
            if !batch.is_empty()
                && (batch.len() >= limit.max(1)
                    || argument_bytes.saturating_add(bytes) > MAX_BATCH_ARGUMENT_BYTES)
            {
                batches.push(std::mem::take(&mut batch));
                argument_bytes = 0;
            }
            argument_bytes = argument_bytes.saturating_add(bytes);
            batch.push(source);
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
    }
    Ok(batches)
}

/// Parses one `status || u64-be length || payload` frame per requested source.
#[cfg(test)]
fn decode_frames(
    transcript: &[u8],
    count: usize,
    image_limit: usize,
) -> Result<Vec<SourceImage>, OracleError> {
    decode_frames_controlled(transcript, count, image_limit, GoOracleControl::default())
}

fn decode_frames_controlled(
    transcript: &[u8],
    count: usize,
    image_limit: usize,
    control: GoOracleControl<'_>,
) -> Result<Vec<SourceImage>, OracleError> {
    let truncated = |what: &str| OracleError::Decode {
        message: format!("authority image batch transcript is {what}"),
        transcript: transcript_prefix(transcript),
    };
    let mut rest = transcript;
    let mut results = Vec::with_capacity(count);
    for _ in 0..count {
        control.check("batch_decode")?;
        let (header, after) = rest
            .split_first_chunk::<FRAME_HEADER_BYTES>()
            .ok_or_else(|| truncated("missing a frame header"))?;
        let length = u64::from_be_bytes(header[1..].try_into().map_err(|_| truncated("malformed"))?);
        let length = usize::try_from(length).map_err(|_| truncated("malformed"))?;
        if length > after.len() {
            return Err(truncated("shorter than its frame length"));
        }
        let (payload, after) = after.split_at(length);
        rest = after;
        results.push(match header[0] {
            FRAME_IMAGE if length > image_limit => Err(OracleError::OutputLimit {
                phase: "collect",
                stream: "stdout",
                observed: length,
                limit: image_limit,
            }),
            FRAME_IMAGE => Ok(payload.to_vec()),
            FRAME_REFUSAL => Err(refusal(&String::from_utf8_lossy(payload))),
            _ => return Err(truncated("using an unknown frame status")),
        });
    }
    control.check("batch_decode")?;
    if rest.is_empty() {
        Ok(results)
    } else {
        Err(truncated("longer than its frames"))
    }
}

/// The error a one-source child exiting with this diagnostic would have produced.
fn refusal(message: &str) -> OracleError {
    let diagnostic = format!("oracle: {message}\n");
    if diagnostic.contains(UNSUPPORTED_CGO_SENTINEL) {
        OracleError::UnsupportedCgo {
            detail: tail(diagnostic.as_bytes()),
        }
    } else {
        OracleError::Exit {
            status: "exit status: 1".to_owned(),
            stderr: tail(diagnostic.as_bytes()),
        }
    }
}

/// An equal error for each source that shares one failed child.
fn replicate(error: &OracleError) -> OracleError {
    use OracleError as E;
    let io = |source: &std::io::Error| std::io::Error::new(source.kind(), source.to_string());
    match error {
        E::Spawn { program, source } => E::Spawn { program: program.clone(), source: io(source) },
        E::GoOracleSourceDirectory(source) => E::GoOracleSourceDirectory(io(source)),
        E::GoOracleSourceFile { path, source } => E::GoOracleSourceFile { path: path.clone(), source: io(source) },
        E::GoOracleHelperCache { detail } => E::GoOracleHelperCache { detail: detail.clone() },
        E::GoToolchainIdentityChanged => E::GoToolchainIdentityChanged,
        E::MissingChildEnvironment => E::MissingChildEnvironment,
        E::UnsupportedCgoOracleBinary => E::UnsupportedCgoOracleBinary,
        E::UnsupportedCgo { detail } => E::UnsupportedCgo { detail: detail.clone() },
        E::WorkspaceWitness(detail) => E::WorkspaceWitness(detail.clone()),
        E::PackageAuthorityWitness(error) => E::WorkspaceWitness(error.to_string()),
        E::WorkspaceWitnessChanged => E::WorkspaceWitnessChanged,
        E::PackageAuthorityWitnessChanged => E::PackageAuthorityWitnessChanged,
        E::DependencyClosureUnavailable { failure } => E::DependencyClosureUnavailable { failure: *failure },
        E::ToolingUnavailable { tool, source } => E::ToolingUnavailable { tool: *tool, source: io(source) },
        E::Exit { status, stderr } => E::Exit { status: status.clone(), stderr: stderr.clone() },
        E::Decode { message, transcript } => E::Decode { message: message.clone(), transcript: transcript.clone() },
        E::OutputLimit { phase, stream, observed, limit } => E::OutputLimit { phase: *phase, stream: *stream, observed: *observed, limit: *limit },
        E::Timeout { phase, milliseconds } => E::Timeout { phase: *phase, milliseconds: *milliseconds },
        E::Cancelled => E::Cancelled,
        E::Staleness { found, expected } => E::Staleness { found: *found, expected: *expected },
        E::Pipe { stream, source } => E::Pipe { stream: *stream, source: io(source) },
        E::WorkerPanic { cause } => E::WorkerPanic { cause: *cause },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(status: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![status];
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn expired_package_batch_plan_and_decode_never_return_partial_images() {
        let deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
        let control = GoOracleControl::new(None, deadline);
        let sources = [PathBuf::from("a/one.go"), PathBuf::from("a/two.go")];
        assert!(
            matches!(plan_batches_controlled(&sources, 16, control), Err(error) if GoOracleControl::deadline_phase(&error) == Some("batch_plan"))
        );
        let mut transcript = frame(FRAME_IMAGE, b"first");
        transcript.extend(frame(FRAME_IMAGE, b"second"));
        assert!(
            matches!(decode_frames_controlled(&transcript, 2, 1024, control), Err(error) if GoOracleControl::deadline_phase(&error) == Some("batch_decode"))
        );
        assert_eq!(control.deadline, Some(deadline));
    }

    #[test]
    fn batches_group_by_directory_in_first_appearance_order_and_respect_the_limit() {
        let sources = ["a/x.go", "b/y.go", "a/z.go", "a/w.go", "b/v.go"].map(PathBuf::from);
        let batches = plan_batches(&sources, 2);
        assert_eq!(
            batches,
            vec![
                vec![PathBuf::from("a/x.go"), PathBuf::from("a/z.go")],
                vec![PathBuf::from("a/w.go")],
                vec![PathBuf::from("b/y.go"), PathBuf::from("b/v.go")],
            ]
        );
        assert!(plan_batches(&[], 4).is_empty());
    }

    #[test]
    fn directory_batches_amortize_large_source_sets_and_bound_arguments() {
        let sources = (0..65)
            .map(|index| PathBuf::from(format!("pkg/source_{index:03}.go")))
            .collect::<Vec<_>>();
        let batches = plan_batches(&sources, MAX_PLANNED_SOURCES);
        assert_eq!(batches, vec![sources.clone()]);
        let long = (0..20)
            .map(|index| PathBuf::from(format!("pkg/{}_{index}.go", "x".repeat(4096))))
            .collect::<Vec<_>>();
        let bounded = plan_batches(&long, MAX_PLANNED_SOURCES);
        assert_eq!(bounded.concat(), long);
        assert!(bounded.iter().all(|batch| {
            batch
                .iter()
                .map(|path| path.as_os_str().as_encoded_bytes().len() + 1)
                .sum::<usize>()
                <= MAX_BATCH_ARGUMENT_BYTES
        }));
        let oversized = [PathBuf::from("x".repeat(MAX_BATCH_ARGUMENT_BYTES + 1))];
        assert!(
            plan_batches_controlled(&oversized, MAX_PLANNED_SOURCES, GoOracleControl::default())
                .is_err()
        );
    }

    #[test]
    fn real_go_directory_session_reuses_one_loader_and_cached_helper_without_losing_reproof()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{
            io,
            time::{Duration, Instant},
        };
        let go = std::env::var_os("COMPILER_GO_COMPILER")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::other(
                    "COMPILER_GO_COMPILER is required for real Go session qualification",
                )
            })?
            .canonicalize()?;
        let root = tempfile::tempdir()?;
        let discovered = std::process::Command::new(&go)
            .args(["env", "GOROOT"])
            .env_clear()
            .env(
                "PATH",
                go.parent()
                    .ok_or_else(|| io::Error::other("Go executable has no parent"))?,
            )
            .env("HOME", root.path())
            .env("GOENV", "off")
            .env("GOTOOLCHAIN", "local")
            .env("GOPROXY", "off")
            .env("GOSUMDB", "off")
            .output()?;
        assert!(discovered.status.success());
        let goroot =
            PathBuf::from(std::str::from_utf8(&discovered.stdout)?.trim()).canonicalize()?;
        let module = root.path().join("module");
        let modules = root.path().join("module-cache");
        std::fs::create_dir(&module)?;
        std::fs::create_dir(&modules)?;
        std::fs::write(
            module.join("go.mod"),
            "module example.com/dirbatch\n\ngo 1.23\n",
        )?;
        let sources = (0..33)
            .map(|index| module.join(format!("source_{index:03}.go")))
            .collect::<Vec<_>>();
        for (index, source) in sources.iter().enumerate() {
            std::fs::write(
                source,
                format!("package dirbatch\nfunc Value{index:03}() int {{ return {index} }}\n"),
            )?;
        }
        let environment = super::super::GoOracleChildEnvironment::new(
            go.clone(),
            goroot,
            modules,
            root.path().join("build-cache"),
        )?;
        let oracle = GoOracle::default()
            .with_configuration(GoOracleConfiguration::go_toolchain(go)?)
            .with_child_environment(environment)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        let control = GoOracleControl::new(None, deadline);
        let witness = oracle.package_authority_witness_controlled(&module, control)?;
        assert!(witness.is_complete());
        let mut first = oracle.begin_package_authority_controlled(
            &module,
            Cow::Borrowed(&witness),
            &sources,
            control,
        )?;
        let mut images = Vec::new();
        for source in &sources {
            images.push(first.image(source)?);
        }
        assert_eq!(
            first.batches_started, 1,
            "33 same-directory files need one real loader"
        );
        assert!(images.iter().all(|image| !image.is_empty()));
        let helper = first.helper.clone();
        let helper_identity = backend_platform::FileIdentity::of_path_nofollow(&helper)?;
        first.finish()?;
        // A new request still gets a fresh witness and the original SDK checks;
        // only the content-addressed helper executable is retained.
        let witness = oracle.package_authority_witness_controlled(&module, control)?;
        let mut second = oracle.begin_package_authority_controlled(
            &module,
            Cow::Borrowed(&witness),
            &sources,
            control,
        )?;
        for (source, expected) in sources.iter().zip(&images) {
            assert_eq!(&second.image(source)?, expected);
        }
        assert_eq!(second.batches_started, 1);
        assert_eq!(second.helper, helper);
        assert_eq!(
            backend_platform::FileIdentity::of_path_nofollow(&helper)?,
            helper_identity
        );
        // Even after every image was obtained, changing an admitted input must
        // reject the whole session instead of publishing a cached image.
        std::fs::write(
            module.join("go.mod"),
            "module example.com/changed\n\ngo 1.23\n",
        )?;
        assert!(matches!(
            second.finish(),
            Err(OracleError::PackageAuthorityWitnessChanged)
        ));
        Ok(())
    }

    #[test]
    fn frames_decode_to_one_result_per_source_with_per_source_refusals() {
        let mut transcript = frame(FRAME_IMAGE, b"image-one");
        transcript.extend(frame(FRAME_REFUSAL, b"Go source has contradictory active/ignored selection"));
        transcript.extend(frame(FRAME_IMAGE, b""));
        let results = decode_frames(&transcript, 3, 1024).expect("well-formed transcript");
        assert_eq!(results[0].as_ref().expect("first image"), b"image-one");
        assert!(matches!(
            &results[1],
            Err(OracleError::Exit { status, stderr })
                if status == "exit status: 1"
                    && stderr == "oracle: Go source has contradictory active/ignored selection\n"
        ));
        assert!(results[2].as_ref().expect("empty image").is_empty());
    }

    #[test]
    fn malformed_transcripts_are_decode_errors_never_partial_results() {
        let image = frame(FRAME_IMAGE, b"abc");
        for truncated in [&image[..4], &image[..image.len() - 1]] {
            assert!(matches!(decode_frames(truncated, 1, 16), Err(OracleError::Decode { .. })));
        }
        let mut trailing = image.clone();
        trailing.push(0);
        assert!(matches!(decode_frames(&trailing, 1, 16), Err(OracleError::Decode { .. })));
        assert!(matches!(decode_frames(&image, 2, 16), Err(OracleError::Decode { .. })));
        assert!(matches!(decode_frames(&frame(7, b""), 1, 16), Err(OracleError::Decode { .. })));
    }

    #[test]
    fn an_oversized_image_frame_is_that_sources_output_limit_error() {
        let results = decode_frames(&frame(FRAME_IMAGE, &[0; 8]), 1, 4).expect("framed");
        assert!(matches!(&results[0], Err(OracleError::OutputLimit { observed: 8, limit: 4, .. })));
    }

    #[test]
    fn cgo_refusals_keep_their_typed_error() {
        let message = format!("Go package needs cgo {UNSUPPORTED_CGO_SENTINEL}");
        assert!(matches!(refusal(&message), OracleError::UnsupportedCgo { .. }));
    }

    #[test]
    fn a_shared_child_failure_replicates_without_losing_its_kind() {
        let timeout = OracleError::Timeout { phase: "collect", milliseconds: 60_000 };
        assert_eq!(replicate(&timeout).to_string(), timeout.to_string());
        let exit = OracleError::Exit { status: "exit status: 2".to_owned(), stderr: "boom".to_owned() };
        assert_eq!(replicate(&exit).to_string(), exit.to_string());
    }
}
