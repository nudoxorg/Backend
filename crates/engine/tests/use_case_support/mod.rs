//! Shared meter for cross-language use-case flows.
//!
//! Token estimates use the same ceiling as `backend_present::estimate_tokens`:
//! one token per four UTF-8 bytes, rounded up. `digest_ns` is the time to
//! build the sorted name digest that an index would ingest. It is not a
//! durable index-segment build; that path needs a reopened publication.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use backend_semantic::ir::{EntityKind, Ir};
use serde::Serialize;

/// Bytes per token. Must match `backend_present::ESTIMATED_BYTES_PER_TOKEN`.
pub const ESTIMATED_BYTES_PER_TOKEN: usize = 4;

/// One finished use-case measurement.
#[derive(Debug, Serialize)]
pub struct UseCaseReport {
    /// Language label, for example `rust` or `typescript`.
    pub language: &'static str,
    /// Stable use-case name.
    pub use_case: &'static str,
    /// Wall time of the authority compile, in nanoseconds.
    pub compile_ns: u128,
    /// Declared entities in the lowered image.
    pub entities: usize,
    /// Occurrence sites in the lowered image.
    pub occurrences: usize,
    /// Graph links in the lowered image.
    pub links: usize,
    /// UTF-8 bytes of the sorted `kind name` digest.
    pub digest_bytes: usize,
    /// Ceiling token estimate of that digest.
    pub estimated_tokens: usize,
    /// Time to build the digest, in nanoseconds.
    pub digest_ns: u128,
    /// Empty when the flow ran. A toolchain gap names the missing tool.
    pub skipped: &'static str,
}

/// Clock for one compile.
pub struct CompileTimer {
    start: Instant,
}

impl CompileTimer {
    /// Starts the compile clock.
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Elapsed time since [`CompileTimer::start`].
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Ceiling token count for `bytes`, matching the present-budget gate.
pub const fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(ESTIMATED_BYTES_PER_TOKEN - 1) / ESTIMATED_BYTES_PER_TOKEN
}

/// Sorted `kind\tname` lines for every entity. This is the index input digest.
pub fn name_digest(ir: &Ir) -> String {
    let mut lines = Vec::new();
    for item in ir.items() {
        let name = String::from_utf8_lossy(item.name()).into_owned();
        lines.push(format!("{:?}\t{name}", item.kind()));
    }
    lines.sort();
    lines.join("\n")
}

/// Measures the digest and writes one JSON report under `target/use-case-bench/`.
pub fn finish(
    language: &'static str,
    use_case: &'static str,
    compile: Duration,
    ir: &Ir,
) -> io::Result<UseCaseReport> {
    let digest_start = Instant::now();
    let digest = name_digest(ir);
    let digest_ns = digest_start.elapsed().as_nanos();
    let digest_bytes = digest.len();
    let report = UseCaseReport {
        language,
        use_case,
        compile_ns: compile.as_nanos(),
        entities: ir.items().count(),
        occurrences: ir.link_occurrences().len(),
        links: ir.items().map(|item| item.links_from().len()).sum(),
        digest_bytes,
        estimated_tokens: estimate_tokens(digest_bytes),
        digest_ns,
        skipped: "",
    };
    write_report(&report)?;
    Ok(report)
}

/// Records a flow that could not run because a required toolchain is absent.
pub fn skip(
    language: &'static str,
    use_case: &'static str,
    reason: &'static str,
) -> io::Result<UseCaseReport> {
    let report = UseCaseReport {
        language,
        use_case,
        compile_ns: 0,
        entities: 0,
        occurrences: 0,
        links: 0,
        digest_bytes: 0,
        estimated_tokens: 0,
        digest_ns: 0,
        skipped: reason,
    };
    write_report(&report)?;
    Ok(report)
}

fn write_report(report: &UseCaseReport) -> io::Result<()> {
    let directory = bench_dir();
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}-{}.json", report.language, report.use_case));
    let body = serde_json::to_vec_pretty(report)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(path, body)
}

fn bench_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/use-case-bench")
}

/// Entity count for one kind and exact name. Tests use this instead of `is_ok`.
pub fn count_named(ir: &Ir, kind: EntityKind, name: &[u8]) -> usize {
    ir.items()
        .filter(|item| item.kind() == kind && item.name() == name)
        .count()
}
