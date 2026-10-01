//! Source-bound audit of the frozen real-package inventory.
//!
//! The generated 210-row matrix exercises shape-specific expectations.  This
//! module is the separate package lane: it resolves only the exact committed
//! coordinates, asks the selected authority for each source when a producer
//! exists, and then runs the same fused compile/publication boundary.  A
//! package is never counted as verified merely because a source tree or a
//! native executable happened to be present.

use super::authority::{go_fixture_for_real_source, rust_fixture_for_source};
use super::comparison::RealAuditField;
use super::execution::{compile_with_authority_until, terminal_kind};
use super::observation::{
    digest_recipe, digest_semantic, digest_source, digest_typed, digest_u64,
    observe_owned_semantic, observe_reopened_semantic,
};
use super::*;

#[path = "real_authority.rs"]
mod real_authority;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RealLaneSummary {
    pub(super) attempted: usize,
    pub(super) source_bound: usize,
    pub(super) output: usize,
    pub(super) verified: usize,
    pub(super) unavailable: usize,
    /// Rows that compiled but carried at least one not-compared observer
    /// plane: the lane's authority cannot ground every plane. Counted
    /// separately from source/toolchain absence and never treated as parity.
    pub(super) not_compared: usize,
    pub(super) terminals: usize,
    pub(super) mismatches: usize,
}

impl RealLaneSummary {
    const ZERO: Self = Self {
        attempted: 0,
        source_bound: 0,
        output: 0,
        verified: 0,
        unavailable: 0,
        not_compared: 0,
        terminals: 0,
        mismatches: 0,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RealAuditSummary {
    pub(super) lanes: [RealLaneSummary; CorpusLanguage::ALL.len()],
    /// Rows this run was required to account for. Equal to the full selection
    /// on a production run; smaller only when the development-only
    /// `NUDOX_AUDIT_FILTER` narrows the run to specific coordinates.
    pub(super) expected: usize,
    pub(super) capacity: inventory::CorpusCapacityVerdict,
    /// Source/authority absence terminals. These are counted, typed outcomes:
    /// a package that is not provisioned on this host is never a silent skip
    /// and never a parity mismatch.
    pub(super) unavailable: Box<[CorpusMismatch]>,
    pub(super) mismatches: Box<[CorpusMismatch]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceAuthorityFacts {
    /// The lane's authority grounds exact declaration name spans, and the
    /// lane's IR attaches source spans: the span plane is verified by the
    /// authority declaration span join on both readers.
    Spans,
    /// The producer is valid but no span-grounded authority projection exists
    /// for this lane: a typed not-compared outcome.
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceExpectation {
    source: SourceIdentity,
    /// The package-relative source path the scope entered; the byte value of
    /// every IR span's file atom on this lane.
    file: Box<[u8]>,
    profile: LanguageProfile,
    tool: NativeTool,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    spans: SourceAuthorityFacts,
    authority: real_authority::AuthorityExpectation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RealCaseDisposition {
    Output {
        mismatches: usize,
        /// Counted observer-plane terminals produced by this compiled row.
        /// They are deliberately separate from `mismatches`: a deferred
        /// observer plane is a typed Unavailable outcome, never a parity
        /// disagreement.
        unavailable: usize,
        /// Counted not-compared observer planes: planes this lane's authority
        /// cannot ground. Verified excludes them without calling them parity.
        not_compared: usize,
        verified: bool,
    },
    Unavailable(AuthorityUnavailableCause),
    Terminal(CompileTerminalKind),
}

/// The TypeScript checker's report/collect budgets for real-package rows.
///
/// The authority for these rows is the TSZ checker: the vendored
/// `NUDOX_TYPESCRIPT_CHECKER_BIN` transaction driven through
/// `backend_frontend_typescript::legacy::Checker`. Its vendored defaults
/// (16 MiB per child stream, 60 s collect) are sized for generated single
/// sources; real packages legitimately exceed both, in opposite directions:
///
/// - `@types/react@19.2.18`'s report *is* the authority payload itself, and
///   a declaration-dense `.d.ts` tree emits ~16.8 MiB of JSON, so the report
///   budget is 64 MiB — four times the default, still firmly bounded. This
///   raise is what turns that row's former `OutputLimit` refusal into a
///   compiled row.
/// - The `@types/jest` and `typescript` checker children need more than 60 s
///   wall clock to collect over large ambient-type trees; 120 s stays within
///   the audit's own per-row authority deadline
///   ([`REAL_ROW_AUTHORITY_DEADLINE`]), so no checker child waits longer
///   than the rest of the pipeline allows.
///
/// The staging file budget (`PackageFileLimit`) is owned by the frontend
/// adapter (`frontends/typescript/src/legacy/checker.rs`), which raised it
/// from 4096 to 8192 files for the same reason: `es-toolkit@1.52.0` (4255
/// files) and `date-fns@4.1.0` (5326 files) are legitimately-large utility
/// packages whose staging is already capped by the 64 MiB byte budget.
///
/// Two rows keep the typed `Unavailable` disposition on purpose:
/// `@types/jest@29.5.14` and `typescript@5.7.2` (`lib.dom.d.ts`) crash the
/// checker child with a V8 heap OOM (`SIGABRT`, "JavaScript heap out of
/// memory") — measured above 8 GiB of old space and 190 s of wall clock each,
/// with no report produced at any budget this host can defend. A bounded
/// authority that cannot check a tree within any raiseable bound is a
/// counted per-row absence, never a fabricated success or a harness crash.
pub(super) const TS_REPORT_OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
pub(super) const TS_REPORT_DEADLINE: Duration = Duration::from_secs(120);

/// Per-row authority deadline for the real-package lane.
///
/// The entry module's shared 120 s `DEADLINE` is sized for the synthetic
/// matrix's single-source rows and is measurably too tight for the largest
/// real rows. The filtered `Java:h2@2.2.224` row measured 157.96 s end to end
/// on this host (2026-09-19, JDK 21, fleet corpus generation
/// `gmv06x4wwq9h7gznm5zmsfi26dg6f07j`): its whole-package authority
/// extraction (javac over the full h2 closure plus the fleet source path)
/// and harness setup consume ≈38 s, and the bounded authority compile phase
/// exhausted the entire 120 s budget and died `DeadlineExceeded` before the
/// row's comparison planes ever started. [`REAL_ROW_AUTHORITY_DEADLINE`] is
/// 300 s: 2.3× the measured ≥120 s requirement and 1.9× the row's whole
/// measured wall clock, far inside the unchanged 15-minute
/// [`ROW_WORKER_HARD_CAP`]. With the raise the h2 authority phase completes
/// (a stack sample taken 13:47 into the re-driven row already sat in
/// `check_real_output`, downstream of the bounded compile) — the row then
/// spends its remaining wall clock inside the audit's own unbudgeted
/// authority-plane comparison over h2's whole-package image and ends as a
/// typed `RowProcessCrash` at the unchanged [`ROW_WORKER_HARD_CAP`]. That
/// comparison-plane cost is audit machinery outside this constant's scope;
/// the deadline raise is necessary for `Java:h2` to progress past its former
/// `DeadlineExceeded` terminal but not sufficient for it to reach output on
/// this (debug-profile) host. The synthetic matrix keeps the 120 s constant:
/// its rows render one generated source each and have never approached it.
pub(super) const REAL_ROW_AUTHORITY_DEADLINE: Duration = Duration::from_secs(300);

/// Final accounting decision for a real-package audit.
///
/// Source, toolchain, and deferred observer-plane absence are counted in the
/// typed `unavailable` terminal collection and can never be mistaken for a
/// parity disagreement. Only a genuine source-vs-output disagreement
/// (`mismatches`) is fatal, together with a shortfall in attempted capacity.
pub(super) fn real_audit_verdict(audit: &RealAuditSummary) -> Result<(), CorpusAuditError> {
    let accounted: usize = audit.lanes.iter().map(|lane| lane.attempted).sum();
    if accounted != audit.expected {
        return Err(CorpusAuditError::Inventory {
            cause: InventoryInvariant::TotalCount {
                observed: accounted,
                expected: audit.expected,
            },
        });
    }
    if let Some(first) = audit.mismatches.first().cloned() {
        return Err(CorpusAuditError::Mismatches {
            count: audit.mismatches.len(),
            first: Some(first),
        });
    }
    if let CorpusCapacityVerdict::Insufficient { observed, required } = audit.capacity {
        return Err(CorpusAuditError::Inventory {
            cause: InventoryInvariant::CorpusCapacity { observed, required },
        });
    }
    Ok(())
}

/// Runs all locally source-bound rows in inventory order.
///
/// Every source-bearing row is driven in its own dedicated worker process
/// ([`real_audit_row_worker`], re-executing this test binary with
/// `NUDOX_AUDIT_ROW=<ordinal>`). The audit previously drove all rows
/// in-process and died as a whole-run `SIGSEGV` inside the in-process
/// libclang FFI: one native fault erased the entire run's summary. The worker
/// process boundary (spawn + `waitpid`) turns such a fault into a typed
/// [`AuthorityUnavailableCause::RowProcessCrash`] disposition for exactly
/// that row — counted, dumped, never silently dropped — while every prior
/// row's result is preserved and the per-language summary is always emitted.
/// Each worker re-resolves, re-stages, compiles, publishes, and reopens its
/// one row exactly as the in-process drive did, so no row's proof is
/// weakened.
pub(super) fn audit_real_inventory(
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
) -> Result<RealAuditSummary, CorpusAuditError> {
    // Row bodies run in workers which build their own tools; these caller
    // arguments remain part of the audit seam and are unused in the parent.
    let _ = (hosts, resolved, authorities);
    inventory::validate_real_inventory().map_err(|cause| CorpusAuditError::Inventory { cause })?;
    let mut lanes = [RealLaneSummary::ZERO; CorpusLanguage::ALL.len()];
    let mut unavailable = Vec::new();
    let mut mismatches = Vec::new();
    let row_artifacts =
        FixtureDir::new("real-row-workers").map_err(|source| CorpusAuditError::Io {
            phase: AuditIoPhase::Fixture,
            source,
        })?;

    // Development-only narrowing. Production runs leave `NUDOX_AUDIT_FILTER`
    // unset and account for the whole selection; when it is set, only matching
    // rows are driven so a single package can be reproduced without the
    // multi-minute full sweep. The filter never changes what the full audit
    // proves, because `expected` tracks exactly the rows it admitted.
    let filter = inventory::audit_filter();
    let mut expected = 0_usize;
    for input in inventory::real_package_inputs() {
        let case = match input {
            inventory::RealPackageInput::Source { case, .. }
            | inventory::RealPackageInput::Unavailable { case, .. } => case,
        };
        if let Some(filter) = filter.as_deref()
            && !inventory::audit_filter_matches(filter, &case)
        {
            continue;
        }
        expected = expected.saturating_add(1);
        let lane = &mut lanes[inventory_language_index(case.language)];
        lane.attempted = lane.attempted.saturating_add(1);
        match input {
            inventory::RealPackageInput::Unavailable { cause, .. } => {
                lane.unavailable = lane.unavailable.saturating_add(1);
                unavailable.push(CorpusMismatch::RealUnavailable {
                    case,
                    field: RealAuditField::Source,
                    cause: AuthorityUnavailableCause::Source(cause),
                });
            }
            inventory::RealPackageInput::Source { source, .. } => {
                lane.source_bound = lane.source_bound.saturating_add(1);
                let disposition = drive_row_in_worker(
                    case,
                    &source,
                    &row_artifacts,
                    &mut unavailable,
                    &mut mismatches,
                )?;
                match disposition {
                    RealCaseDisposition::Output {
                        mismatches: count,
                        unavailable: observer_unavailable,
                        not_compared,
                        verified,
                    } => {
                        lane.output = lane.output.saturating_add(1);
                        lane.mismatches = lane.mismatches.saturating_add(count);
                        if observer_unavailable > 0 {
                            lane.unavailable = lane.unavailable.saturating_add(1);
                        }
                        if not_compared > 0 {
                            lane.not_compared = lane.not_compared.saturating_add(1);
                        }
                        lane.verified += usize::from(verified);
                    }
                    RealCaseDisposition::Unavailable(cause) => {
                        lane.unavailable = lane.unavailable.saturating_add(1);
                        let field = match &cause {
                            // A source-absent row re-driven by a worker keeps
                            // the source field of the parent's own
                            // classification; every authority-side refusal
                            // remains a toolchain field.
                            AuthorityUnavailableCause::Source(_) => RealAuditField::Source,
                            _ => RealAuditField::Toolchain,
                        };
                        unavailable.push(CorpusMismatch::RealUnavailable { case, field, cause });
                    }
                    RealCaseDisposition::Terminal(terminal) => {
                        lane.terminals = lane.terminals.saturating_add(1);
                        lane.mismatches = lane.mismatches.saturating_add(1);
                        mismatches.push(CorpusMismatch::RealTerminal {
                            case,
                            source: source.identity,
                            terminal,
                        });
                    }
                }
            }
        }
    }
    Ok(RealAuditSummary {
        lanes,
        expected,
        capacity: if filter.is_some()
            || lanes.iter().map(|lane| lane.attempted).sum::<usize>()
                >= inventory::MIN_REAL_PACKAGE_COUNT
        {
            inventory::CorpusCapacityVerdict::MeetsMinimum
        } else {
            inventory::CorpusCapacityVerdict::Insufficient {
                observed: lanes.iter().map(|lane| lane.attempted).sum(),
                required: inventory::MIN_REAL_PACKAGE_COUNT,
            }
        },
        unavailable: unavailable.into_boxed_slice(),
        mismatches: mismatches.into_boxed_slice(),
    })
}

fn inventory_language_index(language: CorpusLanguage) -> usize {
    match language {
        CorpusLanguage::Rust => 0,
        CorpusLanguage::TypeScript => 1,
        CorpusLanguage::Python => 2,
        CorpusLanguage::Go => 3,
        CorpusLanguage::Java => 4,
        CorpusLanguage::CSharp => 5,
        CorpusLanguage::Clang => 6,
    }
}

/// Full test name of the row worker, so the parent re-executes this exact
/// binary against exactly that test. libtest names tests by their module
/// path below the crate, so the crate prefix from `module_path!` is stripped.
fn row_worker_test_name() -> String {
    let qualified = concat!(module_path!(), "::real_audit_row_worker");
    qualified
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(qualified)
        .to_owned()
}

/// Hard wall-clock cap for one row worker. Workers keep their own authority
/// deadlines; this cap only bounds a wedged native call that no inner deadline
/// can interrupt (the libclang FFI being the one that crashed the audit).
/// A capped worker is a typed [`AuthorityUnavailableCause::RowProcessCrash`],
/// never a hang.
const ROW_WORKER_HARD_CAP: Duration = Duration::from_secs(15 * 60);

/// Stack budget for the row worker's drive thread.
///
/// The semantic engine's compile and observation recursion is proportional to
/// declaration nesting, and the largest real sources overflow libtest's
/// default 8 MiB test-thread stack (`@types/react`'s selected `.d.ts` did
/// exactly that). The worker therefore drives its row on a dedicated thread
/// with this documented budget instead. A stack overflow that even this
/// budget cannot absorb aborts the worker process and is isolated as a typed
/// [`AuthorityUnavailableCause::RowProcessCrash`] row, never a lost run.
const ROW_WORKER_STACK_BYTES: usize = 512 * 1024 * 1024;

/// Worker → parent telemetry. The worker writes one bounded outcome file and
/// mirrors every diagnostic into the parent's output, so a refusal keeps its
/// authority stderr (oracle exit text, checker crash transcript) exactly as
/// the old in-process `real-setup`/`real-terminal` lines did.
#[derive(Default)]
struct RowTelemetry {
    diags: Vec<String>,
}

impl RowTelemetry {
    fn diag(&mut self, text: String) {
        eprintln!("{text}");
        self.diags.push(text);
    }
}

/// Escapes one telemetry or reason string onto a single outcome-file line.
fn escape_line(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// Inverse of [`escape_line`].
fn unescape_line(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            unescaped.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => unescaped.push('\n'),
            Some('r') => unescaped.push('\r'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    unescaped
}

/// Drives one source-bearing row in its dedicated worker process and applies
/// the worker's typed outcome to the audit's sinks. A worker that dies by
/// signal, exits without a typed outcome, or hits the hard cap becomes a
/// [`AuthorityUnavailableCause::RowProcessCrash`] row — never an aborted run.
///
/// A worker the harness could not run at all (no row log, no spawn, no wait)
/// is not a row outcome: it is an audit I/O fault and ends the run red. It
/// used to be filed as a `RowProcessCrash { signal: -1, code: -1 }` row, which
/// the verdict counts as unavailable, so a full disk turned every remaining
/// row into a passing crash and the audit could go green having run nothing.
fn drive_row_in_worker(
    case: inventory::RealPackageCase,
    source: &inventory::ResolvedPackageSource,
    artifacts: &FixtureDir,
    unavailable_sink: &mut Vec<CorpusMismatch>,
    mismatch_sink: &mut Vec<CorpusMismatch>,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    drive_row_in_worker_with_env(
        case,
        source,
        artifacts,
        unavailable_sink,
        mismatch_sink,
        &[],
    )
}

/// [`drive_row_in_worker`] with extra child environment entries. The audit
/// passes none; tests inject fault hooks through this seam.
fn drive_row_in_worker_with_env(
    case: inventory::RealPackageCase,
    source: &inventory::ResolvedPackageSource,
    artifacts: &FixtureDir,
    unavailable_sink: &mut Vec<CorpusMismatch>,
    mismatch_sink: &mut Vec<CorpusMismatch>,
    extra_env: &[(&str, &str)],
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let outcome_path = artifacts.child(&format!("row-{}.outcome", case.ordinal));
    let log_path = artifacts.child(&format!("row-{}.log", case.ordinal));
    let _ = fs::remove_file(&outcome_path);
    let status =
        spawn_row_worker(case, &outcome_path, &log_path, extra_env).map_err(|source| {
            CorpusAuditError::Io {
                phase: AuditIoPhase::Fixture,
                source,
            }
        })?;
    let outcome = fs::read_to_string(&outcome_path)
        .ok()
        .and_then(|text| parse_row_outcome(&text, case, unavailable_sink, mismatch_sink));
    Ok(match outcome {
        Some(disposition) => disposition,
        None => {
            let cause = row_crash_cause(&status);
            let log_tail = fs::read(&log_path)
                .map(|bytes| {
                    let start = bytes.len().saturating_sub(2048);
                    String::from_utf8_lossy(&bytes[start..]).into_owned()
                })
                .unwrap_or_default();
            eprintln!(
                "real-row-crash lang={:?} ordinal={} coord={:?} cause={cause:?} log_tail={log_tail:?}",
                case.language,
                case.ordinal,
                case.coordinate.raw(),
            );
            unavailable_sink.push(CorpusMismatch::RealUnavailable {
                case,
                field: RealAuditField::Source,
                cause,
            });
            RealCaseDisposition::Unavailable(cause)
        }
    })
}

/// Spawns one row worker (`current_exe` re-invoked against the worker test)
/// and waits for it. Harness faults (no executable, no row log, no spawn, no
/// wait) are errors; the hard cap kills the worker and reports the resulting
/// signal status.
fn spawn_row_worker(
    case: inventory::RealPackageCase,
    outcome_path: &Path,
    log_path: &Path,
    extra_env: &[(&str, &str)],
) -> io::Result<ExitStatus> {
    spawn_row_worker_with_cap(case, outcome_path, log_path, extra_env, ROW_WORKER_HARD_CAP)
}

/// [`spawn_row_worker`] with an explicit cap, the seam the cap-fault test
/// drives with a tiny deadline.
fn spawn_row_worker_with_cap(
    case: inventory::RealPackageCase,
    outcome_path: &Path,
    log_path: &Path,
    extra_env: &[(&str, &str)],
    cap: Duration,
) -> io::Result<ExitStatus> {
    let row_fault = |what: &str, error: io::Error| {
        io::Error::new(
            error.kind(),
            format!(
                "row worker ordinal={} coord={:?}: {what}: {error}",
                case.ordinal,
                case.coordinate.raw()
            ),
        )
    };
    let executable =
        std::env::current_exe().map_err(|error| row_fault("resolve the test binary", error))?;
    let mut command = Command::new(executable);
    command.args([row_worker_test_name().as_str(), "--exact", "--nocapture"]);
    command.env("NUDOX_AUDIT_ROW", case.ordinal.to_string());
    command.env("NUDOX_ROW_OUTCOME_FILE", outcome_path);
    // The worker addresses its row by ordinal; a filter inherited from the
    // parent would only invite double-narrowing.
    command.env_remove("NUDOX_AUDIT_FILTER");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    // The worker leads its own process group so the cap can reap its whole
    // supervised tree. A row worker's native grandchildren (the vendored
    // oracle/checker children) are torn down by the worker itself on every
    // orderly path, but a SIGKILL to the worker alone would re-parent them
    // and leak; killing the group reaps them with it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let log = fs::File::create(log_path).map_err(|error| row_fault("create its row log", error))?;
    let log_duplicate = log
        .try_clone()
        .map_err(|error| row_fault("share its row log", error))?;
    command.stdout(log).stderr(log_duplicate);
    let mut child = command
        .spawn()
        .map_err(|error| row_fault("spawn the worker", error))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < cap => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                eprintln!(
                    "real-row-spawn ordinal={} coord={:?} exceeded the {cap:?} hard cap; reaping",
                    case.ordinal,
                    case.coordinate.raw()
                );
                return reap_row_worker_tree(&mut child)
                    .map_err(|error| row_fault("reap the capped worker", error));
            }
            Err(error) => {
                let _ = reap_row_worker_tree(&mut child);
                return Err(row_fault("wait for the worker", error));
            }
        }
    }
}

/// SIGKILLs the worker's whole process group — the worker and every native
/// grandchild it supervises — then waits for the worker itself. The direct
/// child alone is only a fallback for hosts without process-group support;
/// killing the group is what keeps a capped worker's supervised native
/// children (oracle/checker processes) from being re-parented and leaking.
fn reap_row_worker_tree(child: &mut std::process::Child) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        let killed_group = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|group| {
                rustix::process::kill_process_group(group, rustix::process::Signal::KILL).is_ok()
            });
        if !killed_group {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    child.wait()
}

/// The typed disposition for a worker that produced no outcome file record.
fn row_crash_cause(status: &ExitStatus) -> AuthorityUnavailableCause {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(status).unwrap_or(0);
    #[cfg(not(unix))]
    let signal = 0;
    AuthorityUnavailableCause::RowProcessCrash {
        signal,
        code: status.code().unwrap_or(-1),
    }
}

/// Writes a typed `internal` outcome record for a worker whose drive failed
/// before it could emit a real disposition, and returns the error the worker
/// test should report.
fn write_internal_outcome(outcome_path: &Path, reason: &str) -> CorpusAuditError {
    let mut text = format!("outcome internal reason={}\n", escape_line(reason));
    text.push('\n');
    let _ = fs::write(outcome_path, text.as_bytes());
    CorpusAuditError::Io {
        phase: AuditIoPhase::Fixture,
        source: io::Error::other(reason.to_owned()),
    }
}

/// Parses one worker outcome file: `diag` records are reprinted verbatim,
/// `mismatch`/`unavailable` records refill the audit sinks, and the final
/// `outcome` record is the row's typed disposition. `None` means the file
/// carried no outcome record, which the caller treats as a process crash.
fn parse_row_outcome(
    text: &str,
    case: inventory::RealPackageCase,
    unavailable_sink: &mut Vec<CorpusMismatch>,
    mismatch_sink: &mut Vec<CorpusMismatch>,
) -> Option<RealCaseDisposition> {
    let mut disposition = None;
    for line in text.lines() {
        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        match tag {
            "diag" => eprintln!(
                "real-worker-diag lang={:?} ordinal={} coord={:?} {}",
                case.language,
                case.ordinal,
                case.coordinate.raw(),
                unescape_line(rest),
            ),
            "mismatch" => {
                let Some(entry) = parse_mismatch_record(rest, case) else {
                    continue;
                };
                mismatch_sink.push(entry);
            }
            "unavailable" => {
                let Some(entry) = parse_unavailable_record(rest, case) else {
                    continue;
                };
                unavailable_sink.push(entry);
            }
            "outcome" => {
                let Some((kind, fields)) = rest.split_once(' ') else {
                    continue;
                };
                disposition = match kind {
                    "output" => parse_output_outcome(fields),
                    "unavailable" => rest
                        .strip_prefix("unavailable ")
                        .and_then(|fields| fields.strip_prefix("cause="))
                        .and_then(decode_cause)
                        .map(RealCaseDisposition::Unavailable),
                    "terminal" => fields
                        .split_whitespace()
                        .find_map(|entry| entry.strip_prefix("kind="))
                        .and_then(decode_terminal_kind)
                        .map(RealCaseDisposition::Terminal),
                    "internal" => {
                        eprintln!(
                            "real-worker-internal lang={:?} ordinal={} coord={:?} {}",
                            case.language,
                            case.ordinal,
                            case.coordinate.raw(),
                            fields
                                .split_once("reason=")
                                .map(|(_, reason)| unescape_line(reason))
                                .unwrap_or_default(),
                        );
                        Some(RealCaseDisposition::Unavailable(
                            AuthorityUnavailableCause::RowProcessCrash {
                                signal: 0,
                                code: 101,
                            },
                        ))
                    }
                    _ => None,
                };
            }
            _ => {}
        }
    }
    disposition
}

fn parse_output_outcome(fields: &str) -> Option<RealCaseDisposition> {
    let mut verified = false;
    let mut mismatches = None;
    let mut unavailable = None;
    let mut not_compared = 0_usize;
    for field in fields.split_whitespace() {
        if let Some(value) = field.strip_prefix("verified=") {
            verified = value == "1";
        } else if let Some(value) = field.strip_prefix("mismatches=") {
            mismatches = value.parse::<usize>().ok();
        } else if let Some(value) = field.strip_prefix("unavailable=") {
            unavailable = value.parse::<usize>().ok();
        } else if let Some(value) = field.strip_prefix("not_compared=") {
            not_compared = value.parse::<usize>().unwrap_or(0);
        }
    }
    Some(RealCaseDisposition::Output {
        mismatches: mismatches?,
        unavailable: unavailable?,
        not_compared,
        verified,
    })
}

fn parse_mismatch_record(fields: &str, case: inventory::RealPackageCase) -> Option<CorpusMismatch> {
    let fields = fields.strip_prefix("real ")?;
    let mut field = None;
    let mut expected = None;
    let mut observed = None;
    for entry in fields.split_whitespace() {
        if let Some(value) = entry.strip_prefix("field=") {
            field = decode_real_field(value);
        } else if let Some(value) = entry.strip_prefix("expected=") {
            expected = decode_digest(value);
        } else if let Some(value) = entry.strip_prefix("observed=") {
            observed = decode_digest(value);
        }
    }
    Some(CorpusMismatch::Real {
        case,
        field: field?,
        expected: expected?,
        observed: observed?,
    })
}

fn parse_unavailable_record(
    fields: &str,
    case: inventory::RealPackageCase,
) -> Option<CorpusMismatch> {
    let mut field = None;
    let mut cause = None;
    for entry in fields.split_whitespace() {
        if let Some(value) = entry.strip_prefix("field=") {
            field = decode_real_field(value);
        } else if let Some(value) = entry.strip_prefix("cause=") {
            cause = decode_cause(value);
        }
    }
    Some(CorpusMismatch::RealUnavailable {
        case,
        field: field?,
        cause: cause?,
    })
}

fn decode_digest(text: &str) -> Option<Digest> {
    let digest_len = core::mem::size_of::<Digest>();
    if text.len() != digest_len * 2 {
        return None;
    }
    let mut digest = [0_u8; core::mem::size_of::<Digest>()];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(digest)
}

fn encode_digest(digest: &Digest) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The canonical one-line field spelling, shared with the audit parent's
/// bounded mismatch-detail telemetry.
pub(super) fn encode_real_field(field: RealAuditField) -> &'static str {
    match field {
        RealAuditField::Source => "source",
        RealAuditField::Profile => "profile",
        RealAuditField::Recipe => "recipe",
        RealAuditField::Toolchain => "toolchain",
        RealAuditField::SourceSpans => "source-spans",
        RealAuditField::Declarations => "declarations",
        RealAuditField::Types => "types",
        RealAuditField::Relations => "relations",
        RealAuditField::Documentation => "documentation",
        RealAuditField::Extensions => "extensions",
        RealAuditField::Discovery => "discovery",
        RealAuditField::CanonicalType => "canonical-type",
        RealAuditField::SemanticImage => "semantic-image",
        RealAuditField::Reopened => "reopened",
        RealAuditField::Render => "render",
    }
}

fn decode_real_field(text: &str) -> Option<RealAuditField> {
    let field = match text {
        "source" => RealAuditField::Source,
        "profile" => RealAuditField::Profile,
        "recipe" => RealAuditField::Recipe,
        "toolchain" => RealAuditField::Toolchain,
        "source-spans" => RealAuditField::SourceSpans,
        "declarations" => RealAuditField::Declarations,
        "types" => RealAuditField::Types,
        "relations" => RealAuditField::Relations,
        "documentation" => RealAuditField::Documentation,
        "extensions" => RealAuditField::Extensions,
        "discovery" => RealAuditField::Discovery,
        "canonical-type" => RealAuditField::CanonicalType,
        "semantic-image" => RealAuditField::SemanticImage,
        "reopened" => RealAuditField::Reopened,
        "render" => RealAuditField::Render,
        _ => return None,
    };
    Some(field)
}

fn encode_source_kind(kind: SourceUnavailableKind) -> &'static str {
    match kind {
        SourceUnavailableKind::RootUnset => "root-unset",
        SourceUnavailableKind::PackageDirectoryMissing => "package-directory-missing",
        SourceUnavailableKind::SourceFileMissing => "source-file-missing",
        SourceUnavailableKind::RepositoryFixtureMissing => "repository-fixture-missing",
        SourceUnavailableKind::ReadFailure => "read-failure",
        SourceUnavailableKind::SourceTooLarge => "source-too-large",
        SourceUnavailableKind::SourceTreeTooDeep => "source-tree-too-deep",
        SourceUnavailableKind::SourceDirectoryEntryCountExceeded => {
            "source-directory-entry-count-exceeded"
        }
        SourceUnavailableKind::SourceFileCountExceeded => "source-file-count-exceeded",
    }
}

fn decode_source_kind(text: &str) -> Option<SourceUnavailableKind> {
    let kind = match text {
        "root-unset" => SourceUnavailableKind::RootUnset,
        "package-directory-missing" => SourceUnavailableKind::PackageDirectoryMissing,
        "source-file-missing" => SourceUnavailableKind::SourceFileMissing,
        "repository-fixture-missing" => SourceUnavailableKind::RepositoryFixtureMissing,
        "read-failure" => SourceUnavailableKind::ReadFailure,
        "source-too-large" => SourceUnavailableKind::SourceTooLarge,
        "source-tree-too-deep" => SourceUnavailableKind::SourceTreeTooDeep,
        "source-directory-entry-count-exceeded" => {
            SourceUnavailableKind::SourceDirectoryEntryCountExceeded
        }
        "source-file-count-exceeded" => SourceUnavailableKind::SourceFileCountExceeded,
        _ => return None,
    };
    Some(kind)
}

fn encode_native_tool(tool: NativeTool) -> &'static str {
    match tool {
        NativeTool::Rustc => "rustc",
        NativeTool::TypeScriptCompiler => "typescript-compiler",
        NativeTool::Python => "python",
        NativeTool::GoCompiler => "go-compiler",
        NativeTool::JavaCompiler => "java-compiler",
        NativeTool::CSharpCompiler => "csharp-compiler",
        NativeTool::Clang => "clang",
    }
}

fn decode_native_tool(text: &str) -> Option<NativeTool> {
    let tool = match text {
        "rustc" => NativeTool::Rustc,
        "typescript-compiler" => NativeTool::TypeScriptCompiler,
        "python" => NativeTool::Python,
        "go-compiler" => NativeTool::GoCompiler,
        "java-compiler" => NativeTool::JavaCompiler,
        "csharp-compiler" => NativeTool::CSharpCompiler,
        "clang" => NativeTool::Clang,
        _ => return None,
    };
    Some(tool)
}

fn encode_native_kind(kind: NativeUnavailableKind) -> &'static str {
    match kind {
        NativeUnavailableKind::MissingPath => "missing-path",
        NativeUnavailableKind::MissingTool => "missing-tool",
        NativeUnavailableKind::Canonicalize => "canonicalize",
        NativeUnavailableKind::ProbeVersion => "probe-version",
        NativeUnavailableKind::VersionRejected => "version-rejected",
        NativeUnavailableKind::EmptyVersion => "empty-version",
        NativeUnavailableKind::Resolve => "resolve",
    }
}

fn decode_native_kind(text: &str) -> Option<NativeUnavailableKind> {
    let kind = match text {
        "missing-path" => NativeUnavailableKind::MissingPath,
        "missing-tool" => NativeUnavailableKind::MissingTool,
        "canonicalize" => NativeUnavailableKind::Canonicalize,
        "probe-version" => NativeUnavailableKind::ProbeVersion,
        "version-rejected" => NativeUnavailableKind::VersionRejected,
        "empty-version" => NativeUnavailableKind::EmptyVersion,
        "resolve" => NativeUnavailableKind::Resolve,
        _ => return None,
    };
    Some(kind)
}

fn encode_language(language: CorpusLanguage) -> &'static str {
    match language {
        CorpusLanguage::Rust => "rust",
        CorpusLanguage::TypeScript => "typescript",
        CorpusLanguage::Python => "python",
        CorpusLanguage::Go => "go",
        CorpusLanguage::Java => "java",
        CorpusLanguage::CSharp => "csharp",
        CorpusLanguage::Clang => "clang",
    }
}

fn decode_language(text: &str) -> Option<CorpusLanguage> {
    let language = match text {
        "rust" => CorpusLanguage::Rust,
        "typescript" => CorpusLanguage::TypeScript,
        "python" => CorpusLanguage::Python,
        "go" => CorpusLanguage::Go,
        "java" => CorpusLanguage::Java,
        "csharp" => CorpusLanguage::CSharp,
        "clang" => CorpusLanguage::Clang,
        _ => return None,
    };
    Some(language)
}

pub(super) fn encode_cause(cause: &AuthorityUnavailableCause) -> String {
    match *cause {
        AuthorityUnavailableCause::Native(NativeUnavailableCause { tool, kind }) => format!(
            "native:{}:{}",
            encode_native_tool(tool),
            encode_native_kind(kind)
        ),
        AuthorityUnavailableCause::Source(SourceUnavailableCause { language, kind }) => {
            format!(
                "source:{}:{}",
                encode_language(language),
                encode_source_kind(kind)
            )
        }
        AuthorityUnavailableCause::RustAuthority => "rust-authority".to_owned(),
        AuthorityUnavailableCause::GoOracle => "go-oracle".to_owned(),
        AuthorityUnavailableCause::JavaHarness => "java-harness".to_owned(),
        AuthorityUnavailableCause::CSharpHelper => "csharp-helper".to_owned(),
        AuthorityUnavailableCause::TypeScriptChecker => "typescript-checker".to_owned(),
        AuthorityUnavailableCause::PythonChecker => "python-checker".to_owned(),
        AuthorityUnavailableCause::ObserverUnavailable => "observer-unavailable".to_owned(),
        AuthorityUnavailableCause::NotCompared => "not-compared".to_owned(),
        AuthorityUnavailableCause::RowProcessCrash { signal, code } => {
            format!("row-process-crash:{signal}:{code}")
        }
    }
}

fn decode_cause(text: &str) -> Option<AuthorityUnavailableCause> {
    if let Some(rest) = text.strip_prefix("native:") {
        let (tool, kind) = rest.split_once(':')?;
        return Some(AuthorityUnavailableCause::Native(NativeUnavailableCause {
            tool: decode_native_tool(tool)?,
            kind: decode_native_kind(kind)?,
        }));
    }
    if let Some(rest) = text.strip_prefix("source:") {
        let (language, kind) = rest.split_once(':')?;
        return Some(AuthorityUnavailableCause::Source(SourceUnavailableCause {
            language: decode_language(language)?,
            kind: decode_source_kind(kind)?,
        }));
    }
    if let Some(rest) = text.strip_prefix("row-process-crash:") {
        let (signal, code) = rest.split_once(':')?;
        return Some(AuthorityUnavailableCause::RowProcessCrash {
            signal: signal.parse().ok()?,
            code: code.parse().ok()?,
        });
    }
    let cause = match text {
        "rust-authority" => AuthorityUnavailableCause::RustAuthority,
        "go-oracle" => AuthorityUnavailableCause::GoOracle,
        "java-harness" => AuthorityUnavailableCause::JavaHarness,
        "csharp-helper" => AuthorityUnavailableCause::CSharpHelper,
        "typescript-checker" => AuthorityUnavailableCause::TypeScriptChecker,
        "python-checker" => AuthorityUnavailableCause::PythonChecker,
        "observer-unavailable" => AuthorityUnavailableCause::ObserverUnavailable,
        "not-compared" => AuthorityUnavailableCause::NotCompared,
        _ => return None,
    };
    Some(cause)
}

pub(super) fn encode_terminal_kind(kind: CompileTerminalKind) -> &'static str {
    match kind {
        CompileTerminalKind::SourceLength => "source-length",
        CompileTerminalKind::UnsupportedStage => "unsupported-stage",
        CompileTerminalKind::ToolchainSelectionMismatch => "toolchain-selection-mismatch",
        CompileTerminalKind::ToolchainMismatch => "toolchain-mismatch",
        CompileTerminalKind::NativeWork => "native-work",
        CompileTerminalKind::NativeWorkCleanup => "native-work-cleanup",
        CompileTerminalKind::ToolingUnavailable => "tooling-unavailable",
        CompileTerminalKind::ToolStart => "tool-start",
        CompileTerminalKind::MissingToolInput => "missing-tool-input",
        CompileTerminalKind::MissingToolInputCleanup => "missing-tool-input-cleanup",
        CompileTerminalKind::MissingToolDiagnostic => "missing-tool-diagnostic",
        CompileTerminalKind::MissingToolDiagnosticCleanup => "missing-tool-diagnostic-cleanup",
        CompileTerminalKind::ToolInput => "tool-input",
        CompileTerminalKind::ToolInputCleanup => "tool-input-cleanup",
        CompileTerminalKind::ToolTerminate => "tool-terminate",
        CompileTerminalKind::ToolWait => "tool-wait",
        CompileTerminalKind::ToolWaitCleanup => "tool-wait-cleanup",
        CompileTerminalKind::ToolDiagnosticRead => "tool-diagnostic-read",
        CompileTerminalKind::ToolDiagnosticReadCleanup => "tool-diagnostic-read-cleanup",
        CompileTerminalKind::NativeWorkerPanic => "native-worker-panic",
        CompileTerminalKind::Cancelled => "cancelled",
        CompileTerminalKind::DeadlineExceeded => "deadline-exceeded",
        CompileTerminalKind::DiagnosticLimit => "diagnostic-limit",
        CompileTerminalKind::NativeRejected => "native-rejected",
        CompileTerminalKind::Authority => "authority",
        CompileTerminalKind::AuthorityInputRequired => "authority-input-required",
        CompileTerminalKind::AuthorityInputProfileMismatch => "authority-input-profile-mismatch",
        CompileTerminalKind::LoweringUnsupported => "lowering-unsupported",
        CompileTerminalKind::ExtensionAtomUnbound => "extension-atom-unbound",
        CompileTerminalKind::ExtensionTypeParametersUnbound => "extension-type-parameters-unbound",
        CompileTerminalKind::ClangProjection => "clang-projection",
        CompileTerminalKind::Build => "build",
        CompileTerminalKind::Prepare => "prepare",
        CompileTerminalKind::Write => "write",
        CompileTerminalKind::Validate => "validate",
    }
}

fn decode_terminal_kind(text: &str) -> Option<CompileTerminalKind> {
    let kind = match text {
        "source-length" => CompileTerminalKind::SourceLength,
        "unsupported-stage" => CompileTerminalKind::UnsupportedStage,
        "toolchain-selection-mismatch" => CompileTerminalKind::ToolchainSelectionMismatch,
        "toolchain-mismatch" => CompileTerminalKind::ToolchainMismatch,
        "native-work" => CompileTerminalKind::NativeWork,
        "native-work-cleanup" => CompileTerminalKind::NativeWorkCleanup,
        "tooling-unavailable" => CompileTerminalKind::ToolingUnavailable,
        "tool-start" => CompileTerminalKind::ToolStart,
        "missing-tool-input" => CompileTerminalKind::MissingToolInput,
        "missing-tool-input-cleanup" => CompileTerminalKind::MissingToolInputCleanup,
        "missing-tool-diagnostic" => CompileTerminalKind::MissingToolDiagnostic,
        "missing-tool-diagnostic-cleanup" => CompileTerminalKind::MissingToolDiagnosticCleanup,
        "tool-input" => CompileTerminalKind::ToolInput,
        "tool-input-cleanup" => CompileTerminalKind::ToolInputCleanup,
        "tool-terminate" => CompileTerminalKind::ToolTerminate,
        "tool-wait" => CompileTerminalKind::ToolWait,
        "tool-wait-cleanup" => CompileTerminalKind::ToolWaitCleanup,
        "tool-diagnostic-read" => CompileTerminalKind::ToolDiagnosticRead,
        "tool-diagnostic-read-cleanup" => CompileTerminalKind::ToolDiagnosticReadCleanup,
        "native-worker-panic" => CompileTerminalKind::NativeWorkerPanic,
        "cancelled" => CompileTerminalKind::Cancelled,
        "deadline-exceeded" => CompileTerminalKind::DeadlineExceeded,
        "diagnostic-limit" => CompileTerminalKind::DiagnosticLimit,
        "native-rejected" => CompileTerminalKind::NativeRejected,
        "authority" => CompileTerminalKind::Authority,
        "authority-input-required" => CompileTerminalKind::AuthorityInputRequired,
        "authority-input-profile-mismatch" => CompileTerminalKind::AuthorityInputProfileMismatch,
        "lowering-unsupported" => CompileTerminalKind::LoweringUnsupported,
        "extension-atom-unbound" => CompileTerminalKind::ExtensionAtomUnbound,
        "extension-type-parameters-unbound" => CompileTerminalKind::ExtensionTypeParametersUnbound,
        "clang-projection" => CompileTerminalKind::ClangProjection,
        "build" => CompileTerminalKind::Build,
        "prepare" => CompileTerminalKind::Prepare,
        "write" => CompileTerminalKind::Write,
        "validate" => CompileTerminalKind::Validate,
        _ => return None,
    };
    Some(kind)
}

/// The row worker: one frozen row, one fresh process.
///
/// The audit parent re-executes this test binary with
/// `NUDOX_AUDIT_ROW=<ordinal>` and `NUDOX_ROW_OUTCOME_FILE=<path>`. The worker
/// resolves, stages, compiles, publishes, reopens, and compares exactly that
/// row — the same [`audit_source_case`] body the audit used to run in-process
/// — and records its typed disposition plus every mismatch/unavailable entry
/// in the outcome file. A native fault inside the worker (the libclang FFI
/// crash that used to erase the whole run) kills only this process; the
/// parent turns the lost process into a typed
/// [`AuthorityUnavailableCause::RowProcessCrash`] row.
///
/// Without `NUDOX_AUDIT_ROW` — every ordinary test run — this is a no-op.
#[test]
fn real_audit_row_worker() -> Result<(), CorpusAuditError> {
    let Ok(ordinal) = std::env::var("NUDOX_AUDIT_ROW") else {
        return Ok(());
    };
    // Test-only fault injection: die exactly the way the in-process native
    // crash died — hard, mid-row, with no typed outcome — so the isolation
    // path can be exercised end to end.
    if std::env::var("NUDOX_ROW_WORKER_FAULT").as_deref() == Ok("abort") {
        eprintln!("real-row-worker fault-injected abort ordinal={ordinal}");
        std::process::abort();
    }
    // Test-only fault injection for the cap path: fork a sleeping grandchild
    // (the shape of a supervised native oracle/checker child), record its pid
    // for the parent, and then idle past any cap. The parent kills the
    // worker's whole process group; the grandchild must be reaped with it.
    if std::env::var("NUDOX_ROW_WORKER_FAULT").as_deref() == Ok("fork-sleep") {
        eprintln!("real-row-worker fault-injected fork-sleep ordinal={ordinal}");
        let pid_file = std::env::var("NUDOX_ROW_GRANDCHILD_PID_FILE").unwrap_or_default();
        let mut grandchild = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 300")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("fault worker must fork its sleeping grandchild");
        if !pid_file.is_empty() {
            let _ = fs::write(&pid_file, grandchild.id().to_string());
        }
        // Idle well past the test cap so only the cap can end this worker.
        std::thread::sleep(Duration::from_secs(120));
        let _ = grandchild.kill();
        let _ = grandchild.wait();
        return Ok(());
    }
    let ordinal: usize = ordinal.parse().map_err(|error| CorpusAuditError::Io {
        phase: AuditIoPhase::Fixture,
        source: io::Error::new(io::ErrorKind::InvalidInput, format!("row ordinal: {error}")),
    })?;
    let outcome_path =
        std::env::var("NUDOX_ROW_OUTCOME_FILE").map_err(|error| CorpusAuditError::Io {
            phase: AuditIoPhase::Fixture,
            source: io::Error::new(io::ErrorKind::NotFound, format!("outcome file: {error}")),
        })?;

    let mut telemetry = RowTelemetry::default();
    // Drive the row on a dedicated big-stack thread (see
    // [`ROW_WORKER_STACK_BYTES`]); libtest's own thread is too small for the
    // deepest real sources.
    let (sender, receiver) = std::sync::mpsc::channel();
    let drive_thread = std::thread::Builder::new()
        .name("real-audit-row-worker-drive".to_owned())
        .stack_size(ROW_WORKER_STACK_BYTES)
        .spawn(move || {
            let (disposition, unavailable, mismatches) = drive_worker_row(ordinal, &mut telemetry);
            let _ = sender.send((telemetry, unavailable, mismatches, disposition));
        });
    let mut telemetry = RowTelemetry::default();
    let (disposition, unavailable, mismatches) = match drive_thread {
        Err(error) => {
            let reason = format!("worker drive thread could not start: {error}");
            telemetry.diag(reason.clone());
            return Err(write_internal_outcome(Path::new(&outcome_path), &reason));
        }
        Ok(handle) => match receiver.recv() {
            Ok((mut worker_telemetry, unavailable, mismatches, disposition)) => {
                if handle.join().is_err() {
                    eprintln!("real-row-worker drive thread panicked after emitting its outcome");
                }
                telemetry.diags.append(&mut worker_telemetry.diags);
                (disposition, unavailable, mismatches)
            }
            Err(_) => {
                let _ = handle.join();
                let reason = "worker drive thread panicked before emitting its outcome".to_owned();
                telemetry.diag(reason.clone());
                return Err(write_internal_outcome(Path::new(&outcome_path), &reason));
            }
        },
    };
    let mut records: Vec<String> = telemetry
        .diags
        .iter()
        .map(|diag| format!("diag {}", escape_line(diag)))
        .collect();
    for entry in &unavailable {
        let CorpusMismatch::RealUnavailable { field, cause, .. } = entry else {
            continue;
        };
        records.push(format!(
            "unavailable field={} cause={}",
            encode_real_field(*field),
            encode_cause(cause),
        ));
    }
    for entry in &mismatches {
        let CorpusMismatch::Real {
            field,
            expected,
            observed,
            ..
        } = entry
        else {
            continue;
        };
        records.push(format!(
            "mismatch real field={} expected={} observed={}",
            encode_real_field(*field),
            encode_digest(expected),
            encode_digest(observed),
        ));
    }
    match &disposition {
        Ok(RealCaseDisposition::Output {
            mismatches: count,
            unavailable: observer_unavailable,
            not_compared,
            verified,
        }) => records.push(format!(
            "outcome output verified={} mismatches={count} unavailable={observer_unavailable} not_compared={not_compared}",
            usize::from(*verified),
        )),
        Ok(RealCaseDisposition::Unavailable(cause)) => {
            records.push(format!("outcome unavailable cause={}", encode_cause(cause),))
        }
        Ok(RealCaseDisposition::Terminal(terminal)) => records.push(format!(
            "outcome terminal kind={}",
            encode_terminal_kind(*terminal),
        )),
        Err(error) => {
            records.push(format!(
                "outcome internal reason={}",
                escape_line(&error.to_string()),
            ));
        }
    }
    let mut text = records.join("\n");
    text.push('\n');
    fs::write(&outcome_path, text.as_bytes()).map_err(|source| CorpusAuditError::Io {
        phase: AuditIoPhase::Fixture,
        source,
    })?;
    disposition.map(|_| ())
}

/// Drives exactly one row by selection ordinal inside the worker process.
/// The row's result and its sink entries are returned together so the worker
/// test can serialize both even when the row body failed hard.
fn drive_worker_row(
    ordinal: usize,
    telemetry: &mut RowTelemetry,
) -> (
    Result<RealCaseDisposition, CorpusAuditError>,
    Vec<CorpusMismatch>,
    Vec<CorpusMismatch>,
) {
    let mut unavailable = Vec::new();
    let mut mismatches = Vec::new();
    let disposition = drive_worker_row_inner(ordinal, telemetry, &mut unavailable, &mut mismatches);
    (disposition, unavailable, mismatches)
}

fn drive_worker_row_inner(
    ordinal: usize,
    telemetry: &mut RowTelemetry,
    unavailable: &mut Vec<CorpusMismatch>,
    mismatches: &mut Vec<CorpusMismatch>,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let missing = CorpusAuditError::Invariant {
        key: None,
        cause: CorpusInvariant::MissingCase,
    };
    let mut input = inventory::real_package_input_at(ordinal).ok_or_else(|| missing)?;
    let hosts = HostTools::resolve();
    let resolved = ResolvedTools::from_hosts(&hosts);
    let mut publisher = PassPublisher::new(Pass::Original, REAL_FRAGMENT_BYTES)?;
    let disposition = match &mut input {
        inventory::RealPackageInput::Unavailable { cause, .. } => Ok(
            RealCaseDisposition::Unavailable(AuthorityUnavailableCause::Source(*cause)),
        ),
        inventory::RealPackageInput::Source { case, source } => {
            let setup_key = CaseKey {
                case_id: case.case_id,
                language: case.language,
                shape: PackageShape::Reference,
            };
            let authorities = match case.language {
                // Only these two arms consult the provider slots; every other
                // lane takes a typed-absent factory it never reads.
                CorpusLanguage::Java | CorpusLanguage::CSharp => AuthorityFactory::new(&hosts)
                    .map_err(|cause| CorpusAuditError::AuthoritySetup {
                        key: setup_key,
                        cause,
                    })?,
                _ => AuthorityFactory::absent(),
            };
            audit_source_case(
                *case,
                source,
                &hosts,
                &resolved,
                &authorities,
                &mut publisher,
                unavailable,
                mismatches,
                telemetry,
            )
        }
    };
    publisher.finish()?;
    disposition
}

fn audit_source_case(
    case: inventory::RealPackageCase,
    source: &mut inventory::ResolvedPackageSource,
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
    publisher: &mut PassPublisher,
    unavailable_sink: &mut Vec<CorpusMismatch>,
    mismatch_sink: &mut Vec<CorpusMismatch>,
    telemetry: &mut RowTelemetry,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let (profile, toolchain) = match native_slot(case.language, resolved) {
        Ok(value) => value,
        Err(cause) => {
            return Ok(RealCaseDisposition::Unavailable(
                AuthorityUnavailableCause::Native(cause),
            ));
        }
    };
    source
        .bind_toolchain(native_tool(case.language), toolchain)
        .map_err(|_| CorpusAuditError::Invariant {
            key: None,
            cause: CorpusInvariant::AuthorityBindingMismatch,
        })?;
    let (ecosystem, package) = case.coordinate.lineage();
    let lineage = PackageLineage::new(ecosystem.as_str(), package.as_str())
        .map_err(CorpusAuditError::ScopeLineage)?;
    let path = source
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source");
    let scope = DeclarationScope::new(lineage, path).map_err(CorpusAuditError::ScopeKey)?;
    let mut expected = SourceExpectation {
        source: source.identity,
        file: path.as_bytes().to_vec().into_boxed_slice(),
        profile,
        tool: native_tool(case.language),
        recipe: backend_semantic::vocabulary::CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            native_tool(case.language),
            source.identity.identity,
            toolchain.identity,
        ),
        spans: SourceAuthorityFacts::Unavailable,
        authority: real_authority::AuthorityExpectation::default(),
    };
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let work = NativeWork::create().map_err(|cause| CorpusAuditError::NativeWork {
        key: CaseKey {
            case_id: case.case_id,
            language: case.language,
            shape: PackageShape::Reference,
        },
        cause,
    })?;
    let cancelled = AtomicBool::new(false);
    let mut fragment_output = vec![0xa5_u8; REAL_FRAGMENT_BYTES];

    match case.language {
        CorpusLanguage::Rust => {
            let Some(host) = hosts.rust.host() else {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::Native(hosts.rust.cause),
                ));
            };
            let fixture = match rust_fixture_for_source(case.case_id.raw(), &source.bytes, host) {
                Ok(fixture) => fixture,
                Err(error) if rust_error_is_unavailable(&error) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::RustAuthority,
                    ));
                }
                Err(error) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause: error,
                    });
                }
            };
            // The authority expectation is built inside the row worker while
            // the rust-analyzer owner is alive; it owns every name it carries.
            // The lane attaches the authority's exact declaration extents
            // and name-token spans, so the span plane is verified by the
            // authority declaration span join.
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority = real_authority::rust_authority(
                &fixture.project,
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &source.bytes,
            );
            let compiled = compile_with_authority_until(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Rust {
                    project: &fixture.project,
                    maximum_source_bytes: backend_frontend_rust::legacy::SourceByteLimit(
                        u32::try_from(source.bytes.len()).unwrap_or(u32::MAX),
                    ),
                    features: fixture.features,
                },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::Go => {
            let fixture =
                match go_fixture_for_real_source(case.case_id.raw(), &source.path, &source.bytes) {
                    Ok(fixture) => fixture,
                    Err(AuthorityBuildError::Go(error)) if go_error_is_unavailable(&error) => {
                        // The oracle's exit status and stderr tail stay part of
                        // the row record, both in the worker's own log and in the
                        // parent's reprint.
                        telemetry.diag(format!(
                            "real-setup lang=Go coord={:?} error={error:?}",
                            case.coordinate.raw(),
                        ));
                        return Ok(RealCaseDisposition::Unavailable(
                            AuthorityUnavailableCause::GoOracle,
                        ));
                    }
                    Err(error) => {
                        return Err(CorpusAuditError::AuthoritySetup {
                            key: CaseKey {
                                case_id: case.case_id,
                                language: case.language,
                                shape: PackageShape::Reference,
                            },
                            cause: error,
                        });
                    }
                };
            // The image marks bound rows with their name-token extents and
            // the lane attaches those extents as entity spans, so the span
            // plane is verified by the authority declaration span join.
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority = real_authority::go_authority(&fixture.image);
            let compiled = compile_with_authority_until(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Go {
                    image: &fixture.image,
                },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::Java => {
            let ProviderSlot::Ready(provider) = &authorities.java else {
                let ProviderSlot::Unavailable(cause) = &authorities.java else {
                    unreachable!()
                };
                return Ok(RealCaseDisposition::Unavailable(*cause));
            };
            let image = match provider.image_with_package_source(
                &source.bytes,
                &source.path,
                &source.package_root,
            ) {
                Ok(image) => image,
                Err(error) if java_row_is_unavailable(&error) => {
                    telemetry.diag(format!(
                        "real-setup lang=Java coord={:?} error={error:?}",
                        case.coordinate.raw(),
                    ));
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::JavaHarness,
                    ));
                }
                Err(cause) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause,
                    });
                }
            };
            // Binding-unit declarations carry their own UTF-16 extents,
            // projected onto the byte domain, so the span plane is verified
            // by the authority declaration span join.
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority = real_authority::java_authority(&image, &source.bytes);
            let compiled = compile_with_authority_until(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Java { image: &image },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::CSharp => {
            let ProviderSlot::Ready(provider) = &authorities.csharp else {
                let ProviderSlot::Unavailable(cause) = &authorities.csharp else {
                    unreachable!()
                };
                return Ok(RealCaseDisposition::Unavailable(*cause));
            };
            let image = match provider.image_for_package_source(
                case.case_id.raw(),
                &source.package_root,
                &source.path,
            ) {
                Ok(image) => image,
                Err(error) if csharp_row_is_unavailable(&error) => {
                    telemetry.diag(format!(
                        "real-setup lang=CSharp coord={:?} error={error:?}",
                        case.coordinate.raw(),
                    ));
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::CSharpHelper,
                    ));
                }
                Err(cause) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause,
                    });
                }
            };
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority = real_authority::csharp_authority(&image);
            let compiled = compile_with_authority_until(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::CSharp { image: &image },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::TypeScript => {
            let checker = backend_frontend_typescript::legacy::Checker {
                output_limit: TS_REPORT_OUTPUT_LIMIT,
                timeout: TS_REPORT_DEADLINE,
            };
            let LanguageProfile::TypeScript(profile) = profile else {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::TypeScriptChecker,
                ));
            };
            let report = match checker.run_in_package(profile, &source.bytes, &source.package_root)
            {
                Ok(report) => report,
                Err(error) => {
                    telemetry.diag(format!(
                        "real-setup lang=TypeScript coord={:?} error={error:?}",
                        case.coordinate.raw(),
                    ));
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::TypeScriptChecker,
                    ));
                }
            };
            // The lane registers the authority's exact declaration spans, so
            // the span plane is verified by the authority declaration span
            // join.
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority =
                real_authority::typescript_authority(&report, profile, &source.bytes);
            let compiled = compile_with_authority_until(
                LanguageProfile::TypeScript(profile),
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::TypeScript { report: &report },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            // The checker staged the package but the authority rejects the
            // selected file's syntax: the source is at fault, so the row is
            // unavailable rather than a parity mismatch.
            if let Err(
                failure @ CompileFailure::Authority {
                    failure:
                        backend_engine::driver::AuthorityFailure::TypeScript {
                            cause:
                                backend_frontend_typescript::legacy::AuthorityError::Syntax { .. },
                            ..
                        },
                    ..
                },
            ) = &compiled
            {
                telemetry.diag(format!(
                    "real-terminal lang=TypeScript coord={:?} kind=Authority failure={failure:?}",
                    case.coordinate.raw(),
                ));
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::TypeScriptChecker,
                ));
            }
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::Python => {
            let profile = PythonVersion::Python314;
            let facts = match backend_frontend_python::legacy::extract(&source.bytes, profile) {
                Ok(facts) => facts,
                Err(_) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            let checker = backend_frontend_python::legacy::Pyrefly::from_env();
            if !checker.is_available() {
                return Ok(RealCaseDisposition::Unavailable(
                    AuthorityUnavailableCause::PythonChecker,
                ));
            }
            let report = match checker.analyze(&source.bytes, profile, &facts) {
                Ok(report) => report,
                Err(_) => {
                    return Ok(RealCaseDisposition::Unavailable(
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority = real_authority::python_authority(&facts);
            let compiled = compile_with_authority_until(
                LanguageProfile::Python(profile),
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Python { report: &report },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
        CorpusLanguage::Clang => {
            let project = match backend_frontend_clang::ClangProject::open(
                &source.package_root,
                &source.path,
            ) {
                Ok(project) => project,
                Err(cause) => {
                    return Err(CorpusAuditError::AuthoritySetup {
                        key: CaseKey {
                            case_id: case.case_id,
                            language: case.language,
                            shape: PackageShape::Reference,
                        },
                        cause: cause.into(),
                    });
                }
            };
            expected.spans = SourceAuthorityFacts::Spans;
            expected.authority =
                real_authority::clang_authority(&project, &source.bytes, &cancelled);
            let compiled = compile_with_authority_until(
                profile,
                &source.bytes,
                scope,
                toolchain,
                SemanticAuthorityInput::Clang { project: &project },
                &cancelled,
                Instant::now() + REAL_ROW_AUTHORITY_DEADLINE,
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_real_compile(
                case,
                expected,
                compiled,
                work,
                publisher,
                unavailable_sink,
                mismatch_sink,
                telemetry,
            )
        }
    }
}

const REAL_FRAGMENT_BYTES: usize = 16 * 1024 * 1024;

fn finish_real_compile<'diagnostic, 'output>(
    case: inventory::RealPackageCase,
    expected: SourceExpectation,
    compiled: Result<CompiledSemantic<'output>, CompileFailure<'diagnostic>>,
    work: NativeWork,
    publisher: &mut PassPublisher,
    unavailable_sink: &mut Vec<CorpusMismatch>,
    mismatch_sink: &mut Vec<CorpusMismatch>,
    _telemetry: &mut RowTelemetry,
) -> Result<RealCaseDisposition, CorpusAuditError> {
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let terminal = terminal_kind(&failure);
            work.assert_empty()
                .map_err(|cause| CorpusAuditError::NativeWork {
                    key: CaseKey {
                        case_id: case.case_id,
                        language: case.language,
                        shape: PackageShape::Reference,
                    },
                    cause,
                })?;
            return Ok(RealCaseDisposition::Terminal(terminal));
        }
    };
    work.assert_empty()
        .map_err(|cause| CorpusAuditError::NativeWork {
            key: CaseKey {
                case_id: case.case_id,
                language: case.language,
                shape: PackageShape::Reference,
            },
            cause,
        })?;
    let owned = observe_owned_semantic(&compiled.ir, None);
    // The plane checks run inside the publication reader closure — the
    // reopened semantic image is only lent here, exactly as grouped.rs does —
    // so every authority plane is verified against both readers in one
    // publish/reopen transaction.
    let before = mismatch_sink.len();
    let before_unavailable = unavailable_sink.len();
    let not_compared = publisher.publish_with_reader(&compiled, |_fragment, image| {
        let reopened = observe_reopened_semantic(image, None);
        Ok(check_real_output(
            case,
            &expected,
            &compiled,
            image,
            &owned,
            &reopened,
            unavailable_sink,
            mismatch_sink,
        ))
    })?;
    let count = mismatch_sink.len().saturating_sub(before);
    let total_unavailable = unavailable_sink.len().saturating_sub(before_unavailable);
    let not_compared_entries = unavailable_sink[before_unavailable..]
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                CorpusMismatch::RealUnavailable {
                    cause: AuthorityUnavailableCause::NotCompared,
                    ..
                }
            )
        })
        .count();
    let observer_unavailable = total_unavailable.saturating_sub(not_compared_entries);
    let verified = count == 0 && observer_unavailable == 0 && not_compared == 0;
    Ok(RealCaseDisposition::Output {
        mismatches: count,
        unavailable: observer_unavailable,
        not_compared,
        verified,
    })
}

fn check_real_output<R: SemanticReader + ?Sized>(
    case: inventory::RealPackageCase,
    expected: &SourceExpectation,
    compiled: &CompiledSemantic<'_>,
    image: &R,
    owned: &observation::SemanticObservation,
    reopened: &observation::SemanticObservation,
    unavailable: &mut Vec<CorpusMismatch>,
    mismatches: &mut Vec<CorpusMismatch>,
) -> usize {
    if compiled.artifact.source != expected.source {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Source,
            expected: digest_source(expected.source),
            observed: digest_source(compiled.artifact.source),
        });
    }
    if compiled.artifact.recipe.profile != expected.profile {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Profile,
            expected: digest_typed(&expected.profile),
            observed: digest_typed(&compiled.artifact.recipe.profile),
        });
    }
    if compiled.artifact.recipe != expected.recipe {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Recipe,
            expected: digest_recipe(expected.recipe),
            observed: digest_recipe(compiled.artifact.recipe),
        });
    }
    if compiled.artifact.recipe.tool != expected.tool {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Toolchain,
            expected: digest_u64(u64::from(u8::from(expected.tool))),
            observed: digest_u64(u64::from(u8::from(compiled.artifact.recipe.tool))),
        });
    }
    if compiled.artifact.recipe.toolchain != expected.recipe.toolchain {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Toolchain,
            expected: digest_typed(&expected.recipe.toolchain),
            observed: digest_typed(&compiled.artifact.recipe.toolchain),
        });
    }
    if owned != reopened {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::Reopened,
            expected: digest_semantic(owned),
            observed: digest_semantic(reopened),
        });
    }
    let mut not_compared = 0_usize;
    match expected.spans {
        // The lane grounds declaration spans: the span plane is verified by
        // the authority declaration span join on both readers below, which
        // compares every joined span after replacing transient file atom IDs
        // with their canonical path bytes.
        SourceAuthorityFacts::Spans => {}
        SourceAuthorityFacts::Unavailable => {
            unavailable.push(CorpusMismatch::RealUnavailable {
                case,
                field: RealAuditField::SourceSpans,
                cause: AuthorityUnavailableCause::NotCompared,
            });
            not_compared += 1;
        }
    }
    not_compared += real_authority::check_authority_planes(
        &compiled.ir,
        image,
        &expected.file,
        case,
        &expected.authority,
        unavailable,
        mismatches,
    );
    check_image_provenance(
        case,
        expected,
        owned.image.as_ref(),
        RealAuditField::SemanticImage,
        unavailable,
        mismatches,
    );
    check_image_provenance(
        case,
        expected,
        reopened.image.as_ref(),
        RealAuditField::Reopened,
        unavailable,
        mismatches,
    );
    if owned.identity != reopened.identity {
        mismatches.push(CorpusMismatch::Real {
            case,
            field: RealAuditField::SemanticImage,
            expected: digest_typed(&owned.identity),
            observed: digest_typed(&reopened.identity),
        });
    }
    not_compared
}

fn check_image_provenance(
    case: inventory::RealPackageCase,
    expected: &SourceExpectation,
    image: Option<&observation::ObservedImageFacts>,
    field: RealAuditField,
    unavailable: &mut Vec<CorpusMismatch>,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let Some(image) = image else {
        unavailable.push(CorpusMismatch::RealUnavailable {
            case,
            field,
            cause: AuthorityUnavailableCause::ObserverUnavailable,
        });
        return;
    };
    match &image.provenance {
        observation::ObservedImageProvenance::Captured {
            source: observed_source,
            recipe: observed_recipe,
            ..
        } => {
            if *observed_source != expected.source {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field,
                    expected: digest_source(expected.source),
                    observed: digest_source(*observed_source),
                });
            }
            if *observed_recipe != expected.recipe {
                mismatches.push(CorpusMismatch::Real {
                    case,
                    field,
                    expected: digest_recipe(expected.recipe),
                    observed: digest_recipe(*observed_recipe),
                });
            }
        }
        observation::ObservedImageProvenance::Unavailable => {
            unavailable.push(CorpusMismatch::RealUnavailable {
                case,
                field,
                cause: AuthorityUnavailableCause::ObserverUnavailable,
            });
        }
    }
}

#[cfg(test)]
mod worker_isolation_tests {
    use super::*;
    use super::{
        RealAuditField, RealCaseDisposition, TS_REPORT_DEADLINE, TS_REPORT_OUTPUT_LIMIT,
        decode_cause, decode_real_field, decode_terminal_kind, drive_row_in_worker_with_env,
        encode_cause, encode_real_field, encode_terminal_kind, escape_line, parse_row_outcome,
        row_crash_cause, unescape_line,
    };
    use std::process::Command;

    /// Every terminal kind, field, and cause must survive the worker outcome
    /// protocol unchanged — a lost encoding would silently rewrite a row's
    /// typed disposition.
    #[test]
    fn worker_protocol_roundtrips_every_typed_value() {
        for terminal in [
            CompileTerminalKind::SourceLength,
            CompileTerminalKind::UnsupportedStage,
            CompileTerminalKind::ToolchainSelectionMismatch,
            CompileTerminalKind::ToolchainMismatch,
            CompileTerminalKind::NativeWork,
            CompileTerminalKind::NativeWorkCleanup,
            CompileTerminalKind::ToolingUnavailable,
            CompileTerminalKind::ToolStart,
            CompileTerminalKind::MissingToolInput,
            CompileTerminalKind::MissingToolInputCleanup,
            CompileTerminalKind::MissingToolDiagnostic,
            CompileTerminalKind::MissingToolDiagnosticCleanup,
            CompileTerminalKind::ToolInput,
            CompileTerminalKind::ToolInputCleanup,
            CompileTerminalKind::ToolTerminate,
            CompileTerminalKind::ToolWait,
            CompileTerminalKind::ToolWaitCleanup,
            CompileTerminalKind::ToolDiagnosticRead,
            CompileTerminalKind::ToolDiagnosticReadCleanup,
            CompileTerminalKind::NativeWorkerPanic,
            CompileTerminalKind::Cancelled,
            CompileTerminalKind::DeadlineExceeded,
            CompileTerminalKind::DiagnosticLimit,
            CompileTerminalKind::NativeRejected,
            CompileTerminalKind::Authority,
            CompileTerminalKind::AuthorityInputRequired,
            CompileTerminalKind::AuthorityInputProfileMismatch,
            CompileTerminalKind::LoweringUnsupported,
            CompileTerminalKind::ExtensionAtomUnbound,
            CompileTerminalKind::ExtensionTypeParametersUnbound,
            CompileTerminalKind::ClangProjection,
            CompileTerminalKind::Build,
            CompileTerminalKind::Prepare,
            CompileTerminalKind::Write,
            CompileTerminalKind::Validate,
        ] {
            assert_eq!(
                decode_terminal_kind(encode_terminal_kind(terminal)),
                Some(terminal)
            );
        }
        for field in [
            RealAuditField::Source,
            RealAuditField::Profile,
            RealAuditField::Recipe,
            RealAuditField::Toolchain,
            RealAuditField::SourceSpans,
            RealAuditField::Declarations,
            RealAuditField::Types,
            RealAuditField::Relations,
            RealAuditField::Documentation,
            RealAuditField::Extensions,
            RealAuditField::Discovery,
            RealAuditField::CanonicalType,
            RealAuditField::SemanticImage,
            RealAuditField::Reopened,
            RealAuditField::Render,
        ] {
            assert_eq!(decode_real_field(encode_real_field(field)), Some(field));
        }
        for cause in [
            AuthorityUnavailableCause::RustAuthority,
            AuthorityUnavailableCause::GoOracle,
            AuthorityUnavailableCause::JavaHarness,
            AuthorityUnavailableCause::CSharpHelper,
            AuthorityUnavailableCause::TypeScriptChecker,
            AuthorityUnavailableCause::PythonChecker,
            AuthorityUnavailableCause::ObserverUnavailable,
            AuthorityUnavailableCause::NotCompared,
            AuthorityUnavailableCause::Native(NativeUnavailableCause {
                tool: NativeTool::Clang,
                kind: NativeUnavailableKind::VersionRejected,
            }),
            AuthorityUnavailableCause::Source(SourceUnavailableCause {
                language: CorpusLanguage::Go,
                kind: SourceUnavailableKind::PackageDirectoryMissing,
            }),
            AuthorityUnavailableCause::RowProcessCrash {
                signal: 11,
                code: -1,
            },
            AuthorityUnavailableCause::RowProcessCrash {
                signal: 0,
                code: 101,
            },
        ] {
            assert_eq!(decode_cause(&encode_cause(&cause)), Some(cause));
        }
    }

    /// Telemetry text keeps newlines and backslashes through the single-line
    /// protocol escape.
    #[test]
    fn telemetry_escape_roundtrips_control_characters() {
        for text in [
            "plain".to_owned(),
            "line one\nline two".to_owned(),
            "back\\slash".to_owned(),
            String::from("trailing\\"),
            "mix\n\\end".to_owned(),
        ] {
            assert_eq!(unescape_line(&escape_line(&text)), text);
        }
    }

    /// A harness that cannot run the worker at all is an audit fault, never a
    /// row outcome. With the row log's directory gone (the shape a full disk
    /// produces), spawning must fail with the I/O error rather than return a
    /// status the audit would file as a passing `RowProcessCrash` row.
    #[test]
    fn a_worker_the_harness_cannot_start_is_an_error_not_a_crash_row() {
        let case = inventory::real_package_cases()
            .next()
            .expect("the committed fleet manifest always selects at least one case");
        let artifacts = FixtureDir::new("unstartable-worker").expect("fixture directory");
        let missing = artifacts.child("removed");
        let result = spawn_row_worker_with_cap(
            case,
            &missing.join("row.outcome"),
            &missing.join("row.log"),
            &[],
            Duration::from_secs(1),
        );
        let error = result.expect_err("a row log that cannot be created must fail the harness");
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        assert!(
            error.to_string().contains("create its row log"),
            "the fault names what the harness could not do: {error}"
        );
    }

    /// A worker process killed by a signal must map to a typed
    /// `RowProcessCrash` carrying the exact signal, and an outcome file
    /// without an outcome record must parse as no disposition.
    #[test]
    fn lost_worker_process_becomes_a_typed_crash_row() {
        let mut suicide = Command::new("sh");
        suicide.arg("-c").arg("kill -9 $$");
        let status = suicide.status().expect("child status");
        let cause = row_crash_cause(&status);
        assert!(
            matches!(
                cause,
                AuthorityUnavailableCause::RowProcessCrash {
                    signal: 9,
                    code: -1
                }
            ),
            "a SIGKILLed worker must keep its signal: {cause:?}"
        );
        let mut unavailable = Vec::new();
        let mut mismatches = Vec::new();
        let case = inventory::real_package_cases()
            .next()
            .expect("a frozen case");
        assert!(
            parse_row_outcome("diag hello\n", case, &mut unavailable, &mut mismatches).is_none(),
            "a transcript without an outcome record is a lost row"
        );
        assert!(unavailable.is_empty() && mismatches.is_empty());
    }

    /// A full outcome transcript parses back into sink entries and exactly
    /// the disposition the worker recorded.
    #[test]
    fn outcome_transcript_parses_to_disposition_and_entries() {
        let case = inventory::real_package_cases()
            .next()
            .expect("a frozen case");
        let zeros = "0".repeat(64);
        let ones = "1".repeat(64);
        let transcript = format!(
            "diag real-setup lang=Go coord=\"golang:x\" error=Exit\n\
             mismatch real field=source-spans expected={zeros} observed={ones}\n\
             unavailable field=source-spans cause=observer-unavailable\n\
             outcome output verified=0 mismatches=1 unavailable=1 not_compared=2\n"
        );
        let mut unavailable = Vec::new();
        let mut mismatches = Vec::new();
        let disposition = parse_row_outcome(&transcript, case, &mut unavailable, &mut mismatches);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(unavailable.len(), 1);
        assert!(matches!(
            disposition,
            Some(RealCaseDisposition::Output {
                mismatches: 1,
                unavailable: 1,
                not_compared: 2,
                verified: false,
            })
        ));
    }

    /// The TypeScript report budgets are honest raises over the vendored
    /// defaults, and stay bounded by the audit's own per-row deadline.
    #[test]
    fn typescript_report_budgets_are_raised_bounded_and_documented() {
        let defaults = backend_frontend_typescript::legacy::Checker::default();
        assert!(
            TS_REPORT_OUTPUT_LIMIT > defaults.output_limit,
            "the report budget must exceed the vendored default {TS_REPORT_OUTPUT_LIMIT}"
        );
        assert_eq!(
            TS_REPORT_OUTPUT_LIMIT,
            64 * 1024 * 1024,
            "the documented report budget is 64 MiB"
        );
        assert!(
            TS_REPORT_DEADLINE > defaults.timeout,
            "the collect deadline must exceed the vendored default"
        );
        assert_eq!(
            TS_REPORT_DEADLINE,
            Duration::from_secs(120),
            "the documented collect budget is 120 s"
        );
        assert!(
            TS_REPORT_DEADLINE <= REAL_ROW_AUTHORITY_DEADLINE,
            "the collect deadline must stay within the audit's per-row authority deadline"
        );
    }

    /// Regression for the raised package staging bound: the frontend adapter's
    /// staging budget must admit the frozen corpus's largest legitimate
    /// package (`date-fns@4.1.0` ships 5326 files) instead of refusing it as
    /// `PackageFileLimit`. A package past the raised bound must still be
    /// refused, so the budget stays a bound. Staging completes before any
    /// checker child starts, so the admission case reaches either a real
    /// report or the typed child `Spawn` refusal — never `PackageFileLimit` —
    /// regardless of how this host resolves the checker program.
    #[test]
    fn typescript_package_staging_admits_corpus_scale_but_still_bounds() {
        let root = FixtureDir::new("ts-staging-budget").expect("fixture directory");
        fs::create_dir_all(root.child("lib")).expect("staging fixture directory");
        // 5400 files: above the old 4096 default and above date-fns' 5326,
        // below the raised 8192 bound.
        for index in 0..5400 {
            fs::write(
                root.child(&format!("lib/mod_{index}.d.ts")),
                format!("export declare const admitted_{index}: number;\n"),
            )
            .expect("staging fixture file");
        }
        fs::write(root.child("index.ts"), b"export const probe: number;\n").expect("entry file");
        let checker = backend_frontend_typescript::legacy::Checker::default();
        let admission = checker.run_in_package(
            TypeScriptSource::TypeScript,
            b"export const probe: number;\n",
            &root.path,
        );
        match admission {
            // Staging finished; the vendored TSZ checker child ran to a
            // report over the trivial entry.
            Ok(_) => {}
            // Staging finished; the child program itself could not start on
            // this host (no `NUDOX_TYPESCRIPT_CHECKER_BIN`, no `node`).
            Err(backend_frontend_typescript::legacy::CheckerError::Spawn { .. }) => {}
            Err(backend_frontend_typescript::legacy::CheckerError::PackageFileLimit {
                observed,
                limit,
            }) => panic!(
                "a corpus-scale package ({observed} files) must stage under the raised bound of {limit}"
            ),
            other => panic!("unexpected checker outcome before staging bound proof: {other:?}"),
        }
        // Past the raised bound the budget must still refuse staging.
        for index in 5400..8300 {
            fs::write(
                root.child(&format!("lib/mod_{index}.d.ts")),
                format!("export declare const bounded_{index}: number;\n"),
            )
            .expect("staging fixture file");
        }
        let bounded = checker.run_in_package(
            TypeScriptSource::TypeScript,
            b"export const probe: number;\n",
            &root.path,
        );
        assert!(
            matches!(
                bounded,
                Err(backend_frontend_typescript::legacy::CheckerError::PackageFileLimit {
                    observed,
                    limit: 8192,
                }) if observed > 8192
            ),
            "a package past the raised bound must stay a typed PackageFileLimit: {bounded:?}"
        );
    }

    /// The isolation mechanism survives a full row sequence: a worker that
    /// dies mid-row becomes one typed crash row, and the very next worker
    /// still produces its own real typed disposition — a native fault can
    /// neither abort the audit nor poison the rows after it.
    #[test]
    fn a_crashed_row_does_not_poison_the_next_row() {
        let selected = inventory::real_package_cases()
            .enumerate()
            .find(|(_, case)| case.coordinate.raw().contains("cfg-if"))
            .map(|(ordinal, case)| (ordinal, case));
        let Some((ordinal, case)) = selected else {
            eprintln!("cfg-if is not a manifest row; skipping the isolation repro");
            return;
        };
        let Some(inventory::RealPackageInput::Source { source, .. }) =
            inventory::real_package_input_at(ordinal)
        else {
            eprintln!("cfg-if source is not provisioned; skipping the isolation repro");
            return;
        };
        let artifacts = FixtureDir::new("isolation-sequence-test").expect("fixture directory");
        let mut first_unavailable = Vec::new();
        let mut first_mismatches = Vec::new();
        let crashed = drive_row_in_worker_with_env(
            case,
            &source,
            &artifacts,
            &mut first_unavailable,
            &mut first_mismatches,
            &[("NUDOX_ROW_WORKER_FAULT", "abort")],
        )
        .expect("the harness runs the worker");
        assert!(
            matches!(
                crashed,
                RealCaseDisposition::Unavailable(AuthorityUnavailableCause::RowProcessCrash {
                    signal: 6,
                    ..
                })
            ),
            "the first worker must die as a typed SIGABRT crash row: {crashed:?}"
        );
        let mut second_unavailable = Vec::new();
        let mut second_mismatches = Vec::new();
        let recovered = drive_row_in_worker(
            case,
            &source,
            &artifacts,
            &mut second_unavailable,
            &mut second_mismatches,
        )
        .expect("the harness runs the worker");
        assert!(
            !matches!(
                recovered,
                RealCaseDisposition::Unavailable(AuthorityUnavailableCause::RowProcessCrash { .. })
            ),
            "the row after a crash must produce its own real typed disposition: {recovered:?}"
        );
        assert!(
            !second_unavailable.iter().any(|entry| matches!(
                entry,
                CorpusMismatch::RealUnavailable {
                    cause: AuthorityUnavailableCause::RowProcessCrash { .. },
                    ..
                }
            )),
            "no crash residue may reach the second row's sinks"
        );
    }

    /// End-to-end isolation proof: a worker that dies mid-row with no typed
    /// outcome — exactly how the in-process libclang crash used to erase the
    /// run — becomes one typed `RowProcessCrash` row. Every other row keeps
    /// its own result because the worker process boundary is per row.
    #[test]
    fn crashed_row_worker_is_isolated_and_counted() {
        let selected = inventory::real_package_cases()
            .enumerate()
            .find(|(_, case)| case.coordinate.raw().contains("cfg-if"))
            .map(|(ordinal, case)| (ordinal, case));
        let Some((ordinal, case)) = selected else {
            eprintln!("cfg-if is not a manifest row; skipping the isolation repro");
            return;
        };
        let Some(inventory::RealPackageInput::Source { source, .. }) =
            inventory::real_package_input_at(ordinal)
        else {
            eprintln!("cfg-if source is not provisioned; skipping the isolation repro");
            return;
        };
        let artifacts = FixtureDir::new("isolation-test").expect("fixture directory");
        let mut unavailable = Vec::new();
        let mut mismatches = Vec::new();
        let disposition = drive_row_in_worker_with_env(
            case,
            &source,
            &artifacts,
            &mut unavailable,
            &mut mismatches,
            &[("NUDOX_ROW_WORKER_FAULT", "abort")],
        )
        .expect("the harness runs the worker");
        let crashed = matches!(
            disposition,
            RealCaseDisposition::Unavailable(AuthorityUnavailableCause::RowProcessCrash {
                signal: 6,
                ..
            })
        );
        assert!(
            crashed,
            "an aborted worker must become a typed SIGABRT crash row: {disposition:?}"
        );
        assert_eq!(unavailable.len(), 1, "the crash row stays counted");
        assert!(matches!(
            unavailable.first(),
            Some(CorpusMismatch::RealUnavailable {
                field: RealAuditField::Source,
                cause: AuthorityUnavailableCause::RowProcessCrash { .. },
                ..
            })
        ));
        assert!(mismatches.is_empty(), "a crash is never a parity mismatch");
    }

    /// The cap must reap the worker's whole process tree, not just the direct
    /// child: the worker leads its own process group, and a worker that forks
    /// a sleeping grandchild — the exact shape of every supervised native
    /// oracle/checker child — is reaped together with it when the hard cap
    /// fires. Without the group kill, the SIGKILLed worker's grandchildren
    /// are re-parented and leak because their in-worker teardown never runs.
    /// The lost row still parses as a typed `RowProcessCrash` carrying the
    /// cap signal, so the outcome-file crash-safety protocol is unchanged.
    #[cfg(unix)]
    #[test]
    fn a_capped_row_worker_reaps_its_native_grandchildren() {
        use rustix::process::Pid;

        let selected = inventory::real_package_cases()
            .enumerate()
            .find(|(_, case)| case.coordinate.raw().contains("cfg-if"))
            .map(|(ordinal, case)| (ordinal, case));
        let Some((ordinal, case)) = selected else {
            eprintln!("cfg-if is not a manifest row; skipping the cap repro");
            return;
        };
        let Some(inventory::RealPackageInput::Source { .. }) =
            inventory::real_package_input_at(ordinal)
        else {
            eprintln!("cfg-if source is not provisioned; skipping the cap repro");
            return;
        };
        let artifacts = FixtureDir::new("cap-grandchildren-test").expect("fixture directory");
        let pid_path = artifacts.child("grandchild.pid");
        let status = spawn_row_worker_with_cap(
            case,
            &artifacts.child(&format!("row-{}.outcome", case.ordinal)),
            &artifacts.child(&format!("row-{}.log", case.ordinal)),
            &[
                ("NUDOX_ROW_WORKER_FAULT", "fork-sleep"),
                (
                    "NUDOX_ROW_GRANDCHILD_PID_FILE",
                    &pid_path.as_os_str().to_string_lossy(),
                ),
            ],
            Duration::from_secs(4),
        );
        let cause = row_crash_cause(&status.expect("the harness runs the worker"));
        assert!(
            matches!(
                cause,
                AuthorityUnavailableCause::RowProcessCrash { signal: 9, .. }
            ),
            "the capped worker must die as a typed SIGKILL crash row: {cause:?}"
        );
        let recorded =
            fs::read_to_string(&pid_path).expect("the fault worker must record its grandchild");
        let grandchild = recorded.trim().parse::<i32>().expect("grandchild pid");
        assert!(grandchild > 0, "a real grandchild pid must be recorded");
        let grandchild = Pid::from_raw(grandchild).expect("grandchild pid");
        // The group kill reaps the grandchild immediately; poll a small
        // bounded window to absorb reaping latency, and fail if the orphan
        // is still alive (it sleeps 300 s, so survival is unambiguous).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if rustix::process::test_kill_process(grandchild).is_err() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the capped worker's sleeping grandchild must be reaped, not re-parented and leaked"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
