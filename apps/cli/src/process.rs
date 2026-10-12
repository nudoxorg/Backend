//! CLI process startup, endpoint composition, and readiness progress.
//!
//! The whole process is four steps, in this order and no other: split the
//! global options, parse the command against the shared registry, execute it,
//! render it. Every step fails with the same typed [`Fault`], so a failure in
//! any of them reaches the reader through the same three-line grammar and the
//! same exit code table.
//!
//! Startup can return a typed pending observation before any command is sent.
//! `index` returns the owner's exact acceptance and progress identity. The
//! client holds it for its exact terminal receipt only with explicit --wait;
//! global retrieval readiness is a separate observation.

use crate::invoke::{self, SURFACE_VERB};
use crate::options::{self, Options};
use crate::render;
use crate::run::{self, Answer};
use backend_client::Session;
use backend_present::{
    Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand, OwnerContext, grammar_for,
};
use backend_present::{Request, lower, lower_surface_json};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

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
    if words.first().is_some_and(|word| word == "embedding-setup") {
        return match crate::embedding_setup::run(&words[1..], &options) {
            Ok(output) => emit(&output),
            Err(fault) => report(&fault, &options),
        };
    }
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
            if written == ExitCode::SUCCESS {
                outcome
            } else {
                written
            }
        }
        Err(fault) => report(&fault, &options),
    }
}

fn early_exit(args: &[String]) -> Option<ExitCode> {
    if let Some(help) = help_output(args) {
        println!("{help}");
        return Some(ExitCode::SUCCESS);
    }
    if args.iter().any(|word| word == "--version" || word == "-V") {
        println!("backend {}", env!("CARGO_PKG_VERSION"));
        return Some(ExitCode::SUCCESS);
    }
    None
}

fn help_output(args: &[String]) -> Option<String> {
    if !args.is_empty()
        && !args
            .iter()
            .any(|word| matches!(word.as_str(), "--help" | "-h"))
    {
        return None;
    }
    // Global option values are paths or projection names, not help topics.
    // Reuse the normal option split before selecting the command, while help
    // still exits before workspace discovery or daemon startup.
    let arguments = args
        .iter()
        .filter(|word| !matches!(word.as_str(), "--help" | "-h"))
        .cloned()
        .collect::<Vec<_>>();
    let command = options::split(&arguments)
        .ok()
        .and_then(|(_, words)| words.into_iter().next());
    Some(match command.as_deref() {
        Some("cluster") => crate::cluster::help_text().to_owned(),
        Some("semantic-hydrate") => crate::semantic_hydrate::help_text().to_owned(),
        Some("embedding-setup") => crate::embedding_setup::help_text().to_owned(),
        command => command
            .and_then(grammar_for)
            .map_or_else(invoke::help, invoke::help_for),
    })
}

fn run_words(words: &[String], options: &Options) -> Result<(String, ExitCode), Fault> {
    let workspace = workspace(options)?;
    let project = workspace.project().to_string_lossy().into_owned();
    let plan = plan(words, options, &project)?;
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
            )
            .and_then(|()| stderr.flush());
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
            if options.passive()
                && error.startup_recovery() == backend_client::StartupRecovery::RetryOriginalCommand
            {
                Affordance::RecoverOwner {
                    context: OwnerContext::new(
                        project.clone(),
                        workspace.data().to_string_lossy().into_owned(),
                        workspace.endpoint().to_string_lossy().into_owned(),
                    ),
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
    session.observe_query_preparation(|progress| {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "search preparation: {}", progress.state.as_str())
            .and_then(|()| stderr.flush());
        true
    });
    // Startup is a single bounded wait; the planned command is submitted once,
    // only after an endpoint connection succeeded.
    let (answer, outcome) = execute_plan(&mut session, &plan, &workspace)?;
    render_admitted_answer(&session, &answer, options).map(|output| (output, outcome))
}

#[cfg(unix)]
fn startup_progress(phase: backend_runtime::StartupPhase) {
    // Separate stderr progress preserves machine stdout and is flushed before
    // waiting on the child's durable setup. It never describes indexing.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "local startup: {}", phase.as_str()).and_then(|()| stderr.flush());
}

fn startup_error_action(error: &backend_runtime::RuntimeError) -> Affordance {
    Affordance::startup(error.startup_recovery())
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

struct Plan {
    request: Request,
    wait: Option<crate::index_wait::IndexWaitPolicy>,
}

#[cfg(all(test, unix))]
#[path = "tests/index_wait.rs"]
mod index_wait_tests;

fn execute_plan(
    session: &mut Session,
    plan: &Plan,
    workspace: &backend_runtime::WorkspacePaths,
) -> Result<(Answer, ExitCode), Fault> {
    let answer = run::execute_in_workspace(session, &plan.request, workspace)?;
    let (answer, override_exit) = match plan.wait {
        Some(policy) => crate::index_wait::finish(session, workspace, answer, policy)?,
        None => (answer, None),
    };
    let exit = override_exit.map_or_else(|| render::answer_exit_code(&answer), ExitCode::from);
    Ok((answer, exit))
}

fn plan(words: &[String], options: &Options, project: &str) -> Result<Plan, Fault> {
    if words.first().is_some_and(|word| word == SURFACE_VERB) {
        let encoded = words.get(1).ok_or_else(|| {
            Fault::usage(
                SURFACE_VERB,
                "surface takes exactly one tagged SurfaceCommand JSON object",
            )
        })?;
        return lower_surface_json(encoded).map(|request| Plan {
            request,
            wait: None,
        });
    }
    let invocation = invoke::parse(words, options.limit())?;
    let wait = crate::index_wait::IndexWaitPolicy::from_invocation(&invocation)?;
    lower(&invocation, project).map(|request| Plan { request, wait })
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
    let plan = plan(words, options, &project)?;
    if plan.wait.is_some() {
        return Err(Fault::usage(
            "--wait",
            "this session-only API has no durable caller journal; use the workspace CLI route to wait for Add",
        ));
    }
    run::execute(session, &plan.request)
}

#[cfg(test)]
mod help_tests {
    use super::help_output;

    #[test]
    fn command_help_skips_global_option_values() {
        for (arguments, command) in [
            (vec!["--project", "/repo", "add", "--help"], "add"),
            (vec!["--detail", "full", "graph", "--help"], "graph"),
            (vec!["--format", "json", "search", "--help"], "search"),
            (
                vec![
                    "--workspace",
                    "/state",
                    "--endpoint",
                    "/socket",
                    "resolve",
                    "-h",
                ],
                "resolve",
            ),
            (vec!["--help", "--project", "/repo", "add"], "add"),
            (vec!["--help", "--passive", "health"], "health"),
            (vec!["--help", "--limit", "3", "search"], "search"),
        ] {
            let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let help = help_output(&arguments).expect("help requested");
            let prefix = format!("nudox {command}");
            assert!(help.starts_with(&prefix), "{help}");
            assert!(
                matches!(help.as_bytes().get(prefix.len()), Some(b' ' | b'\n')),
                "{help}"
            );
            assert!(!help.contains("quick start"), "expected command help");
        }
    }

    #[test]
    fn special_command_help_works_after_global_options() {
        for (command, expected) in [
            ("cluster", crate::cluster::help_text()),
            ("semantic-hydrate", crate::semantic_hydrate::help_text()),
        ] {
            let arguments = ["--project", "/repo", command, "--help"].map(str::to_owned);
            assert_eq!(help_output(&arguments).as_deref(), Some(expected));
        }
        assert!(help_output(&[]).expect("bare help").contains("quick start"));
        assert!(help_output(&["add".to_owned(), ".".to_owned()]).is_none());
    }
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
        let error = backend_runtime::RuntimeError::StartupPending(pending);
        assert_eq!(
            error.startup_recovery(),
            backend_client::StartupRecovery::RetryOriginalCommand
        );
        let fault = Fault::from_client_error(
            &backend_client::ClientError::from(error),
            Operand::Whole,
        );
        assert_eq!(fault.affordance(), &Affordance::Retry);
        assert!(fault.cause().sentence().contains("it has not been submitted"));
    }

    #[test]
    fn startup_policy_matches_client_faults_and_all_cli_renderers() {
        use backend_runtime::RuntimeError;
        use std::io::{Error, ErrorKind};
        let cases = [
            (
                RuntimeError::MissingExecutable("/debug/deps/backend-locald".into()),
                Affordance::None,
            ),
            (RuntimeError::CompanionProtocolMismatch, Affordance::None),
            (
                RuntimeError::InvalidPath("endpoint path is empty; ConnectionRefused; retry"),
                Affordance::None,
            ),
            (
                RuntimeError::InvalidCredential("/state/authority.secret".into()),
                Affordance::None,
            ),
            (
                RuntimeError::DefaultStateInitialization {
                    path: "/profile/state".into(),
                    source: Error::from(ErrorKind::PermissionDenied),
                },
                Affordance::None,
            ),
            (
                RuntimeError::Spawn {
                    executable: "/bin/backend-locald".into(),
                    source: Error::from(ErrorKind::PermissionDenied),
                },
                Affordance::None,
            ),
            (
                RuntimeError::Io(Error::new(
                    ErrorKind::PermissionDenied,
                    "private state parent /state refused",
                )),
                Affordance::None,
            ),
            (
                RuntimeError::Io(Error::from(ErrorKind::ConnectionRefused)),
                Affordance::Retry,
            ),
            (
                RuntimeError::Io(Error::from(ErrorKind::NotFound)),
                Affordance::None,
            ),
            (
                RuntimeError::EndpointUnavailable {
                    endpoint: "/socket".into(),
                    source: Error::from(ErrorKind::NotFound),
                },
                Affordance::Retry,
            ),
            (
                RuntimeError::EndpointUnavailable {
                    endpoint: "/socket".into(),
                    source: Error::from(ErrorKind::ConnectionRefused),
                },
                Affordance::Retry,
            ),
            (
                RuntimeError::StartTimeout("/socket".into()),
                Affordance::Retry,
            ),
            (
                RuntimeError::DaemonExited {
                    code: Some(70),
                    diagnostic: None,
                },
                Affordance::None,
            ),
            (
                RuntimeError::DaemonExited {
                    code: Some(i32::from(backend_runtime::OWNER_CONTENDED_EXIT_CODE)),
                    diagnostic: None,
                },
                Affordance::Retry,
            ),
        ];
        for (error, expected) in cases {
            let original = error.to_string();
            assert_eq!(startup_error_action(&error), expected, "{original}");
            let fault = Fault::from_client_error(
                &backend_client::ClientError::from(error),
                Operand::Path("/socket".into()),
            );
            assert_eq!(fault.affordance(), &expected);
            assert_eq!(
                fault.cause().sentence(),
                format!("the local daemon could not be started or reached: {original}")
            );
            let json = render::fault(&fault, &Options::plain(options::Format::Json));
            let dto: serde_json::Value = serde_json::from_str(&json).expect("actual fault JSON");
            assert_eq!(dto["answer"], "fault");
            assert_eq!(dto["slug"], "endpoint");
            assert_eq!(dto["operand"], "/socket");
            assert_eq!(dto["cause"], "unreachable");
            assert_eq!(dto["detail"], fault.cause().sentence());
            for projection in [
                backend_present::Detail::Summary,
                backend_present::Detail::Standard,
                backend_present::Detail::Full,
            ] {
                let payload = backend_present::encode_serializable(
                    "fault",
                    projection,
                    None,
                    backend_present::FaultDto::new(&fault),
                    backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
                )
                .expect("bounded typed fault envelope");
                let envelope: serde_json::Value =
                    serde_json::from_slice(&payload.bytes).expect("encoded fault envelope");
                assert_eq!(envelope["detail"], dto["detail"]);
                assert_eq!(envelope["responseDetail"], projection.name());
                for field in ["slug", "operand", "cause", "shell", "call"] {
                    assert_eq!(envelope[field], dto[field]);
                }
            }
            assert!(dto.get("call").is_none());
            let text = render::fault(&fault, &Options::fallback());
            let markdown = render::fault(&fault, &Options::plain(options::Format::Markdown));
            assert_eq!(
                text,
                backend_present::text::fault(&fault, backend_present::Theme::plain())
            );
            assert_eq!(markdown, backend_present::markdown::fault(&fault));
            if expected == Affordance::Retry {
                assert_eq!(dto["shell"], "re-run the same command");
                assert!(text.contains("→ re-run the same command"));
                // Markdown offers only actionable tool calls; Retry has no invented tool call.
                assert!(!markdown.contains("→"));
            } else {
                assert!(dto.get("shell").is_none());
                assert!(!text.contains("→"));
                assert!(!markdown.contains("→"));
            }
        }
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
