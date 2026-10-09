//! CLI process startup, endpoint composition, and readiness progress.
//!
//! The whole process is four steps, in this order and no other: split the
//! global options, parse the command against the shared registry, execute it,
//! render it. Every step fails with the same typed [`Fault`], so a failure in
//! any of them reaches the reader through the same three-line grammar and the
//! same exit code table.
//!
//! Startup can return a typed pending observation before any command is sent.
//! `index` also reports readiness progress. When someone is
//! watching, it rewrites a single readiness line in place until the lanes
//! agree, so the honest answer — this takes time — is visible rather than
//! implied by silence.

use crate::invoke::{self, SURFACE_VERB};
use crate::options::{self, Options};
use crate::render;
use crate::run::{self, Answer};
use backend_client::Session;
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand, grammar_for};
use backend_present::{Request, lower, lower_surface_json};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long `index` watches readiness before handing the reader the line.
const PROGRESS_LIMIT: Duration = Duration::from_secs(20);
/// One command invocation waits for the shared owner under the runtime's
/// existing bounded cold-start deadline. Progress is reported on stderr; if
/// the deadline expires, the original command remains explicitly unsent.
#[cfg(unix)]
const STARTUP_COMMAND_BUDGET: Duration = backend_runtime::COMMAND_STARTUP_TIMEOUT;
/// EX_TEMPFAIL: the bounded owner wait ended before a command was submitted.
#[cfg(unix)]
const EXIT_STARTUP_PENDING: u8 = 75;

/// Runs the CLI process against the configured daemon endpoint.
#[must_use]
pub fn main_entry() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(early) = early_exit(&args) {
        return early;
    }
    let (options, words) = match options::split(&args) {
        Ok(split) => split,
        Err(fault) => return report(&fault, &Options::fallback()),
    };
    if words.first().is_some_and(|word| word == "cluster") {
        return match crate::cluster::run(&words[1..], &options) {
            Ok(output) => emit(&output),
            Err(fault) => report(&fault, &options),
        };
    }
    if words.first().is_some_and(|word| word == "semantic-hydrate") {
        return match crate::semantic_hydrate::run(&words[1..], &options) {
            Ok(output) => emit(&output),
            Err(fault) => report(&fault, &options),
        };
    }
    match run_words(&words, &options) {
        Ok((output, outcome)) => {
            let written = emit(&output);
            if written == ExitCode::SUCCESS { outcome } else { written }
        }
        Err(fault) => report(&fault, &options),
    }
}

fn early_exit(args: &[String]) -> Option<ExitCode> {
    if args.first().is_some_and(|word| word == "semantic-hydrate")
        && args
            .iter()
            .any(|word| matches!(word.as_str(), "--help" | "-h"))
    {
        println!("{}", crate::semantic_hydrate::help_text());
        return Some(ExitCode::SUCCESS);
    }
    if args.is_empty() || args.iter().any(|word| word == "--help" || word == "-h") {
        let topic = args
            .iter()
            .find(|word| !word.starts_with('-'))
            .and_then(|word| grammar_for(word));
        let is_cluster_command = options::split(args)
            .ok()
            .is_some_and(|(_, words)| words.first().is_some_and(|word| word == "cluster"));
        if is_cluster_command {
            println!("{}", crate::cluster::help_text());
        } else {
            println!("{}", topic.map_or_else(invoke::help, invoke::help_for));
        }
        return Some(ExitCode::SUCCESS);
    }
    if args.iter().any(|word| word == "--version" || word == "-V") {
        println!("backend {}", env!("CARGO_PKG_VERSION"));
        return Some(ExitCode::SUCCESS);
    }
    None
}

fn run_words(words: &[String], options: &Options) -> Result<(String, ExitCode), Fault> {
    let workspace = workspace(options)?;
    let project = workspace.project().to_string_lossy().into_owned();
    let request = plan(words, options, &project)?;
    let endpoint_result = if options.passive() {
        backend_runtime::connect_existing_locald(&workspace)
    } else {
        #[cfg(unix)]
        {
            match backend_runtime::start_locald(
                &workspace,
                STARTUP_COMMAND_BUDGET,
                startup_progress,
            ) {
                Ok(backend_runtime::LocaldStartup::Ready(endpoint)) => Ok(endpoint),
                Ok(backend_runtime::LocaldStartup::Pending(pending)) => {
                    return Ok((
                        render_startup_pending(&pending, options)?,
                        ExitCode::from(EXIT_STARTUP_PENDING),
                    ));
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(unix))]
        {
            // The existing Windows durable bootstrap is retained. The bounded
            // child/pipe guarantee above is Unix-only, not inferred for Windows.
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "local startup: starting (this platform uses the existing synchronous setup)"
            ).and_then(|()| stderr.flush());
            backend_runtime::ensure_locald(&workspace)
        }
    };
    let endpoint = endpoint_result.map_err(|error| {
        let message = if options.passive() {
            format!("the existing local owner could not be reached without starting it: {error}")
        } else {
            format!("the local daemon could not be started or reached: {error}")
        };
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path(workspace.endpoint().to_string_lossy().into_owned()),
            Cause::new(CauseSlug::Unreachable, message),
            if options.passive() {
                Affordance::UseCommand {
                    name: "health",
                    args: Box::new([
                        "--project".to_owned(),
                        project.clone(),
                        "--workspace".to_owned(),
                        workspace.data().to_string_lossy().into_owned(),
                        "--endpoint".to_owned(),
                        workspace.endpoint().to_string_lossy().into_owned(),
                    ]),
                }
            } else {
                startup_error_action(&error)
            },
        )
    })?;
    let mut session = Session::connect(&endpoint).map_err(|error| {
        Fault::from_client_error(
            &error,
            Operand::Path(endpoint.to_string_lossy().into_owned()),
        )
    })?;
    // Startup is a single bounded wait; the planned command is submitted once,
    // only after an endpoint connection succeeded.
    let answer = run::execute(&mut session, &request)?;
    if let Request::Index(path) = &request {
        watch_readiness(&mut session, path, options);
    }
    render_admitted_answer(&session, &answer, options)
        .map(|output| (output, render::answer_exit_code(&answer)))
}

#[cfg(unix)]
fn startup_progress(phase: backend_runtime::StartupPhase) {
    // Separate stderr progress preserves machine stdout and is flushed before
    // waiting on the child's durable setup. It never describes indexing.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "local startup: {}", phase.as_str()).and_then(|()| stderr.flush());
}

fn startup_error_action(error: &backend_runtime::RuntimeError) -> Affordance {
    #[cfg(unix)]
    if matches!(
        error,
        backend_runtime::RuntimeError::CompanionProtocolMismatch
    ) {
        // The closed runtime cause includes the matched install/build action.
        // Retrying an incompatible executable pair cannot repair its grammar.
        return Affordance::None;
    }
    let _ = error;
    Affordance::Retry
}

#[cfg(unix)]
fn render_startup_pending(
    pending: &backend_runtime::StartupPending,
    options: &Options,
) -> Result<String, Fault> {
    if options.is_machine() {
        #[derive(serde::Serialize)]
        struct Pending<'a> {
            kind: &'static str,
            phase: &'a str,
            cause: &'a str,
            action: &'a str,
            candidate_pid: Option<u32>,
            contender_exited: bool,
            command_submitted: bool,
        }
        let observation = Pending {
            kind: "local-startup-pending",
            phase: pending.phase().as_str(),
            cause: pending.cause().as_str(),
            action: pending.action().as_str(),
            candidate_pid: pending.candidate_pid(),
            contender_exited: pending.contender_exited(),
            command_submitted: false,
        };
        // Only fixed strings, booleans and a numeric observation are serialized.
        // This local CLI response is not a live engine DTO or operation receipt.
        return serde_json::to_string(&observation)
            .map(|json| format!("{json}\n"))
            .map_err(|error| {
                Fault::usage(
                    "startup response",
                    format!("cannot encode startup observation: {error}"),
                )
            });
    }
    let candidate = pending.candidate_pid().map_or_else(
        || "no candidate was started".to_owned(),
        |pid| format!("candidate {pid}; ownership is not established by this observation"),
    );
    Ok(format!(
        "local startup pending: {} ({candidate})\ncause: {}\naction: retry the original command; it has not been submitted\n",
        pending.phase().as_str(),
        pending.cause().as_str()
    ))
}

pub(crate) fn render_admitted_answer(
    session: &Session,
    answer: &Answer,
    options: &Options,
) -> Result<String, Fault> {
    let cursor = answer
        .continuation()
        .filter(|continuation| session.has_portable_query_continuation(*continuation))
        .map(|continuation| session.encode_query_continuation(continuation))
        .transpose()
        .map_err(|error| {
            Fault::from_client_error(&error, Operand::Argument("cursor".to_owned()))
        })?;
    render::try_answer_with_cursor(answer, options, cursor.as_deref())
}

fn plan(words: &[String], options: &Options, project: &str) -> Result<Request, Fault> {
    if words.first().is_some_and(|word| word == SURFACE_VERB) {
        let encoded = words.get(1).ok_or_else(|| {
            Fault::usage(
                SURFACE_VERB,
                "surface takes exactly one tagged SurfaceCommand JSON object",
            )
        })?;
        return lower_surface_json(encoded);
    }
    let invocation = invoke::parse(words, options.limit())?;
    lower(&invocation, project)
}

fn workspace(options: &Options) -> Result<backend_runtime::WorkspacePaths, Fault> {
    backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| {
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path(options.project().map_or_else(
                || ".".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )),
            Cause::new(
                CauseSlug::Unreachable,
                format!("the workspace could not be opened: {error}"),
            ),
            Affordance::None,
        )
    })
}

/// Rewrites one readiness line while an index request settles.
fn watch_readiness(session: &mut Session, path: &str, options: &Options) {
    if options.is_machine() || !options::stderr_is_terminal() {
        return;
    }
    let deadline = Instant::now() + PROGRESS_LIMIT;
    let mut stderr = std::io::stderr().lock();
    let name = PathBuf::from(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    while Instant::now() < deadline {
        let Ok(report) = session.health() else {
            break;
        };
        let status = backend_present::Status::from_report(&report, None);
        let line = format!("\r\u{1b}[2K{} {}", name, status.coverage().render());
        if stderr
            .write_all(line.as_bytes())
            .and_then(|()| stderr.flush())
            .is_err()
        {
            return;
        }
        if status.readiness() == "ready" {
            break;
        }
        std::thread::yield_now();
    }
    let _ = stderr.write_all(b"\r\x1b[2K");
    let _ = stderr.flush();
}

fn emit(output: &str) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::from(render::EXIT_IO);
    }
    ExitCode::SUCCESS
}

fn report(fault: &Fault, options: &Options) -> ExitCode {
    let rendered = render::fault(fault, options);
    if options.is_machine() {
        let _ = std::io::stdout().lock().write_all(rendered.as_bytes());
    } else {
        let _ = writeln!(std::io::stderr().lock(), "{rendered}");
    }
    render::exit_code(fault)
}

/// Answers one command against an already connected session.
///
/// This is the seam the journeys and the in-process tests use: it skips
/// daemon discovery and nothing else, so what it renders is what the process
/// renders.
///
/// # Errors
///
/// Returns the typed fault the grammar, the engine, or the transport produced.
pub fn answer_with_session(
    session: &mut Session,
    words: &[String],
    options: &Options,
) -> Result<Answer, Fault> {
    let project = options
        .project()
        .map(backend_runtime::normalize_surface_path)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(backend_runtime::normalize_surface_path)
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .to_string_lossy()
        .into_owned();
    let request = plan(words, options, &project)?;
    run::execute(session, &request)
}

#[cfg(all(test, unix))]
mod startup_tests {
    use super::*;

    #[test]
    fn pending_json_is_a_startup_observation_without_an_accepted_command() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!("ncs-{}-{nonce}", std::process::id()));
        let workspace = backend_runtime::WorkspacePaths::discover(
            Some(std::env::current_dir().expect("cwd")),
            Some(root.join("state")),
            Some(root.join("owner.sock")),
        )
        .expect("uncreated explicit workspace");
        let backend_runtime::LocaldStartup::Pending(pending) =
            backend_runtime::start_locald(&workspace, Duration::ZERO, |_| {
                panic!("zero-budget response must not start a candidate")
            })
            .expect("pending observation")
        else {
            panic!("no endpoint exists");
        };
        let json = render_startup_pending(&pending, &Options::plain(options::Format::Json))
            .expect("startup response");
        let value: serde_json::Value = serde_json::from_str(&json).expect("typed JSON");
        assert_eq!(value["kind"], "local-startup-pending");
        assert_eq!(value["phase"], "starting");
        assert_eq!(value["cause"], "response-budget");
        assert_eq!(value["action"], "retry-original-command");
        assert_eq!(value["command_submitted"], false);
        assert!(value["candidate_pid"].is_null());
        assert_eq!(value["contender_exited"], false);
        assert!(value.get("operation").is_none());
        assert!(value.get("readiness").is_none());
        let text = render_startup_pending(&pending, &Options::fallback()).expect("human pending");
        assert!(text.contains("no candidate was started"));
        assert!(text.contains("it has not been submitted"));
        assert!(!workspace.data().exists());
    }

    #[test]
    fn mismatched_companion_action_is_install_or_rebuild_instead_of_retry() {
        let error = backend_runtime::RuntimeError::CompanionProtocolMismatch;
        assert!(matches!(startup_error_action(&error), Affordance::None));
        assert!(
            error
                .to_string()
                .contains("install matched CLI and locald or rebuild")
        );
    }
}
