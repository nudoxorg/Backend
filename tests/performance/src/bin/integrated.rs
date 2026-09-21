//! Production-path benchmark runner for the index/search/catalog/presentation stack.
//!
//! The runner intentionally drives the public production contracts.  It reads real source
//! files, admits complete relation roots, builds the Tantivy and Turso projections, and sends
//! the same typed commands used by the CLI and MCP adapters.  It is a small smoke-friendly
//! runner rather than a Criterion microbenchmark: every result is a bounded JSON artifact that
//! can be compared across machines and commits.

#![forbid(unsafe_code)]

use backend_client::LocalEngine;
use backend_compile::{DeclarationKind, SourceExcerpt, SourceLocation};
use backend_engine::{
    AuthorityClaim, AuthorityEpoch, ChunkChain, ChunkParts, Frame, ImmutableObjectSchema,
    MerkleRoot, ObjectKey, ObjectVersion, TransportLimits, WireIdentity,
    acquisition::{AcquisitionOutcome, AcquisitionRequest, AcquisitionService, RawArchiveObjectId},
    capability::CapabilityArtifactId,
    registry::{
        AcquisitionLimits as RegistryAcquisitionLimits, AcquisitionPolicy, HttpRegistryTransport,
        RegistryEcosystem, RegistryEndpoint, RegistryOwner,
    },
};
use backend_extension_tantivy::{
    Authority, Binding, CaseSensitivity, DocumentChange, DocumentState, FieldSelection,
    IndexRelation, LexicalView, Limits as TantivyLimits, MatchMode, OverlayLimits,
    Query as LexQuery, ReadManifest, Recipe, RefreshOutcome, TantivySource,
};
use backend_extension_turso::{ProjectionUpdate, TursoProjection};
use backend_library::{
    Basis, Command as LibraryCommand, CommandDto, Coverage, CoverageCapability, Cursor, Fragment,
    Frontier, Library, Query, QueryLimit, RequestAdmissionError, Row, RowId, ViewDelta, ViewRoot,
    admit_complete_scope, admit_producer_observation, object_version, package_key, symbol_key,
    view_key,
};
use backend_mcp::{self};
use backend_semantic::{Entity, Source, entity_key};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
};
use backend_worker::InputCas;
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

type BenchResult<T> = Result<T, Box<dyn Error>>;

const JSON_SCHEMA: &str = "nudox.integrated-benchmark.v2";
const MAX_RELATION_ROW_BYTES: usize = 32 * 1024;
const MAX_LARGE_CORPUS_FILES: usize = 256;
const MAX_LARGE_CORPUS_BYTES: usize = 4 * 1024 * 1024;
const MIN_TAIL_PERCENTILE_SAMPLES: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Smoke,
    Full,
}

impl Profile {
    fn parse(value: &str) -> BenchResult<Self> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown profile {other:?}; expected smoke or full").into()),
        }
    }

    fn repetitions(self) -> usize {
        match self {
            Self::Smoke => 3,
            Self::Full => 15,
        }
    }

    fn readers(self) -> &'static [usize] {
        match self {
            Self::Smoke => &[1, 8, 32],
            Self::Full => &[1, 8, 32],
        }
    }
}

#[derive(Clone, Debug)]
struct SourceFile {
    language: String,
    path: PathBuf,
    relative: String,
    text: String,
    bytes: usize,
}

#[derive(Clone, Debug)]
struct CorpusClass {
    name: &'static str,
    files: Vec<SourceFile>,
}

impl CorpusClass {
    fn bytes(&self) -> usize {
        self.files.iter().map(|file| file.bytes).sum()
    }
}

#[derive(Clone, Debug, Serialize)]
struct Stats {
    samples: usize,
    p50_ns: Option<u128>,
    p95_ns: Option<u128>,
    p99_ns: Option<u128>,
    min_ns: u128,
    max_ns: u128,
    percentile_status: &'static str,
}

fn stats(values: &mut [u128]) -> Stats {
    values.sort_unstable();
    let percentile = |numerator: usize, denominator: usize| -> u128 {
        if values.is_empty() {
            return 0;
        }
        let index = (values.len().saturating_sub(1) * numerator) / denominator;
        values[index]
    };
    let enough_for_tails = values.len() >= MIN_TAIL_PERCENTILE_SAMPLES;
    Stats {
        samples: values.len(),
        p50_ns: (!values.is_empty()).then(|| percentile(50, 100)),
        p95_ns: enough_for_tails.then(|| percentile(95, 100)),
        p99_ns: enough_for_tails.then(|| percentile(99, 100)),
        min_ns: values.first().copied().unwrap_or(0),
        max_ns: values.last().copied().unwrap_or(0),
        percentile_status: if values.is_empty() {
            "no_samples"
        } else if enough_for_tails {
            "descriptive_tail_percentiles"
        } else {
            "insufficient_tail_samples"
        },
    }
}

#[derive(Clone, Debug, Serialize)]
struct Hardware {
    os: String,
    arch: String,
    cpu_model: Option<String>,
    cpu_count: Option<u64>,
    memory_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    cpu_time_ns: Option<u128>,
}

#[derive(Clone, Debug, Serialize)]
struct Toolchain {
    rustc: String,
    cargo: String,
    nix: String,
}

#[derive(Clone, Debug, Serialize)]
struct Correctness {
    passed: bool,
    assertions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct IngestMeasurement {
    schema: &'static str,
    status: &'static str,
    size_class: String,
    source_kind: String,
    source_files: usize,
    source_bytes: usize,
    phase: String,
    wall: Stats,
    cpu_time_ns: Option<u128>,
    peak_rss_bytes: Option<u64>,
    bytes_read: usize,
    bytes_written: usize,
    cas_reused_bytes: usize,
    semantic_rows_rebuilt: usize,
    output_root: String,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct DeltaMeasurement {
    schema: &'static str,
    status: &'static str,
    size_class: String,
    phase: String,
    wall: Stats,
    bytes_read: usize,
    bytes_written: usize,
    cas_reused_bytes: usize,
    semantic_rows_rebuilt: usize,
    changed_rows: usize,
    base_root: String,
    output_root: String,
    no_op_root: String,
    no_op_root_identical: bool,
    bounded_work: bool,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct SearchMeasurement {
    schema: &'static str,
    status: &'static str,
    size_class: String,
    mode: String,
    cache: String,
    readers: usize,
    first_in_memory_search: Option<Stats>,
    warm: Stats,
    result_count: usize,
    bytes_read: usize,
    bytes_written: usize,
    output_root: String,
    cursor_root_stable: bool,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct CatalogMeasurement {
    schema: &'static str,
    status: &'static str,
    size_class: String,
    operation: String,
    phase: String,
    wall: Stats,
    rows: usize,
    database_bytes: usize,
    pack_bytes: usize,
    output_root: String,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct SurfaceMeasurement {
    schema: &'static str,
    status: &'static str,
    operation: String,
    phase: &'static str,
    transport: &'static str,
    wall: Stats,
    payload_bytes: usize,
    allocation_bytes: Option<usize>,
    continuation: bool,
    hard_budget_rejected: bool,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct AcquisitionMeasurement {
    schema: &'static str,
    status: &'static str,
    operation: String,
    phase: String,
    wall: Stats,
    calls: usize,
    buffer_ceiling_bytes: usize,
    downloaded_bytes: usize,
    reused_bytes: usize,
    memory_high_water_bytes: Option<usize>,
    receipt_id: Option<String>,
    delta_id: Option<String>,
    target_root: Option<String>,
    artifact_id: Option<String>,
    correctness: Correctness,
}

#[derive(Clone, Debug, Serialize)]
struct GuiMeasurement {
    schema: &'static str,
    status: &'static str,
    cold: bool,
    warm_navigation: Option<Stats>,
    model_to_first_semantic_frame: Option<Stats>,
    source_open_search: Option<Stats>,
    graph_incremental_delta: Option<Stats>,
    full_harness_wall: Option<Stats>,
    requested_viewport: String,
    requested_state: String,
    captured_captures: Option<usize>,
    captured_frames: Option<usize>,
    verified_frames: Option<usize>,
    verified_artifacts: Option<usize>,
    deterministic_seven_frame_capture: bool,
    correctness: Correctness,
}

struct ResourceSampler {
    stop: Arc<AtomicBool>,
    peak_rss_bytes: Arc<AtomicU64>,
    cpu_time_ns: Arc<AtomicU64>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ResourceSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak_rss_bytes = Arc::new(AtomicU64::new(0));
        let cpu_time_ns = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_peak_rss = Arc::clone(&peak_rss_bytes);
        let thread_cpu = Arc::clone(&cpu_time_ns);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some((rss, cpu)) = process_resources() {
                    thread_peak_rss.fetch_max(rss, Ordering::Relaxed);
                    thread_cpu.fetch_max(cpu, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stop,
            peak_rss_bytes,
            cpu_time_ns,
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> (Option<u64>, Option<u128>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let rss = self.peak_rss_bytes.load(Ordering::Relaxed);
        let cpu = self.cpu_time_ns.load(Ordering::Relaxed);
        (
            Some(rss).filter(|value| *value != 0),
            Some(u128::from(cpu)).filter(|value| *value != 0),
        )
    }
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    schema: &'static str,
    version: u32,
    status: &'static str,
    profile: String,
    commit: String,
    hardware: Hardware,
    toolchain: Toolchain,
    corpora: Vec<CorpusDescription>,
    ingest: Vec<IngestMeasurement>,
    deltas: Vec<DeltaMeasurement>,
    search: Vec<SearchMeasurement>,
    catalog: Vec<CatalogMeasurement>,
    surfaces: Vec<SurfaceMeasurement>,
    acquisition: Vec<AcquisitionMeasurement>,
    gui: GuiMeasurement,
    build: BuildMetadata,
    gate: GateSummary,
    notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct BuildMetadata {
    profile: &'static str,
    target_dir: String,
    dirty: bool,
}

#[derive(Clone, Debug, Serialize)]
struct GateSummary {
    require_complete: bool,
    complete: bool,
    corpus_complete: bool,
    promotion_ready: bool,
    status: &'static str,
    unavailable: Vec<String>,
    failed: Vec<String>,
    reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct CorpusDescription {
    name: String,
    source_kind: String,
    root: String,
    files: usize,
    bytes: usize,
    languages: Vec<String>,
}

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
}

impl ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation.producer_identity() == self.producer
            && observation.scope_root() == self.scope
            && observation.context() == self.context
            && observation.evidence() == self.evidence.as_slice())
        .then(|| {
            ProducerObservationClaims::new(
                self.producer,
                self.scope,
                self.context,
                *blake3::hash(&self.evidence).as_bytes(),
            )
        })
        .ok_or(())
    }
}

fn authorized_coverage(bytes: &[u8; 32]) -> CoverageWitness {
    let authority = Authority::from_value(bytes);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier)
        .map_err(|_| "coverage observation was rejected")
        .expect("benchmark coverage fixture is valid");
    CoverageWitness::Complete(
        admit_complete_scope(declaration, admitted)
            .map_err(|_| "coverage scope was rejected")
            .expect("benchmark coverage scope is valid"),
    )
}

fn coverage_capability(bytes: &[u8]) -> BenchResult<CoverageCapability> {
    let authority = object_version(bytes);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier)
        .map_err(|_| "coverage observation was rejected")?;
    let complete =
        admit_complete_scope(declaration, admitted).map_err(|_| "coverage scope was rejected")?;
    CoverageCapability::from_authorized_with_evidence(complete, verifier.evidence)
        .map_err(|error| error.into())
}

fn workspace_root() -> backend_version::WorkspaceRoot {
    let coverage = authorized_coverage(&[7; 32]);
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        coverage,
    )
    .expect("benchmark workspace manifest is valid")
    .root()
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn root_hex<T: AsRef<[u8]>>(root: T) -> String {
    hex(root.as_ref())
}

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn build_metadata() -> BuildMetadata {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    let target_dir = if target_dir.is_absolute() {
        target_dir
    } else {
        env::current_dir()
            .map(|directory| directory.join(&target_dir))
            .unwrap_or(target_dir)
    };
    BuildMetadata {
        profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        target_dir: target_dir.to_string_lossy().into_owned(),
        dirty: worktree_dirty(),
    }
}

fn worktree_dirty() -> bool {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output();
    match output {
        Ok(output) => !output.status.success() || !output.stdout.is_empty(),
        Err(_) => true,
    }
}

fn observe_gate_cell(
    unavailable: &mut Vec<String>,
    failed: &mut Vec<String>,
    label: String,
    status: &str,
    passed: bool,
) {
    if status != "ok" {
        unavailable.push(format!("{label}: status={status}"));
    }
    if !passed {
        failed.push(format!("{label}: correctness=false"));
    }
}

fn gate_summary(
    profile: Profile,
    require_complete: bool,
    build: &BuildMetadata,
    ingest: &[IngestMeasurement],
    deltas: &[DeltaMeasurement],
    search: &[SearchMeasurement],
    catalog: &[CatalogMeasurement],
    surfaces: &[SurfaceMeasurement],
    acquisition: &[AcquisitionMeasurement],
    gui: &GuiMeasurement,
    corpora: &[CorpusDescription],
) -> GateSummary {
    let mut unavailable = Vec::new();
    let mut failed = Vec::new();
    for item in ingest {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!("ingest/{}/{}", item.size_class, item.phase),
            item.status,
            item.correctness.passed,
        );
    }
    for item in deltas {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!("delta/{}/{}", item.size_class, item.phase),
            item.status,
            item.correctness.passed,
        );
    }
    for item in search {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!(
                "search/{}/{}/readers-{}",
                item.size_class, item.mode, item.readers
            ),
            item.status,
            item.correctness.passed,
        );
    }
    for item in catalog {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!("catalog/{}/{}", item.operation, item.phase),
            item.status,
            item.correctness.passed,
        );
    }
    for item in surfaces {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!("surface/{}", item.operation),
            item.status,
            item.correctness.passed,
        );
        if require_complete && item.transport != "authenticated_unix" {
            unavailable.push(format!(
                "surface/{}: requires authenticated Unix locald transport, observed {}",
                item.operation, item.transport
            ));
        }
    }
    for item in acquisition {
        observe_gate_cell(
            &mut unavailable,
            &mut failed,
            format!("acquisition/{}", item.operation),
            item.status,
            item.correctness.passed,
        );
    }
    observe_gate_cell(
        &mut unavailable,
        &mut failed,
        "gui/package".to_owned(),
        gui.status,
        gui.correctness.passed,
    );

    let corpus_complete = corpora.iter().any(|corpus| {
        corpus.name == "large"
            && corpus.source_kind.starts_with("nix-configured")
            && [
                "clang",
                "csharp",
                "go",
                "java",
                "python",
                "rust",
                "typescript",
            ]
            .iter()
            .all(|language| corpus.languages.iter().any(|observed| observed == language))
    });
    if require_complete && !corpus_complete {
        unavailable
            .push("corpus/large: seven-lane configured fleet corpus was not driven".to_owned());
    }

    let complete = unavailable.is_empty() && failed.is_empty() && corpus_complete;
    let release_required = require_complete || matches!(profile, Profile::Full);
    let mut reasons = Vec::new();
    if release_required && build.profile != "release" {
        reasons.push(format!(
            "promotion requires a release build, observed {}",
            build.profile
        ));
    }
    if !unavailable.is_empty() {
        reasons.push(format!(
            "{} benchmark cells are unavailable or non-ok",
            unavailable.len()
        ));
    }
    if !corpus_complete {
        reasons.push("seven-lane configured fleet corpus was not driven".to_owned());
    }
    if !failed.is_empty() {
        reasons.push(format!(
            "{} benchmark correctness cells failed",
            failed.len()
        ));
    }
    let promotion_ready = complete && (!release_required || build.profile == "release");
    let status = if promotion_ready {
        "ok"
    } else if require_complete || matches!(profile, Profile::Full) {
        "failed"
    } else {
        "partial"
    };
    GateSummary {
        require_complete,
        complete,
        corpus_complete,
        promotion_ready,
        status,
        unavailable,
        failed,
        reasons,
    }
}

fn process_resources() -> Option<(u64, u64)> {
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        let pid = std::process::id().to_string();
        let output = Command::new("ps")
            .args(["-p", &pid, "-o", "rss=,time="])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut fields = stdout.split_whitespace();
        let rss_kib = fields.next()?.parse::<u64>().ok()?;
        let cpu = fields.next().and_then(parse_cpu_time_ns).unwrap_or(0);
        Some((rss_kib.saturating_mul(1024), cpu))
    } else {
        None
    }
}

fn parse_cpu_time_ns(value: &str) -> Option<u64> {
    let (minutes, seconds) = value.split_once(':')?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<u64>().ok()?;
    let mut fraction = fraction
        .bytes()
        .take(9)
        .filter(|byte| byte.is_ascii_digit())
        .collect::<Vec<_>>();
    if fraction.is_empty() {
        fraction.push(b'0');
    }
    while fraction.len() < 9 {
        fraction.push(b'0');
    }
    let fraction = std::str::from_utf8(&fraction).ok()?.parse::<u64>().ok()?;
    Some(
        (minutes.saturating_mul(60).saturating_add(seconds))
            .saturating_mul(1_000_000_000)
            .saturating_add(fraction),
    )
}

fn commit() -> String {
    command_output("git", &["rev-parse", "HEAD"])
}

fn hardware() -> Hardware {
    let cpu_model = if cfg!(target_os = "macos") {
        let value = command_output("sysctl", &["-n", "machdep.cpu.brand_string"]);
        (value != "unavailable").then_some(value)
    } else {
        fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name\t: "))
                .map(str::to_owned)
        })
    };
    let cpu_count = if cfg!(target_os = "macos") {
        command_output("sysctl", &["-n", "hw.ncpu"]).parse().ok()
    } else {
        thread::available_parallelism()
            .ok()
            .map(|value| value.get() as u64)
    };
    let memory_bytes = if cfg!(target_os = "macos") {
        command_output("sysctl", &["-n", "hw.memsize"]).parse().ok()
    } else {
        fs::read_to_string("/proc/meminfo").ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("MemTotal:")?.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kib| kib.saturating_mul(1024))
        })
    };
    Hardware {
        os: env::consts::OS.to_owned(),
        arch: env::consts::ARCH.to_owned(),
        cpu_model,
        cpu_count,
        memory_bytes,
        // The runner is safe Rust and does not make a platform-specific RSS claim.  A parent
        // process can add peak RSS when the host exposes it; unavailable is honest here.
        peak_rss_bytes: None,
        cpu_time_ns: None,
    }
}

fn toolchain() -> Toolchain {
    Toolchain {
        rustc: command_output("rustc", &["--version", "--verbose"]),
        cargo: command_output("cargo", &["--version"]),
        nix: command_output("nix", &["--version"]),
    }
}

fn extension_language(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "rs" => Some("rust"),
        "ts" | "tsx" | "js" => Some("typescript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "cs" => Some("csharp"),
        "cpp" | "cc" | "c" | "h" => Some("clang"),
        _ => None,
    }
}

fn walk_sources(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let excluded = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| matches!(name, ".git" | ".local" | "target"));
            if !excluded {
                walk_sources(&path, out)?;
            }
        } else if extension_language(&path).is_some() {
            out.push(path);
        }
    }
    Ok(())
}

fn read_corpus(root: &Path, source_kind: &str) -> BenchResult<Vec<SourceFile>> {
    let mut paths = Vec::new();
    walk_sources(root, &mut paths)?;
    let mut files = Vec::new();
    for path in paths {
        let Some(language) = extension_language(&path) else {
            continue;
        };
        let bytes = fs::read(&path)?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| format!("{source_kind} source is not UTF-8: {}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        files.push(SourceFile {
            language: language.to_owned(),
            path,
            relative,
            text,
            bytes: bytes.len(),
        });
    }
    Ok(files)
}

fn configured_corpora() -> Vec<(String, PathBuf)> {
    [
        ("rust", "NUDOX_RUST_CORPUS_DIR"),
        ("typescript", "NUDOX_TYPESCRIPT_CORPUS_DIR"),
        ("python", "NUDOX_PYTHON_CORPUS_DIR"),
        ("go", "NUDOX_GO_CORPUS_DIR"),
        ("java", "NUDOX_JAVA_CORPUS_DIR"),
        ("csharp", "NUDOX_CSHARP_CORPUS_DIR"),
        ("clang", "NUDOX_CLANG_CORPUS_DIR"),
    ]
    .into_iter()
    .filter_map(|(language, variable)| {
        env::var_os(variable)
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .map(|path| (language.to_owned(), path))
    })
    .collect()
}

fn discover_files() -> BenchResult<(Vec<SourceFile>, String, String)> {
    let configured = configured_corpora();
    if !configured.is_empty() {
        let mut all = Vec::new();
        for (language, root) in configured {
            for mut file in read_corpus(&root, "nix-corpus")? {
                file.language = language.clone();
                all.push(file);
            }
        }
        all.retain(|file| file.bytes <= MAX_RELATION_ROW_BYTES);
        all.sort_by(|left, right| {
            left.relative
                .cmp(&right.relative)
                .then(left.path.cmp(&right.path))
        });
        return Ok((
            all,
            "nix-configured-row-capped".to_owned(),
            "configured Nix corpus roots".to_owned(),
        ));
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let files = read_corpus(&workspace, "workspace-source-tree")?
        .into_iter()
        .filter(|file| file.bytes <= MAX_RELATION_ROW_BYTES)
        .collect::<Vec<_>>();
    Ok((
        files,
        "workspace-source-tree-row-capped".to_owned(),
        workspace.display().to_string(),
    ))
}

fn corpus_classes(mut files: Vec<SourceFile>) -> Vec<CorpusClass> {
    files.sort_by(|left, right| {
        left.language
            .cmp(&right.language)
            .then(left.relative.cmp(&right.relative))
    });
    // Pick one file per language first so every size class remains genuinely multilingual when
    // the configured fleet corpus is present.  The remaining files retain canonical path order.
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for file in &files {
        if seen.insert(file.language.clone()) {
            selected.push(file.clone());
        }
    }
    let selected_paths = selected
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    selected.extend(
        files
            .into_iter()
            .filter(|file| !selected_paths.contains(&file.path)),
    );
    let small_len = selected.len().min(3).max(1);
    let medium_len = selected.len().min(5).max(small_len);
    let mut large = Vec::new();
    let mut large_bytes: usize = 0;
    for file in selected.iter().cloned() {
        if large.len() >= MAX_LARGE_CORPUS_FILES
            || (!large.is_empty()
                && large_bytes.saturating_add(file.bytes) > MAX_LARGE_CORPUS_BYTES)
        {
            break;
        }
        large_bytes = large_bytes.saturating_add(file.bytes);
        large.push(file);
    }
    vec![
        CorpusClass {
            name: "small",
            files: selected[..small_len].to_vec(),
        },
        CorpusClass {
            name: "medium",
            files: selected[..medium_len].to_vec(),
        },
        CorpusClass {
            name: "large",
            files: large,
        },
    ]
}

fn source_entity(file: &SourceFile) -> BenchResult<backend_semantic::EntityId> {
    let source = Source::new(0x4e_u128, file.relative.clone())?;
    Ok(entity_key(&Entity::new(
        source,
        format!("{}::root", file.language),
        None,
    )?))
}

fn index_documents(
    files: &[SourceFile],
) -> BenchResult<Vec<(backend_semantic::EntityId, Vec<(String, String)>)>> {
    files
        .iter()
        .map(|file| {
            Ok((
                source_entity(file)?,
                vec![
                    // DocumentState canonicalizes fields lexicographically
                    // before deriving the relation root. Keep fixture fields
                    // in that order so the binding uses the same root.
                    ("body".to_owned(), file.text.clone()),
                    (
                        "documentation".to_owned(),
                        "polyglot multilingual source".to_owned(),
                    ),
                    (
                        "name".to_owned(),
                        format!("polyglot {} {}", file.language, file.relative),
                    ),
                ],
            ))
        })
        .collect()
}

fn benchmark_tantivy_limits() -> TantivyLimits {
    TantivyLimits {
        max_field_bytes: 512 * 1024,
        ..TantivyLimits::default()
    }
}

fn build_document_state(files: &[SourceFile]) -> BenchResult<DocumentState> {
    let coverage = authorized_coverage(&[7; 32]);
    let mut documents = index_documents(files)?;
    documents.sort_by_key(|(id, _)| *id);
    let relation =
        RelationState::<IndexRelation>::from_entries(documents.iter().cloned(), coverage)?;
    let binding = Binding::new(
        workspace_root(),
        relation.root(),
        Recipe::from_value(&[1; 32]),
        Authority::from_value(&[2; 32]),
        ReadManifest::from_value(b"bench-reads"),
    );
    Ok(DocumentState::new(
        binding,
        coverage,
        documents,
        benchmark_tantivy_limits(),
    )?)
}

fn dir_bytes(root: &Path) -> usize {
    fn recurse(path: &Path, total: &mut usize) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                recurse(&path, total);
            } else if let Ok(metadata) = path.metadata() {
                *total = total.saturating_add(metadata.len() as usize);
            }
        }
    }
    let mut total = 0;
    recurse(root, &mut total);
    total
}

fn temp_root(label: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "nudox-integrated-bench-{label}-{}",
        std::process::id()
    ))
}

fn run_ingest(
    classes: &[CorpusClass],
    source_kind: &str,
    profile: Profile,
) -> BenchResult<(Vec<IngestMeasurement>, Vec<DeltaMeasurement>)> {
    let mut ingest = Vec::new();
    let mut deltas = Vec::new();
    for class in classes {
        let mut cold_timings = Vec::new();
        let mut warm_timings = Vec::new();
        let mut final_state = None;
        for sample in 0..profile.repetitions() {
            let started = Instant::now();
            let state = build_document_state(&class.files)?;
            let _source = TantivySource::build(&state, benchmark_tantivy_limits())?;
            let elapsed = started.elapsed().as_nanos();
            if sample == 0 {
                cold_timings.push(elapsed);
            } else {
                warm_timings.push(elapsed);
            }
            final_state = Some(state);
        }
        let state = final_state.ok_or("ingest did not produce a state")?;
        let directory = temp_root(&format!("tantivy-{}", class.name));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory)?;
        let durable_started = Instant::now();
        let _durable = TantivySource::build_in_dir(&state, benchmark_tantivy_limits(), &directory)?;
        // `open_in_dir` verifies the persisted binding, schema, and token count against the
        // authoritative state. A successful reopen is the durable-root correctness assertion.
        let _reopened = TantivySource::open_in_dir(&state, benchmark_tantivy_limits(), &directory)?;
        let durable_ns = durable_started.elapsed().as_nanos();
        let bytes_written = dir_bytes(&directory);
        let output_root = root_hex(state.binding().root.as_bytes());
        ingest.push(IngestMeasurement {
            schema: JSON_SCHEMA,
            status: "ok",
            size_class: class.name.to_owned(),
            source_kind: source_kind.to_owned(),
            source_files: class.files.len(),
            source_bytes: class.bytes(),
            phase: "durable_publish_and_reopen".to_owned(),
            wall: stats(&mut vec![durable_ns]),
            cpu_time_ns: None,
            peak_rss_bytes: None,
            bytes_read: class.bytes(),
            bytes_written,
            cas_reused_bytes: 0,
            semantic_rows_rebuilt: class.files.len(),
            output_root: output_root.clone(),
            correctness: Correctness {
                passed: true,
                assertions: vec![
                    "cold Tantivy source build completed".to_owned(),
                    "durable Tantivy reopen binding checked".to_owned(),
                ],
            },
        });
        ingest.push(IngestMeasurement {
            schema: JSON_SCHEMA,
            status: "ok",
            size_class: class.name.to_owned(),
            source_kind: source_kind.to_owned(),
            source_files: class.files.len(),
            source_bytes: class.bytes(),
            phase: "warm_in_memory_build".to_owned(),
            wall: stats(&mut warm_timings),
            cpu_time_ns: None,
            peak_rss_bytes: None,
            bytes_read: class.bytes(),
            bytes_written: 0,
            cas_reused_bytes: 0,
            semantic_rows_rebuilt: class.files.len(),
            output_root: output_root.clone(),
            correctness: Correctness {
                passed: true,
                assertions: vec!["warm in-memory Tantivy builds completed".to_owned()],
            },
        });

        let documents = index_documents(&class.files)?;
        let first = documents.first().ok_or("delta corpus has no rows")?;
        let mut changed_fields = first.1.clone();
        changed_fields.push(("delta".to_owned(), "one-file-delta".to_owned()));
        let changed_bytes = changed_fields
            .iter()
            .map(|(field, text)| field.len().saturating_add(text.len()))
            .sum::<usize>();
        let delta = state.prepare_delta(vec![DocumentChange::Add {
            id: first.0,
            fields: changed_fields,
        }])?;
        let no_op_delta = state.prepare_delta(vec![DocumentChange::Add {
            id: first.0,
            fields: first.1.clone(),
        }])?;
        let lexical = LexicalView::from_state(&state)?;
        let mut delta_timings = Vec::new();
        let mut target_root = state.binding().root;
        let no_op_root;
        let mut bounded_work = true;
        for _ in 0..profile.repetitions() {
            let started = Instant::now();
            let next = state.apply_delta(&delta)?;
            target_root = next.binding().root;
            bounded_work &= match lexical.advance(&delta, OverlayLimits::default())? {
                RefreshOutcome::Advanced(_) => true,
                RefreshOutcome::Reused(_) | RefreshOutcome::RebuildRequired(_) => false,
            };
            delta_timings.push(started.elapsed().as_nanos());
        }
        let no_op = state.apply_delta(&no_op_delta)?;
        no_op_root = no_op.binding().root;
        bounded_work &= match lexical.advance(&no_op_delta, OverlayLimits::default())? {
            RefreshOutcome::Reused(_) => true,
            RefreshOutcome::Advanced(_) | RefreshOutcome::RebuildRequired(_) => false,
        };
        let mut delta_assertions = vec![
            "one-file transition applied".to_owned(),
            "overlay stayed within bounded work budget".to_owned(),
        ];
        if no_op_root == state.binding().root {
            delta_assertions.push("no-op root is identical".to_owned());
        } else {
            delta_assertions
                .push("no-op root changed: backend retained a noncanonical no-op".to_owned());
        }
        deltas.push(DeltaMeasurement {
            schema: JSON_SCHEMA,
            status: "ok",
            size_class: class.name.to_owned(),
            phase: "warm_overlay".to_owned(),
            wall: stats(&mut delta_timings),
            bytes_read: first
                .1
                .iter()
                .map(|(field, text)| field.len() + text.len())
                .sum(),
            bytes_written: changed_bytes,
            cas_reused_bytes: class.bytes().saturating_sub(changed_bytes),
            semantic_rows_rebuilt: 1,
            changed_rows: 1,
            base_root: output_root,
            output_root: root_hex(target_root.as_bytes()),
            no_op_root: root_hex(no_op_root.as_bytes()),
            no_op_root_identical: no_op_root == state.binding().root,
            bounded_work,
            correctness: Correctness {
                passed: bounded_work && no_op_root == state.binding().root,
                assertions: delta_assertions,
            },
        });
        let _ = fs::remove_dir_all(directory);
    }
    Ok((ingest, deltas))
}

fn run_search(classes: &[CorpusClass], profile: Profile) -> BenchResult<Vec<SearchMeasurement>> {
    let mut measurements = Vec::new();
    for class in classes {
        let state = build_document_state(&class.files)?;
        let source = Arc::new(TantivySource::build(&state, benchmark_tantivy_limits())?);
        let root = root_hex(state.binding().root.as_bytes());
        let queries = [
            (
                "exact",
                LexQuery::new(vec!["polyglot".to_owned()], benchmark_tantivy_limits())?,
            ),
            (
                "prefix",
                LexQuery::prefix(vec!["poly".to_owned()], benchmark_tantivy_limits())?,
            ),
            (
                "full_text",
                LexQuery::with_options(
                    vec!["polyglot".to_owned(), "multilingual".to_owned()],
                    MatchMode::Exact,
                    FieldSelection::All,
                    CaseSensitivity::FoldAscii,
                    benchmark_tantivy_limits(),
                )?,
            ),
        ];
        let lexical = LexicalView::from_state(&state)?;
        let page = lexical.page(&queries[0].1, None, 1, benchmark_tantivy_limits())?;
        let cursor_root_stable = page.binding == state.binding();
        if !cursor_root_stable {
            return Err("search cursor root changed".into());
        }
        for (mode, query) in queries {
            let cold_start = Instant::now();
            let expected = source.search(&query)?;
            let cold = cold_start.elapsed().as_nanos();
            let expected_ids = expected.iter().map(|hit| hit.document).collect::<Vec<_>>();
            let mut warm_timings = Vec::new();
            for _ in 0..profile.repetitions() {
                let started = Instant::now();
                let observed = source.search(&query)?;
                if observed.iter().map(|hit| hit.document).collect::<Vec<_>>() != expected_ids {
                    return Err(format!("{mode} query changed result ordering").into());
                }
                warm_timings.push(started.elapsed().as_nanos());
            }
            for &readers in profile.readers() {
                let mut concurrent_timings = Vec::new();
                let mut handles = Vec::new();
                for _ in 0..readers {
                    let source = Arc::clone(&source);
                    let query = query.clone();
                    handles.push(thread::spawn(
                        move || -> Result<(u128, Vec<backend_semantic::EntityId>), String> {
                            let started = Instant::now();
                            let mut ids = Vec::new();
                            for _ in 0..2 {
                                ids = source
                                    .search(&query)
                                    .map_err(|error| error.to_string())?
                                    .into_iter()
                                    .map(|hit| hit.document)
                                    .collect();
                            }
                            Ok((started.elapsed().as_nanos(), ids))
                        },
                    ));
                }
                for handle in handles {
                    let (elapsed, ids) = handle
                        .join()
                        .map_err(|_| "search reader panicked")?
                        .map_err(|error| error.into_boxed_str())
                        .map_err(|error| format!("search reader failed: {error:?}"))?;
                    if ids != expected_ids {
                        return Err(
                            format!("{mode} query reader returned different results").into()
                        );
                    }
                    concurrent_timings.push(elapsed);
                }
                measurements.push(SearchMeasurement {
                    schema: JSON_SCHEMA,
                    status: "ok",
                    size_class: class.name.to_owned(),
                    mode: mode.to_owned(),
                    cache: "warm".to_owned(),
                    readers,
                    first_in_memory_search: (readers == 1).then_some(stats(&mut vec![cold])),
                    warm: stats(&mut concurrent_timings),
                    result_count: expected_ids.len(),
                    bytes_read: class.bytes(),
                    bytes_written: 0,
                    output_root: root.clone(),
                    cursor_root_stable,
                    correctness: Correctness {
                        passed: !expected_ids.is_empty() && cursor_root_stable,
                        assertions: vec![
                            "exact typed result IDs remained stable".to_owned(),
                            "concurrent readers agreed on ranked IDs".to_owned(),
                            "binding root remained stable".to_owned(),
                        ],
                    },
                });
            }
        }
    }
    Ok(measurements)
}

fn build_view(class: &CorpusClass) -> BenchResult<(ViewRoot, CoverageCapability)> {
    let object = object_version(b"nudox-integrated-view-source");
    let capability = coverage_capability(b"nudox-integrated-view-source")?;
    let basis_root =
        backend_library::view_state_root(&[("source".to_owned(), "polyglot".to_owned())]);
    let basis = Basis::new(basis_root, object);
    let frontier = Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0);
    let package = package_key("polyglot@0.1.0");
    let mut rows = vec![Row::new(RowId::Package(package), basis, "polyglot@0.1.0")];
    for file in &class.files {
        let symbol = symbol_key(&format!("polyglot::{}:1::root", file.relative));
        let location = SourceLocation::new(file.relative.clone(), 1)?;
        let row = Row::in_package(
            RowId::Symbol(symbol),
            basis,
            package,
            format!("polyglot::{}:1::root", file.relative),
        )
        .with_document(vec![Fragment::Code(file.text.clone())])
        .with_signature(format!("fn {}::root()", file.language))
        .with_kind(DeclarationKind::Function)
        .with_source(location)
        .with_excerpt(SourceExcerpt::capture_bounded(&file.text));
        rows.push(row);
    }
    let view = ViewRoot::new_checked(
        view_key(b"nudox-integrated-view"),
        basis,
        frontier,
        rows,
        vec![Coverage::Complete],
        capability.clone(),
    )
    .map_err(|error| format!("view construction failed: {error:?}"))?;
    Ok((view, capability))
}

fn database_pack_bytes(path: &Path) -> (usize, usize) {
    let database = dir_bytes(path);
    let pack = fs::read_dir(path)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
                .filter_map(|entry| entry.metadata().ok())
                .map(|metadata| metadata.len() as usize)
                .sum()
        })
        .unwrap_or(0);
    (database, pack)
}

fn run_catalog(class: &CorpusClass, profile: Profile) -> BenchResult<Vec<CatalogMeasurement>> {
    let (view, capability) = build_view(class)?;
    let root = root_hex(view.root().as_bytes());
    let path = temp_root("turso");
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path)?;
    let database_path = path.join(backend_extension_turso::FILE_NAME);
    let mut measurements = Vec::new();
    let mut projection = futures_executor::block_on(TursoProjection::open(&database_path))?;
    let cold_started = Instant::now();
    let cold_update = futures_executor::block_on(projection.synchronize(&view))?;
    if !matches!(cold_update, ProjectionUpdate::Rebuilt { .. }) {
        return Err("unexpected Turso cold update".into());
    }
    let cold_timing = cold_started.elapsed().as_nanos();
    let (database_bytes, pack_bytes) = database_pack_bytes(&path);
    measurements.push(CatalogMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        size_class: class.name.to_owned(),
        operation: "cold_open_synchronize".to_owned(),
        phase: "cold_open".to_owned(),
        wall: stats(&mut vec![cold_timing]),
        rows: view.row_count() as usize,
        database_bytes,
        pack_bytes,
        output_root: root.clone(),
        correctness: Correctness {
            passed: true,
            assertions: vec!["Turso projection rebuilt and root-fenced".to_owned()],
        },
    });

    let mut warm_timings = Vec::new();
    for _ in 0..profile.repetitions().saturating_sub(1) {
        let started = Instant::now();
        let update = futures_executor::block_on(projection.synchronize(&view))?;
        if !matches!(update, ProjectionUpdate::Reused { .. }) {
            return Err("Turso warm synchronize did not reuse the exact root".into());
        }
        warm_timings.push(started.elapsed().as_nanos());
    }
    measurements.push(CatalogMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        size_class: class.name.to_owned(),
        operation: "warm_synchronize_no_op".to_owned(),
        phase: "warm".to_owned(),
        wall: stats(&mut warm_timings),
        rows: view.row_count() as usize,
        database_bytes,
        pack_bytes,
        output_root: root.clone(),
        correctness: Correctness {
            passed: true,
            assertions: vec!["exact-root synchronize reused without SQL rebuild".to_owned()],
        },
    });

    drop(projection);
    let restart_started = Instant::now();
    let mut restarted = futures_executor::block_on(TursoProjection::open(&database_path))?;
    let restart_update = futures_executor::block_on(restarted.synchronize(&view))?;
    if !matches!(restart_update, ProjectionUpdate::Reused { .. }) {
        return Err("Turso restart did not reuse the exact root".into());
    }
    measurements.push(CatalogMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        size_class: class.name.to_owned(),
        operation: "restart_recovery_no_op_304".to_owned(),
        phase: "cold_restart".to_owned(),
        wall: stats(&mut vec![restart_started.elapsed().as_nanos()]),
        rows: view.row_count() as usize,
        database_bytes,
        pack_bytes,
        output_root: root.clone(),
        correctness: Correctness {
            passed: true,
            assertions: vec!["exact-root synchronize reused without SQL rebuild".to_owned()],
        },
    });

    let first_symbol = class
        .files
        .first()
        .ok_or("catalog class has no source file")?;
    let package = package_key("polyglot@0.1.0");
    let symbol = symbol_key(&format!("polyglot::{}:1::root", first_symbol.relative));
    let next_row = Row::in_package(
        RowId::Symbol(symbol),
        view.basis(),
        package,
        format!("polyglot::{}:1::root delta", first_symbol.relative),
    )
    .with_document(vec![Fragment::Code(format!(
        "{}\n// delta",
        first_symbol.text
    ))])
    .with_signature("fn delta()")
    .with_kind(DeclarationKind::Function);
    let prepared = view
        .prepare(ViewDelta::Upsert { row: next_row }, capability.clone())
        .map_err(|error| format!("view delta preparation failed: {error:?}"))?;
    let (next_view, committed) = view
        .clone()
        .commit(prepared)
        .map_err(|error| format!("view delta commit failed: {error:?}"))?;
    let delta_started = Instant::now();
    let update = futures_executor::block_on(restarted.apply(&committed))?;
    if !matches!(update, ProjectionUpdate::Advanced { .. }) {
        return Err("Turso one-package delta did not advance".into());
    }
    measurements.push(CatalogMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        size_class: class.name.to_owned(),
        operation: "one_package_delta_publication".to_owned(),
        phase: "warm_delta".to_owned(),
        wall: stats(&mut vec![delta_started.elapsed().as_nanos()]),
        rows: next_view.row_count() as usize,
        database_bytes: database_pack_bytes(&path).0,
        pack_bytes: database_pack_bytes(&path).1,
        output_root: root_hex(next_view.root().as_bytes()),
        correctness: Correctness {
            passed: committed.base_root() == view.root()
                && committed.target_root() == next_view.root(),
            assertions: vec!["checked view delta applied atomically".to_owned()],
        },
    });

    let search_started = Instant::now();
    let rows = futures_executor::block_on(restarted.search("polyglot", 16))?;
    measurements.push(CatalogMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        size_class: class.name.to_owned(),
        operation: "catalog_search_read".to_owned(),
        phase: "warm".to_owned(),
        wall: stats(&mut vec![search_started.elapsed().as_nanos()]),
        rows: rows.ids.len(),
        database_bytes: database_pack_bytes(&path).0,
        pack_bytes: database_pack_bytes(&path).1,
        output_root: root_hex(next_view.root().as_bytes()),
        correctness: Correctness {
            passed: !rows.ids.is_empty(),
            assertions: vec!["catalog search returned real projected rows".to_owned()],
        },
    });
    let (final_database_bytes, final_pack_bytes) = database_pack_bytes(&path);
    for operation in [
        "dependency_dependent_traversal",
        "registry_read",
        "advisory_read",
        "concurrent_readers_writer",
    ] {
        measurements.push(CatalogMeasurement {
            schema: JSON_SCHEMA,
            status: "unavailable",
            size_class: class.name.to_owned(),
            operation: operation.to_owned(),
            phase: "unavailable".to_owned(),
            wall: stats(&mut Vec::new()),
            rows: 0,
            database_bytes: final_database_bytes,
            pack_bytes: final_pack_bytes,
            output_root: root_hex(next_view.root().as_bytes()),
            correctness: Correctness {
                passed: false,
                assertions: vec![
                    "no public production registry/advisory/traversal or concurrent writer seam was configured"
                        .to_owned(),
                ],
            },
        });
    }
    let _ = fs::remove_dir_all(path);
    Ok(measurements)
}

struct LibraryEngine(Library);

impl LocalEngine for LibraryEngine {
    fn execute(&mut self, request: backend_library::CommandDto) -> backend_library::ReplyDto {
        self.0.execute_dto(request)
    }
}

#[cfg(unix)]
fn run_authenticated_surfaces(
    endpoint: &Path,
    profile: Profile,
) -> BenchResult<Vec<SurfaceMeasurement>> {
    let mut measurements = Vec::new();
    let mut cli_transport = backend_client::UnixCommandTransport::connect(endpoint)
        .map_err(|error| format!("authenticated CLI transport failed: {error}"))?;
    let mut cli_timings = Vec::new();
    let mut cli_bytes = 0;
    for _ in 0..profile.repetitions() {
        let started = Instant::now();
        let reply =
            backend_cli::execute_with_transport(&mut cli_transport, LibraryCommand::Packages)
                .map_err(|error| format!("authenticated CLI request failed: {error}"))?;
        cli_bytes = backend_cli::run_json(&reply).len();
        cli_timings.push(started.elapsed().as_nanos());
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cli_packages_authenticated_unix".to_owned(),
        phase: "authenticated_unix_cli",
        transport: "authenticated_unix",
        wall: stats(&mut cli_timings),
        payload_bytes: cli_bytes,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected: false,
        correctness: Correctness {
            passed: cli_bytes > 0,
            assertions: vec![
                "CLI request crossed an authenticated Unix locald transport".to_owned(),
            ],
        },
    });

    let mut mcp_transport = backend_client::UnixCommandTransport::connect(endpoint)
        .map_err(|error| format!("authenticated MCP transport failed: {error}"))?;
    let mut mcp_timings = Vec::new();
    let mut mcp_bytes = 0;
    for ordinal in 0..profile.repetitions() {
        let request = CommandDto::new(ordinal as u64 + 1, LibraryCommand::Packages);
        let started = Instant::now();
        let frame = backend_mcp::encode_request(&request)?;
        let output = backend_mcp::dispatch_frame_with_transport(&mut mcp_transport, &frame)
            .map_err(|error| format!("authenticated MCP request failed: {error}"))?;
        mcp_bytes = output.len();
        mcp_timings.push(started.elapsed().as_nanos());
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "mcp_packages_authenticated_unix".to_owned(),
        phase: "authenticated_unix_mcp",
        transport: "authenticated_unix",
        wall: stats(&mut mcp_timings),
        payload_bytes: mcp_bytes,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected: false,
        correctness: Correctness {
            passed: mcp_bytes > 0,
            assertions: vec![
                "MCP request crossed an authenticated Unix locald transport".to_owned(),
            ],
        },
    });
    Ok(measurements)
}

fn run_surfaces(class: &CorpusClass, profile: Profile) -> BenchResult<Vec<SurfaceMeasurement>> {
    #[cfg(unix)]
    if let Some(endpoint) = env::var_os("BACKEND_LOCALD_ENDPOINT") {
        let endpoint = Path::new(&endpoint);
        if endpoint.exists() {
            return run_authenticated_surfaces(endpoint, profile);
        }
    }
    let (view, _) = build_view(class)?;
    let basis = view.root();
    let library = Library::from_view(view.clone(), Cursor::for_view_root(&view))?;
    let limit = QueryLimit::new(1).ok_or("invalid query limit")?;
    let mut measurements = Vec::new();
    let mut cli = LibraryEngine(library.clone());
    let mut mcp = LibraryEngine(library.clone());

    let mut list_times = Vec::new();
    let mut payload_bytes = 0;
    for _ in 0..profile.repetitions() {
        let started = Instant::now();
        let reply = backend_cli::execute(&mut cli, backend_cli::Command::Packages);
        let encoded = backend_cli::run_json(&reply);
        payload_bytes = encoded.len();
        list_times.push(started.elapsed().as_nanos());
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cli_list".to_owned(),
        phase: "in_process_cli",
        transport: "in_process",
        wall: stats(&mut list_times),
        payload_bytes,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected: false,
        correctness: Correctness {
            passed: payload_bytes > 0,
            assertions: vec!["CLI used production Library DTO path".to_owned()],
        },
    });

    let query = Query::new("polyglot", basis, limit);
    let mut search_times = Vec::new();
    for _ in 0..profile.repetitions() {
        let started = Instant::now();
        let reply = backend_cli::execute(&mut cli, backend_cli::Command::Search(query.clone()));
        payload_bytes = backend_cli::run_json(&reply).len();
        search_times.push(started.elapsed().as_nanos());
    }
    let first = library.execute(backend_library::Command::Search(query.clone()))?;
    let (continuation, continuation_root) = match first {
        backend_library::CommandReply::Search(snapshot) => (snapshot.next, snapshot.root.root()),
        _ => (None, basis),
    };
    if let Some(cursor) = continuation {
        let mut continuation_times = Vec::new();
        let cursor_root_stable = cursor.root() == continuation_root;
        let mut continuation_basis_stable = true;
        for _ in 0..profile.repetitions() {
            let started = Instant::now();
            let reply = library.execute(backend_library::Command::Search(
                query.clone().with_cursor(cursor),
            ))?;
            continuation_basis_stable &= match reply {
                backend_library::CommandReply::Search(snapshot) => {
                    snapshot.root.basis().root == basis
                }
                _ => false,
            };
            continuation_times.push(started.elapsed().as_nanos());
        }
        measurements.push(SurfaceMeasurement {
            schema: JSON_SCHEMA,
            status: "ok",
            operation: "cli_search_continuation".to_owned(),
            phase: "in_process_library",
            transport: "in_process",
            wall: stats(&mut continuation_times),
            payload_bytes,
            allocation_bytes: None,
            continuation: true,
            hard_budget_rejected: false,
            correctness: Correctness {
                passed: cursor_root_stable && continuation_basis_stable,
                assertions: vec![
                    "continuation cursor retained the exact preceding page root".to_owned(),
                    "continuation pages retained the immutable source basis".to_owned(),
                ],
            },
        });
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cli_search".to_owned(),
        phase: "in_process_cli",
        transport: "in_process",
        wall: stats(&mut search_times),
        payload_bytes,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected: false,
        correctness: Correctness {
            passed: payload_bytes > 0,
            assertions: vec!["CLI search returned a bounded wire payload".to_owned()],
        },
    });

    let mut mcp_times = Vec::new();
    let mut mcp_bytes = 0;
    for ordinal in 0..profile.repetitions() {
        let started = Instant::now();
        let reply = backend_mcp::call(
            &mut mcp,
            ordinal as u64 + 1,
            backend_mcp::Command::Search(query.clone()),
        );
        mcp_bytes = serde_json::to_vec(&reply)?.len();
        mcp_times.push(started.elapsed().as_nanos());
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "mcp_search".to_owned(),
        phase: "in_process_mcp",
        transport: "in_process",
        wall: stats(&mut mcp_times),
        payload_bytes: mcp_bytes,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected: false,
        correctness: Correctness {
            passed: mcp_bytes > 0,
            assertions: vec!["MCP call used production in-process transport adapter".to_owned()],
        },
    });

    let oversized_request = CommandDto::new(
        7,
        LibraryCommand::Resolve {
            text: "x".repeat(backend_library::MAX_COMMAND_TEXT + 1),
        },
    );
    let mut rejection_timings = Vec::new();
    let mut hard_budget_rejected = true;
    for _ in 0..profile.repetitions() {
        let started = Instant::now();
        let invalid_limit = QueryLimit::new(0);
        let admission = backend_library::admit_request(&oversized_request);
        rejection_timings.push(started.elapsed().as_nanos());
        hard_budget_rejected &=
            invalid_limit.is_none() && admission == Err(RequestAdmissionError::TextTooLarge);
    }
    measurements.push(SurfaceMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "hard_budget_admission_rejection".to_owned(),
        phase: "boundary_rejection",
        transport: "in_process",
        wall: stats(&mut rejection_timings),
        payload_bytes: backend_library::MAX_COMMAND_TEXT + 1,
        allocation_bytes: None,
        continuation: false,
        hard_budget_rejected,
        correctness: Correctness {
            passed: hard_budget_rejected,
            assertions: vec![
                "zero query limit and oversized text remained rejected at the boundary".to_owned(),
            ],
        },
    });
    Ok(measurements)
}

#[derive(Default)]
struct LoopbackFixtureCounters {
    requests: AtomicU64,
    feed_requests: AtomicU64,
    archive_requests: AtomicU64,
    response_bytes: AtomicU64,
    feed_response_bytes: AtomicU64,
    archive_response_bytes: AtomicU64,
}

struct LoopbackFixtureReport {
    requests: u64,
    feed_requests: u64,
    archive_requests: u64,
    response_bytes: u64,
    feed_response_bytes: u64,
    archive_response_bytes: u64,
}

fn read_http_request(stream: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while bytes.len() < 8 * 1024 {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("loopback fixture request read failed: {error}"))?;
        if read == 0 {
            return Err("loopback fixture client closed before request headers".to_owned());
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let line = bytes
                .split(|byte| *byte == b'\n')
                .next()
                .ok_or_else(|| "loopback fixture request line was empty".to_owned())?;
            return String::from_utf8(line.trim_ascii().to_vec())
                .map_err(|_| "loopback fixture request line was not UTF-8".to_owned());
        }
    }
    Err("loopback fixture request headers exceeded 8 KiB".to_owned())
}

fn serve_loopback_registry(
    listener: TcpListener,
    feed: Vec<u8>,
    archive: Vec<u8>,
    counters: Arc<LoopbackFixtureCounters>,
) -> Result<LoopbackFixtureReport, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("loopback fixture nonblocking setup failed: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while counters.requests.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| format!("loopback fixture blocking setup failed: {error}"))?;
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .map_err(|error| format!("loopback fixture read timeout failed: {error}"))?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .map_err(|error| format!("loopback fixture write timeout failed: {error}"))?;
                let request = read_http_request(&mut stream)?;
                let path = request
                    .split_ascii_whitespace()
                    .nth(1)
                    .ok_or_else(|| "loopback fixture request path was missing".to_owned())?;
                let (kind, body) = if path.starts_with("/feed?") {
                    ("feed", feed.as_slice())
                } else if path == "/archive" {
                    ("archive", archive.as_slice())
                } else {
                    return Err(format!(
                        "loopback fixture received unexpected path {path:?}"
                    ));
                };
                counters.requests.fetch_add(1, Ordering::AcqRel);
                match kind {
                    "feed" => {
                        counters.feed_requests.fetch_add(1, Ordering::AcqRel);
                        counters
                            .feed_response_bytes
                            .fetch_add(body.len() as u64, Ordering::AcqRel);
                    }
                    "archive" => {
                        counters.archive_requests.fetch_add(1, Ordering::AcqRel);
                        counters
                            .archive_response_bytes
                            .fetch_add(body.len() as u64, Ordering::AcqRel);
                    }
                    _ => unreachable!("fixture response kind is closed above"),
                }
                counters
                    .response_bytes
                    .fetch_add(body.len() as u64, Ordering::AcqRel);
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .and_then(|_| stream.write_all(body))
                    .map_err(|error| format!("loopback fixture response write failed: {error}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(format!("loopback fixture accept failed: {error}")),
        }
    }
    let report = LoopbackFixtureReport {
        requests: counters.requests.load(Ordering::Acquire),
        feed_requests: counters.feed_requests.load(Ordering::Acquire),
        archive_requests: counters.archive_requests.load(Ordering::Acquire),
        response_bytes: counters.response_bytes.load(Ordering::Acquire),
        feed_response_bytes: counters.feed_response_bytes.load(Ordering::Acquire),
        archive_response_bytes: counters.archive_response_bytes.load(Ordering::Acquire),
    };
    if report.requests != 2 || report.feed_requests != 1 || report.archive_requests != 1 {
        return Err(format!(
            "loopback fixture deadline expired after {} requests (feed={}, archive={})",
            report.requests, report.feed_requests, report.archive_requests
        ));
    }
    Ok(report)
}

fn start_loopback_registry(
    feed: Vec<u8>,
    archive: Vec<u8>,
) -> BenchResult<(
    String,
    Arc<LoopbackFixtureCounters>,
    thread::JoinHandle<Result<LoopbackFixtureReport, String>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let counters = Arc::new(LoopbackFixtureCounters::default());
    let thread_counters = Arc::clone(&counters);
    let handle =
        thread::spawn(move || serve_loopback_registry(listener, feed, archive, thread_counters));
    Ok((format!("http://{address}"), counters, handle))
}

fn gui_phase_stats(parsed: Option<&serde_json::Value>, phase: &str) -> Option<Stats> {
    let root = parsed?;
    let from_value = |value: &serde_json::Value| -> Option<Stats> {
        let mut samples = match value {
            serde_json::Value::Number(value) => {
                value.as_u64().map(u128::from).into_iter().collect()
            }
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(serde_json::Value::as_u64)
                .map(u128::from)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        (!samples.is_empty()).then(|| stats(&mut samples))
    };
    for container_name in ["phase_timings_ns", "phase_durations_ns", "timings_ns"] {
        if let Some(value) = root
            .get(container_name)
            .and_then(|container| container.get(phase))
            .and_then(from_value)
        {
            return Some(value);
        }
    }
    root.get("phases")
        .and_then(serde_json::Value::as_array)
        .and_then(|phases| {
            phases.iter().find_map(|entry| {
                let name = entry
                    .get("name")
                    .or_else(|| entry.get("phase"))
                    .and_then(serde_json::Value::as_str)?;
                if name != phase {
                    return None;
                }
                entry
                    .get("duration_ns")
                    .or_else(|| entry.get("elapsed_ns"))
                    .and_then(from_value)
            })
        })
}

fn verify_gui_package_artifacts(output: &Path) -> (Option<usize>, usize, Vec<String>) {
    let manifest_path = output
        .join("1440x1000@1x")
        .join("manifests")
        .join("package.json");
    let Ok(text) = fs::read_to_string(&manifest_path) else {
        return (None, 0, vec!["package manifest was not written".to_owned()]);
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (
            None,
            0,
            vec!["package manifest was not valid JSON".to_owned()],
        );
    };
    let Some(frames) = manifest.get("frames").and_then(serde_json::Value::as_array) else {
        return (None, 0, vec!["package manifest omitted frames".to_owned()]);
    };
    let Some(base) = manifest_path.parent().and_then(Path::parent) else {
        return (
            Some(frames.len()),
            0,
            vec!["package manifest base path was invalid".to_owned()],
        );
    };
    let mut verified = 0;
    let mut failures = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let Some(relative) = frame.get("path").and_then(serde_json::Value::as_str) else {
            failures.push(format!("frame {index} omitted its artifact path"));
            continue;
        };
        let path = base.join(relative);
        let Ok(bytes) = fs::read(&path) else {
            failures.push(format!(
                "frame {index} artifact is missing: {}",
                path.display()
            ));
            continue;
        };
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            verified += 1;
        } else {
            failures.push(format!("frame {index} artifact failed PNG verification"));
        }
    }
    (Some(frames.len()), verified, failures)
}

struct NetworkRound {
    callers: usize,
    cold_ns: u128,
    warm_ns: u128,
    restart_ns: u128,
    cold_result: Arc<backend_engine::acquisition::RegistryAcquisitionResult>,
    warm_result: Arc<backend_engine::acquisition::RegistryAcquisitionResult>,
    restart_result: Arc<backend_engine::acquisition::RegistryAcquisitionResult>,
    fixture: LoopbackFixtureReport,
    telemetry_leaders: u64,
    telemetry_followers: u64,
}

fn run_network_round(
    callers: usize,
    round: usize,
    endpoint_seed: &[u8],
    archive: &[u8],
    registry_limits: RegistryAcquisitionLimits,
) -> BenchResult<NetworkRound> {
    let (endpoint_url, fixture_counters, fixture_thread) =
        start_loopback_registry(endpoint_seed.to_vec(), archive.to_vec())?;
    let endpoint = RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_url.clone())?;
    let registry_root = temp_root(&format!("network-singleflight-{callers}-{round}"));
    let _ = fs::remove_dir_all(&registry_root);
    let (owner, _) = RegistryOwner::open(
        &registry_root,
        endpoint.clone(),
        AcquisitionPolicy::Online,
        registry_limits,
    )?;
    let service = Arc::new(AcquisitionService::from_owner(
        owner,
        registry_root.join("service-coordination"),
    )?);
    let request = AcquisitionRequest::new(
        service.source_id(),
        "pkg:cargo/integrated-fixture@1.0.0",
        RawArchiveObjectId::from_bytes(archive),
        1,
        0,
    )?;
    let barrier = Arc::new(Barrier::new(callers));
    let (outcomes, receiver) = mpsc::channel();
    let started = Instant::now();
    let mut threads = Vec::with_capacity(callers);
    for _ in 0..callers {
        let service = Arc::clone(&service);
        let barrier = Arc::clone(&barrier);
        let request = request.clone();
        let endpoint = endpoint.clone();
        let outcomes = outcomes.clone();
        threads.push(thread::spawn(move || {
            barrier.wait();
            let mut transport = HttpRegistryTransport::new(endpoint, None, registry_limits)
                .expect("loopback transport is valid");
            outcomes
                .send(service.acquire(&request, &mut transport))
                .ok();
        }));
    }
    drop(outcomes);
    let mut cold_result = None;
    let mut same_result = true;
    for _ in 0..callers {
        match receiver.recv_timeout(Duration::from_secs(4)) {
            Ok(AcquisitionOutcome::Hit(result)) => {
                if let Some(expected) = &cold_result {
                    same_result &= Arc::ptr_eq(expected, &result)
                        && Arc::ptr_eq(&expected.receipt, &result.receipt)
                        && Arc::ptr_eq(&expected.delta, &result.delta)
                        && expected.receipt.target == result.receipt.target;
                } else {
                    cold_result = Some(result);
                }
            }
            Ok(_) => return Err("singleflight caller returned a non-hit outcome".into()),
            Err(error) => return Err(format!("singleflight caller timed out: {error}").into()),
        }
    }
    for thread in threads {
        thread
            .join()
            .map_err(|_| "singleflight caller thread panicked".to_owned())?;
    }
    let cold_ns = started.elapsed().as_nanos();
    let fixture = fixture_thread
        .join()
        .map_err(|_| "loopback fixture thread panicked".to_owned())??;
    let cold_result = cold_result.ok_or("singleflight emitted no result")?;
    let telemetry = service.telemetry();
    let expected_network_bytes = endpoint_seed.len().saturating_add(archive.len());
    let fixture_ok = fixture.requests == 2
        && fixture.feed_requests == 1
        && fixture.archive_requests == 1
        && fixture.response_bytes as usize == expected_network_bytes
        && fixture_counters.requests.load(Ordering::Acquire) == 2;
    let byte_ok = fixture.feed_response_bytes as usize == endpoint_seed.len()
        && fixture.archive_response_bytes as usize == archive.len();
    let published_artifact_bytes = cold_result.artifact.bytes() == archive;
    if !same_result
        || !fixture_ok
        || !byte_ok
        || !published_artifact_bytes
        || telemetry.leaders != 1
        || telemetry.followers != (callers - 1) as u64
    {
        return Err(format!(
            "singleflight round {callers} failed identity/counter admission: leaders={}, followers={}, requests={}",
            telemetry.leaders, telemetry.followers, fixture.requests
        )
        .into());
    }

    let warm_started = Instant::now();
    let mut warm_transport = HttpRegistryTransport::new(endpoint.clone(), None, registry_limits)?;
    let warm_result = match service.acquire(&request, &mut warm_transport) {
        AcquisitionOutcome::Hit(result) => result,
        _ => return Err("warm registry cache resolution was not a hit".into()),
    };
    let warm_ns = warm_started.elapsed().as_nanos();
    // A warm lookup intentionally emits a new no-op delta/receipt from the
    // already-published target. Reuse is proved by retaining the same target
    // snapshot and immutable archive identity, while the receipt remains an
    // auditable fact about this lookup boundary.
    let warm_identity_ok = warm_result.receipt.target == cold_result.receipt.target
        && warm_result.artifact.version().as_ref() == cold_result.artifact.version().as_ref();
    if !warm_identity_ok {
        return Err("warm registry cache changed an immutable identity".into());
    }

    drop(service);
    let (restarted_owner, _) = RegistryOwner::open(
        &registry_root,
        endpoint,
        AcquisitionPolicy::Offline,
        registry_limits,
    )?;
    let restarted = AcquisitionService::from_owner(
        restarted_owner,
        registry_root.join("service-coordination"),
    )?;
    let restart_request = AcquisitionRequest::new(
        restarted.source_id(),
        "pkg:cargo/integrated-fixture@1.0.0",
        RawArchiveObjectId::from_bytes(archive),
        1,
        0,
    )?;
    let restart_started = Instant::now();
    let mut offline_transport = HttpRegistryTransport::new(
        RegistryEndpoint::new(RegistryEcosystem::Cargo, endpoint_url)?,
        None,
        registry_limits,
    )?;
    let restart_result = match restarted.acquire(&restart_request, &mut offline_transport) {
        AcquisitionOutcome::Hit(result) => result,
        other => return Err(format!("offline registry restart was not a hit: {other:?}").into()),
    };
    let restart_ns = restart_started.elapsed().as_nanos();
    let restart_identity_ok = restart_result.receipt.target == cold_result.receipt.target
        && restart_result.artifact.version().as_ref() == cold_result.artifact.version().as_ref();
    if !restart_identity_ok {
        return Err("restart registry cache changed an immutable identity".into());
    }
    let _ = fs::remove_dir_all(registry_root);
    Ok(NetworkRound {
        callers,
        cold_ns,
        warm_ns,
        restart_ns,
        cold_result,
        warm_result,
        restart_result,
        fixture,
        telemetry_leaders: telemetry.leaders,
        telemetry_followers: telemetry.followers,
    })
}

fn run_acquisition(
    class: &CorpusClass,
    profile: Profile,
) -> BenchResult<Vec<AcquisitionMeasurement>> {
    let limits = TransportLimits::default();
    let source = class
        .files
        .iter()
        .filter(|file| file.bytes <= limits.max_chunk)
        .max_by_key(|file| file.bytes)
        .ok_or("acquisition corpus has no source file within the 64 KiB frame budget")?;
    let bytes = source.text.as_bytes().to_vec();
    if bytes.is_empty() {
        return Err("acquisition source file has no bytes".into());
    }
    let path = temp_root("cas");
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path)?;
    let key = ObjectKey::<ImmutableObjectSchema>::from_value(&bytes);
    let version = ObjectVersion::<ImmutableObjectSchema>::from_value(&bytes);
    let authority_id =
        ObjectVersion::<ImmutableObjectSchema>::from_value(b"nudox-integrated-cas-authority");
    let authority = AuthorityClaim::from_typed(&authority_id, AuthorityEpoch(1));
    let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, limits)?;

    let cold_started = Instant::now();
    let frame = Frame::new(
        backend_engine::TransferId::new(1)?,
        key,
        version,
        ChunkParts {
            object_len: bytes.len() as u64,
            offset: 0,
            sequence: 0,
            previous_chain: ChunkChain([0; 32]),
            payload: bytes.clone(),
        },
        authority,
    )?;
    let admitted = cas.ingest_complete_frame(frame, authority)?;
    let first = cas
        .get(admitted)?
        .ok_or("cold CAS object was not readable")?;
    let cold_ns = cold_started.elapsed().as_nanos();
    let cas_root = MerkleRoot::from_admitted_manifest(
        1,
        ObjectVersion::<ImmutableObjectSchema>::from_value(b"nudox-integrated-cas-root"),
    );
    cas.record_input_claim(cas_root, WireIdentity::from_typed(&version))?;
    cas.record_root(cas_root)?;
    let (objects, retained_bytes) = cas.retained_usage();
    let mut measurements = vec![AcquisitionMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cas_streaming_first_resolution".to_owned(),
        phase: "cold".to_owned(),
        wall: stats(&mut vec![cold_ns]),
        calls: 1,
        buffer_ceiling_bytes: limits.max_chunk,
        downloaded_bytes: bytes.len(),
        reused_bytes: 0,
        memory_high_water_bytes: None,
        receipt_id: None,
        delta_id: None,
        target_root: None,
        artifact_id: None,
        correctness: Correctness {
            passed: admitted == version && first.as_ref() == bytes.as_slice() && objects == 1,
            assertions: vec![
                "complete production InputCas frame was admitted".to_owned(),
                "canonical object version and bytes were checked".to_owned(),
            ],
        },
    }];

    let mut warm_timings = Vec::new();
    let mut warm_correct = true;
    for _ in 0..32 {
        let started = Instant::now();
        let cached = cas.get(version)?.ok_or("warm CAS object disappeared")?;
        warm_correct &= cached.as_ref() == bytes.as_slice();
        warm_timings.push(started.elapsed().as_nanos());
    }
    measurements.push(AcquisitionMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cas_warm_32_calls".to_owned(),
        phase: "warm".to_owned(),
        wall: stats(&mut warm_timings),
        calls: 32,
        buffer_ceiling_bytes: limits.max_chunk,
        downloaded_bytes: 0,
        reused_bytes: bytes.len() * 32,
        memory_high_water_bytes: None,
        receipt_id: None,
        delta_id: None,
        target_root: None,
        artifact_id: None,
        correctness: Correctness {
            passed: warm_correct,
            assertions: vec!["32 warm reads reused the exact persisted CAS object".to_owned()],
        },
    });
    drop(cas);

    let restart_started = Instant::now();
    let mut reopened = InputCas::<ImmutableObjectSchema>::open(&path, limits)?;
    let offline = reopened
        .get(version)?
        .ok_or("offline CAS object was not recovered")?;
    let restart_ns = restart_started.elapsed().as_nanos();
    measurements.push(AcquisitionMeasurement {
        schema: JSON_SCHEMA,
        status: "ok",
        operation: "cas_offline_restart_resolution".to_owned(),
        phase: "warm_restart".to_owned(),
        wall: stats(&mut vec![restart_ns]),
        calls: 1,
        buffer_ceiling_bytes: limits.max_chunk,
        downloaded_bytes: 0,
        reused_bytes: bytes.len(),
        memory_high_water_bytes: None,
        receipt_id: None,
        delta_id: None,
        target_root: None,
        artifact_id: None,
        correctness: Correctness {
            passed: offline.as_ref() == bytes.as_slice() && retained_bytes >= bytes.len() as u64,
            assertions: vec!["reopened CAS served an offline persisted object".to_owned()],
        },
    });
    let archive = b"nudox integrated benchmark archive".to_vec();
    let artifact = CapabilityArtifactId::from_value(&archive);
    let endpoint_seed = format!(
        concat!(
            "{{\"schema\":1,\"next\":\"{}\",\"items\":[{{",
            "\"name\":\"integrated-fixture\",\"version\":\"1.0.0\",",
            "\"archive\":\"/archive\",\"blake3\":\"{}\",",
            "\"provenance\":\"{}\"}}]}}"
        ),
        hex(&[7; 32]),
        hex(artifact.as_bytes()),
        hex(&[9; 32]),
    );
    let mut registry_limits = RegistryAcquisitionLimits::default();
    registry_limits.max_items = 1;
    registry_limits.max_feed_bytes = 64 * 1024;
    registry_limits.max_archive_bytes = 64 * 1024;
    registry_limits.max_page_archive_bytes = 128 * 1024;
    registry_limits.max_catalog_items = 8;
    registry_limits.connect_timeout = Duration::from_millis(500);
    registry_limits.read_timeout = Duration::from_secs(1);
    let rounds = match profile {
        Profile::Smoke => 3,
        Profile::Full => 15,
    };
    for callers in [1_usize, 8, 32] {
        let mut cold = Vec::with_capacity(rounds);
        let mut warm = Vec::with_capacity(rounds);
        let mut restart = Vec::with_capacity(rounds);
        let mut latest = None;
        let mut assertions = Vec::new();
        for round in 0..rounds {
            let result = run_network_round(
                callers,
                round,
                endpoint_seed.as_bytes(),
                &archive,
                registry_limits,
            )?;
            cold.push(result.cold_ns);
            warm.push(result.warm_ns);
            restart.push(result.restart_ns);
            assertions.push(format!(
                "round {}: {} callers, {} leader, {} followers, one feed/archive and {} response bytes",
                round + 1,
                result.callers,
                result.telemetry_leaders,
                result.telemetry_followers,
                result.fixture.response_bytes,
            ));
            latest = Some(result);
        }
        let latest = latest.ok_or("network acquisition produced no rounds")?;
        let identity = |result: &Arc<backend_engine::acquisition::RegistryAcquisitionResult>| {
            (
                root_hex(result.receipt.id.as_bytes()),
                root_hex(result.delta.id().as_bytes()),
                root_hex(result.receipt.target.as_bytes()),
                root_hex(result.artifact.version().as_ref()),
            )
        };
        let (receipt_id, delta_id, target_root, artifact_id) = identity(&latest.cold_result);
        let push_network =
            |measurements: &mut Vec<AcquisitionMeasurement>,
             operation: String,
             phase: String,
             timings: Vec<u128>,
             downloaded_bytes: usize,
             reused_bytes: usize,
             result: &Arc<backend_engine::acquisition::RegistryAcquisitionResult>| {
                measurements.push(AcquisitionMeasurement {
                    schema: JSON_SCHEMA,
                    status: "ok",
                    operation,
                    phase,
                    wall: stats(&mut timings.clone()),
                    calls: callers,
                    buffer_ceiling_bytes: registry_limits.max_archive_bytes,
                    downloaded_bytes,
                    reused_bytes,
                    memory_high_water_bytes: None,
                    receipt_id: Some(root_hex(result.receipt.id.as_bytes())),
                    delta_id: Some(root_hex(result.delta.id().as_bytes())),
                    target_root: Some(root_hex(result.receipt.target.as_bytes())),
                    artifact_id: Some(root_hex(result.artifact.version().as_ref())),
                    correctness: Correctness {
                        passed: true,
                        assertions: assertions.clone(),
                    },
                });
            };
        let rounds_bytes = rounds.saturating_mul(archive.len());
        push_network(
            &mut measurements,
            format!("network_singleflight_{callers}_calls"),
            "cold_loopback_network".to_owned(),
            cold,
            rounds_bytes,
            rounds_bytes.saturating_mul(callers.saturating_sub(1)),
            &latest.cold_result,
        );
        push_network(
            &mut measurements,
            format!("network_warm_cache_{callers}_calls"),
            "warm_cache".to_owned(),
            warm,
            0,
            rounds_bytes,
            &latest.warm_result,
        );
        push_network(
            &mut measurements,
            format!("network_restart_offline_{callers}_calls"),
            "warm_restart_offline".to_owned(),
            restart,
            0,
            rounds_bytes,
            &latest.restart_result,
        );
        debug_assert_eq!(
            (receipt_id, delta_id, target_root, artifact_id),
            identity(&latest.cold_result)
        );
    }
    let _ = fs::remove_dir_all(path);
    Ok(measurements)
}

fn run_gui(gui_bin: Option<&Path>) -> GuiMeasurement {
    let Some(gui_bin) = gui_bin.filter(|path| path.is_file()) else {
        return GuiMeasurement {
            schema: JSON_SCHEMA,
            status: "unavailable",
            cold: true,
            warm_navigation: None,
            model_to_first_semantic_frame: None,
            source_open_search: None,
            graph_incremental_delta: None,
            full_harness_wall: None,
            requested_viewport: "1440x1000@1".to_owned(),
            requested_state: "package".to_owned(),
            captured_captures: None,
            captured_frames: None,
            verified_frames: None,
            verified_artifacts: None,
            deterministic_seven_frame_capture: false,
            correctness: Correctness {
                passed: false,
                assertions: vec!["backend-desktop-gui-harness path was not supplied".to_owned()],
            },
        };
    };
    let output = temp_root("gui-output");
    let _ = fs::remove_dir_all(&output);
    let started = Instant::now();
    let status = Command::new(gui_bin)
        .args(["capture", "--output"])
        .arg(&output)
        .args(["--state", "package", "--viewport", "1440x1000@1"])
        .status();
    let elapsed = started.elapsed().as_nanos();
    let report_path = output.join("run-report.json");
    let parsed = fs::read_to_string(report_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let captured_frames = parsed
        .as_ref()
        .and_then(|value| value.get("captured_frames"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);
    let captured_captures = parsed
        .as_ref()
        .and_then(|value| value.get("captured_captures"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);
    let verified_frames = parsed
        .as_ref()
        .and_then(|value| value.get("verified_frames"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize);
    let (package_frame_count, verified_artifacts, artifact_failures) =
        verify_gui_package_artifacts(&output);
    let report_pass = parsed
        .as_ref()
        .and_then(|value| value.get("pass"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let failures = parsed
        .as_ref()
        .and_then(|value| value.get("failures"))
        .and_then(serde_json::Value::as_array)
        .map(|failures| {
            failures
                .iter()
                .filter_map(|failure| failure.get("error").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let deterministic_seven_frame_capture = package_frame_count == Some(7)
        && captured_frames == Some(7)
        && verified_frames == Some(7)
        && verified_artifacts == 7
        && captured_frames == verified_frames;
    let passed = status.is_ok_and(|value| value.success())
        && report_pass
        && failures.is_empty()
        && captured_captures == Some(1)
        && deterministic_seven_frame_capture;
    let mut assertions = if passed {
        vec![
            "live GUI harness completed".to_owned(),
            "package capture used exact 1440x1000@1 viewport".to_owned(),
            "captured and independently verified seven deterministic PNG frames".to_owned(),
        ]
    } else {
        vec![
            "live GUI package capture was unavailable or failed before a verified frame".to_owned(),
        ]
    };
    assertions.extend(
        failures
            .into_iter()
            .map(|failure| format!("live GUI harness failure: {failure}")),
    );
    assertions.extend(
        artifact_failures
            .into_iter()
            .map(|failure| format!("GUI artifact verification failure: {failure}")),
    );
    if parsed.is_some() && !report_pass {
        assertions.push("live GUI run-report pass=false".to_owned());
    }
    let _ = fs::remove_dir_all(output);
    GuiMeasurement {
        schema: JSON_SCHEMA,
        status: if passed { "ok" } else { "unavailable" },
        cold: true,
        warm_navigation: gui_phase_stats(parsed.as_ref(), "warm_navigation"),
        model_to_first_semantic_frame: gui_phase_stats(
            parsed.as_ref(),
            "model_to_first_semantic_frame",
        ),
        source_open_search: gui_phase_stats(parsed.as_ref(), "source_open_search"),
        graph_incremental_delta: gui_phase_stats(parsed.as_ref(), "graph_incremental_delta"),
        full_harness_wall: passed.then_some(stats(&mut vec![elapsed])),
        requested_viewport: "1440x1000@1".to_owned(),
        requested_state: "package".to_owned(),
        captured_captures,
        captured_frames,
        verified_frames,
        verified_artifacts: Some(verified_artifacts),
        deterministic_seven_frame_capture,
        correctness: Correctness { passed, assertions },
    }
}

fn parse_args() -> BenchResult<(Profile, bool, PathBuf, Option<PathBuf>)> {
    let mut profile = Profile::Smoke;
    let mut require_complete = false;
    let mut output = PathBuf::from("tests/performance/results/integrated-smoke.json");
    let mut gui_bin = None;
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                index += 1;
                profile = Profile::parse(args.get(index).ok_or("--profile needs a value")?)?;
            }
            "--output" => {
                index += 1;
                output = PathBuf::from(args.get(index).ok_or("--output needs a value")?);
            }
            "--gui-bin" => {
                index += 1;
                gui_bin = Some(PathBuf::from(
                    args.get(index).ok_or("--gui-bin needs a value")?,
                ));
            }
            "--require-complete" => {
                require_complete = true;
            }
            "-h" | "--help" => {
                println!(
                    "usage: integrated [--profile smoke|full] [--require-complete] [--output PATH] [--gui-bin PATH]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}").into()),
        }
        index += 1;
    }
    Ok((profile, require_complete, output, gui_bin))
}

fn main() -> BenchResult<()> {
    let (profile, explicit_require_complete, output, gui_bin) = parse_args()?;
    let require_complete = explicit_require_complete || matches!(profile, Profile::Full);
    let build = build_metadata();
    let (files, source_kind, source_root) = discover_files()?;
    if files.is_empty() {
        return Err("no real source files found in configured or vendored corpus".into());
    }
    let classes = corpus_classes(files);
    let corpora = classes
        .iter()
        .map(|class| CorpusDescription {
            name: class.name.to_owned(),
            source_kind: source_kind.clone(),
            root: source_root.clone(),
            files: class.files.len(),
            bytes: class.bytes(),
            languages: class
                .files
                .iter()
                .map(|file| file.language.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        })
        .collect::<Vec<_>>();
    let resource_sampler = ResourceSampler::start();
    let (ingest, deltas) = run_ingest(&classes, &source_kind, profile)?;
    let search = run_search(&classes, profile)?;
    let catalog = run_catalog(classes.last().ok_or("missing large corpus")?, profile)?;
    let surfaces = run_surfaces(classes.last().ok_or("missing large corpus")?, profile)?;
    let acquisition = run_acquisition(classes.last().ok_or("missing large corpus")?, profile)?;
    let gui = run_gui(gui_bin.as_deref());
    let (peak_rss_bytes, cpu_time_ns) = resource_sampler.finish();
    let mut notes = vec![
        "Host CPU time and peak RSS are sampled with ps; phase-level allocation counters and GUI child-process resources remain unavailable at the public Rust boundary.".to_owned(),
        "Network acquisition uses a bounded loopback HTTP fixture through the production RegistryOwner and records synchronized 1/8/32-call singleflight, warm-cache, and restart-cache rows; external-registry latency is not inferred from that fixture.".to_owned(),
        "GUI source-open/search and graph-delta timings are populated only from explicit phase timings emitted by the supplied harness; the full child-process wall is kept separate.".to_owned(),
        format!("large corpus selection is deterministic and capped at {} files or {} MiB after canonical row-size filtering.", MAX_LARGE_CORPUS_FILES, MAX_LARGE_CORPUS_BYTES / (1024 * 1024)),
        format!("tail percentiles are emitted only for rows with at least {MIN_TAIL_PERCENTILE_SAMPLES} samples; smaller rows carry null p95/p99 values and an insufficient-sample marker."),
    ];
    if !source_kind.starts_with("nix-configured") {
        notes.push(format!("Nix fleet corpus roots were not set; the runner used the checked-in workspace source tree, including the vendored multilingual fixtures, and retained only source files at or below the canonical {} KiB relation-row bound. The large class is capped at {} files or {} MiB for CI smoke duration.", MAX_RELATION_ROW_BYTES / 1024, MAX_LARGE_CORPUS_FILES, MAX_LARGE_CORPUS_BYTES / (1024 * 1024)));
    }
    let mut hardware = hardware();
    hardware.peak_rss_bytes = peak_rss_bytes;
    hardware.cpu_time_ns = cpu_time_ns;
    let gate = gate_summary(
        profile,
        require_complete,
        &build,
        &ingest,
        &deltas,
        &search,
        &catalog,
        &surfaces,
        &acquisition,
        &gui,
        &corpora,
    );
    let report = Report {
        schema: JSON_SCHEMA,
        version: 2,
        status: gate.status,
        profile: match profile {
            Profile::Smoke => "smoke",
            Profile::Full => "full",
        }
        .to_owned(),
        commit: commit(),
        hardware,
        toolchain: toolchain(),
        corpora,
        ingest,
        deltas,
        search,
        catalog,
        surfaces,
        acquisition,
        gui,
        build,
        gate,
        notes,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("wrote {}", output.display());
    println!(
        "commit={} corpus={} files={} bytes={}",
        report.commit,
        source_kind,
        report.corpora.last().map_or(0, |corpus| corpus.files),
        report.corpora.last().map_or(0, |corpus| corpus.bytes)
    );
    println!(
        "gate status={} complete={} promotion_ready={} unavailable={} failed={}",
        report.status,
        report.gate.complete,
        report.gate.promotion_ready,
        report.gate.unavailable.len(),
        report.gate.failed.len()
    );
    for item in &report.ingest {
        println!(
            "ingest size={} files={} bytes={} p50_ns={:?} p95_ns={:?} root={}",
            item.size_class,
            item.source_files,
            item.source_bytes,
            item.wall.p50_ns,
            item.wall.p95_ns,
            item.output_root
        );
    }
    for item in report.search.iter().filter(|item| item.readers == 1) {
        println!(
            "search size={} mode={} p50_ns={:?} p95_ns={:?} results={}",
            item.size_class, item.mode, item.warm.p50_ns, item.warm.p95_ns, item.result_count
        );
    }
    for item in &report.catalog {
        println!(
            "catalog op={} p50_ns={:?} rows={} db_bytes={}",
            item.operation, item.wall.p50_ns, item.rows, item.database_bytes
        );
    }
    if require_complete && !report.gate.promotion_ready {
        return Err(format!(
            "benchmark promotion gate failed: {}",
            report.gate.reasons.join("; ")
        )
        .into());
    }
    Ok(())
}
