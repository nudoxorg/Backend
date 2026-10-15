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
    ConfiguredGoOracle, GoCgoPolicy, GoOracle, GoOracleConfiguration, GoPackageAuthorityWitness,
    OracleError, UNSUPPORTED_CGO_SENTINEL, check_go_cancellation, tail, transcript_prefix,
};

/// Same-directory sources loaded by one child. A smaller batch bounds the
/// images held at once; a larger one amortizes more loads.
const MAX_BATCH_SOURCES: usize = 16;
const MAX_PLANNED_SOURCES: usize = 16_384;
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
    cancelled: Option<&'run AtomicBool>,
    helper: PathBuf,
    /// Not-yet-run batches, each holding sources of exactly one directory.
    pending: Vec<Vec<PathBuf>>,
    /// Produced images and refusals not yet taken by [`Self::image`].
    ready: BTreeMap<PathBuf, SourceImage>,
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
        check_go_cancellation(cancelled)?;
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
        Self::prove_inputs(environment, &witness, package_root, cancelled)?;
        // The helper is built (and the toolchain re-proved) only when the
        // content-addressed cache has no entry for this toolchain and source.
        let (helper, _) = self.cached_helper_binary_cancellable(cancelled)?;
        let helper = helper.ok_or(OracleError::MissingChildEnvironment)?;
        Ok(GoPackageAuthoritySession {
            oracle: self,
            package_root: package_root.to_path_buf(),
            witness,
            cancelled,
            helper,
            pending: plan_batches(sources, MAX_BATCH_SOURCES),
            ready: BTreeMap::new(),
        })
    }

    /// Re-proves the witnessed inputs and the toolchain identity exactly once.
    fn prove_inputs(
        environment: &super::GoOracleChildEnvironment,
        witness: &GoPackageAuthorityWitness,
        package_root: &Path,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), OracleError> {
        let current = witness.matches_current_cancellable(package_root, cancelled)?;
        check_go_cancellation(cancelled)?;
        if !current {
            return Err(OracleError::PackageAuthorityWitnessChanged);
        }
        // A recapture that lists the dependency closure already re-proved the
        // toolchain; only a witness without one needs its own proof.
        if !witness.recapture_proves_toolchain() {
            environment.revalidate_toolchain()?;
        }
        check_go_cancellation(cancelled)
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
        check_go_cancellation(self.cancelled)?;
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
        ConfiguredGoOracle::prove_inputs(environment, &self.witness, &self.package_root, self.cancelled)
    }

    /// Runs one batch, splitting it when its transcript outgrows the bound.
    fn run(&mut self, batch: Vec<PathBuf>) -> Result<(), OracleError> {
        let outcome = self.spawn(&batch);
        let results = match outcome {
            Err(OracleError::Cancelled) => return Err(OracleError::Cancelled),
            Err(OracleError::OutputLimit { .. }) if batch.len() > 1 => {
                // These images are large: later batches start at the size that fits.
                let half = batch.len() / 2;
                let remaining = self.pending.drain(..).flatten().collect::<Vec<_>>();
                self.pending = plan_batches(&remaining, half);
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

    fn spawn(&self, batch: &[PathBuf]) -> Result<Vec<SourceImage>, OracleError> {
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
        .execute_with_origin(&mut command, true, self.cancelled)?;
        tracing::info!(
            target: "compiler::go_authority_session",
            sources = batch.len(),
            transcript_bytes = transcript.len(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "Go authority batch produced"
        );
        decode_frames(&transcript, batch.len(), image_limit)
    }
}

/// Groups `sources` by directory, keeping first-appearance order, and cuts each
/// group into batches of at most `limit` sources.
fn plan_batches(sources: &[PathBuf], limit: usize) -> Vec<Vec<PathBuf>> {
    let mut groups: Vec<(Option<&Path>, Vec<PathBuf>)> = Vec::new();
    for source in sources {
        let directory = source.parent();
        match groups.iter_mut().find(|(owner, _)| *owner == directory) {
            Some((_, group)) => group.push(source.clone()),
            None => groups.push((directory, vec![source.clone()])),
        }
    }
    groups
        .into_iter()
        .flat_map(|(_, group)| {
            group
                .chunks(limit.max(1))
                .map(<[PathBuf]>::to_vec)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Parses one `status || u64-be length || payload` frame per requested source.
fn decode_frames(
    transcript: &[u8],
    count: usize,
    image_limit: usize,
) -> Result<Vec<SourceImage>, OracleError> {
    let truncated = |what: &str| OracleError::Decode {
        message: format!("authority image batch transcript is {what}"),
        transcript: transcript_prefix(transcript),
    };
    let mut rest = transcript;
    let mut results = Vec::with_capacity(count);
    for _ in 0..count {
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
