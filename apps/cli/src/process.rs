//! CLI process startup, endpoint composition, and readiness progress.
//!
//! The whole process is four steps, in this order and no other: split the
//! global options, parse the command against the shared registry, execute it,
//! render it. Every step fails with the same typed [`Fault`], so a failure in
//! any of them reaches the reader through the same three-line grammar and the
//! same exit code table.
//!
//! `index` is the one command whose answer is "not yet". When someone is
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
    match run_words(&words, &options) {
        Ok(output) => emit(&output),
        Err(fault) => report(&fault, &options),
    }
}

fn early_exit(args: &[String]) -> Option<ExitCode> {
    if args.is_empty() || args.iter().any(|word| word == "--help" || word == "-h") {
        let topic = args
            .iter()
            .find(|word| !word.starts_with('-'))
            .and_then(|word| grammar_for(word));
        println!("{}", topic.map_or_else(invoke::help, invoke::help_for));
        return Some(ExitCode::SUCCESS);
    }
    if args.iter().any(|word| word == "--version" || word == "-V") {
        println!("backend {}", env!("CARGO_PKG_VERSION"));
        return Some(ExitCode::SUCCESS);
    }
    None
}

fn run_words(words: &[String], options: &Options) -> Result<String, Fault> {
    let workspace = workspace(options)?;
    let project = workspace.project().to_string_lossy().into_owned();
    let request = plan(words, options, &project)?;
    let endpoint = backend_runtime::ensure_locald(&workspace).map_err(|error| {
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path(workspace.endpoint().to_string_lossy().into_owned()),
            Cause::new(
                CauseSlug::Unreachable,
                format!("the local daemon could not be started or reached: {error}"),
            ),
            Affordance::Retry,
        )
    })?;
    let mut session = Session::connect(&endpoint).map_err(|error| {
        Fault::from_client_error(
            &error,
            Operand::Path(endpoint.to_string_lossy().into_owned()),
        )
    })?;
    let answer = run::execute(&mut session, &request)?;
    if let Request::Index(path) = &request {
        watch_readiness(&mut session, path, options);
    }
    Ok(render::answer(&answer, options))
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
