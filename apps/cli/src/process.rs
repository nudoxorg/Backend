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
/// Status can report an unsent cold-start observation while the candidate
/// finishes ordinary owner admission. Other commands retain their deadline.
#[cfg(unix)]
const STATUS_STARTUP_RESPONSE_BUDGET: Duration = Duration::from_secs(1);
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
    let startup_scope = backend_client::RequestContext::new();
    startup_scope
        .read_deadline(backend_runtime::COMMAND_STARTUP_TIMEOUT)
        .map_err(|error| {
            Fault::from_client_error(&error, Operand::Text("local startup".to_owned()))
        })?;
    let mut recovery = crate::owner_recovery::OwnerRecovery::new(
        &workspace,
        options.passive(),
        startup_scope.clone(),
    );
    let endpoint_result = if options.passive() {
        backend_runtime::connect_existing_locald(&workspace)
    } else {
        #[cfg(unix)]
        {
            let progress = recovery.progress_sender();
            match backend_runtime::start_locald_cancellable(
                &workspace,
                startup_scope
                    .remaining_read_budget()
                    .map_err(|error| {
                        Fault::from_client_error(&error, Operand::Text("local startup".to_owned()))
                    })?
                    .unwrap_or(STARTUP_COMMAND_BUDGET)
                    .min(startup_response_budget(&plan.request)),
                |phase| {
                    if let Some(progress) = &progress {
                        let _ = progress.try_send(format!("local startup: {}", phase.as_str()));
                    }
                },
                || startup_scope.check().is_ok(),
            ) {
                Ok(backend_runtime::LocaldStartup::Ready(endpoint)) => Ok(endpoint),
                Ok(backend_runtime::LocaldStartup::Pending(pending)) => {
                    let retry = pending_status_retry(&plan.request, &workspace);
                    return Ok((
                        render_startup_pending(&pending, options, retry.as_deref())?,
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
                if matches!(&plan.request, Request::Surface(command) if matches!(&**command,
                backend_library::SurfaceCommand::EmbeddingRefresh {
                    request: backend_library::EmbeddingRefreshAction::Observe { .. },
                })) {
                    passive_observe_replay(&plan.request, &workspace).unwrap_or(Affordance::Retry)
                } else {
                    Affordance::RecoverOwner {
                        context: OwnerContext::new(
                            project.clone(),
                            workspace.data().to_string_lossy().into_owned(),
                            workspace.endpoint().to_string_lossy().into_owned(),
                        ),
                    }
                }
            } else {
                startup_error_action(&error)
            },
        )
    })?;
    let mut session = recovery.connect_initial(&endpoint).map_err(|error| {
        Fault::from_client_error(
            &error,
            Operand::Path(endpoint.to_string_lossy().into_owned()),
        )
    })?;
    // Only an authenticated replacement can reconcile a saved start or repeat
    // an unpinned read. No outer Add invocation is replayed after disconnect.
    let (answer, outcome) =
        execute_plan_with_recovery(&mut session, &plan, &workspace, Some(&mut recovery))?;
    render_admitted_answer(&session, &answer, options).map(|output| (output, outcome))
}

#[cfg(unix)]
fn startup_response_budget(request: &Request) -> Duration {
    match request {
        Request::Status => STATUS_STARTUP_RESPONSE_BUDGET.min(STARTUP_COMMAND_BUDGET),
        _ => STARTUP_COMMAND_BUDGET,
    }
}

#[cfg(unix)]
fn pending_status_retry(
    request: &Request,
    workspace: &backend_runtime::WorkspacePaths,
) -> Option<String> {
    if !matches!(request, Request::Status) {
        return None;
    }
    crate::connection_replay::context(workspace)?.command_shell(
        &backend_present::ConnectionReplayCommand::new(
            backend_library::CommandId::Health,
            false,
            Vec::new(),
        ),
    )
}

fn passive_observe_replay(
    request: &Request,
    workspace: &backend_runtime::WorkspacePaths,
) -> Option<Affordance> {
    let Request::Surface(command) = request else {
        return None;
    };
    let backend_library::SurfaceCommand::EmbeddingRefresh {
        request: action @ backend_library::EmbeddingRefreshAction::Observe { .. },
    } = &**command
    else {
        return None;
    };
    Some(Affordance::ReplayConnection {
        context: crate::connection_replay::context(workspace)?,
        command: backend_present::ConnectionReplayCommand::new(
            backend_library::CommandId::EmbeddingRefresh,
            true,
            vec![serde_json::to_string(action).ok()?],
        ),
    })
}

fn startup_error_action(error: &backend_runtime::RuntimeError) -> Affordance {
    Affordance::startup(error.startup_recovery())
}

#[cfg(unix)]
fn render_startup_pending(
    pending: &backend_runtime::StartupPending,
    options: &Options,
    retry_command: Option<&str>,
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
            #[serde(skip_serializing_if = "Option::is_none")]
            retry_command: Option<&'a str>,
        }
        let observation = Pending {
            kind: "local-startup-pending",
            phase: pending.phase().as_str(),
            cause: pending.cause().as_str(),
            action: pending.action().as_str(),
            candidate_pid: pending.candidate_pid(),
            contender_exited: pending.contender_exited(),
            command_submitted: false,
            retry_command,
        };
        // This local CLI response is not a live engine DTO or operation receipt.
        // A status retry retains only the already-selected connection operands.
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
    let retry = retry_command.map_or_else(String::new, |command| format!("{command}\n"));
    Ok(format!(
        "local startup pending: {} ({candidate})\ncause: {}\naction: retry the original command; it has not been submitted\n{retry}",
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

#[cfg(all(test, unix))]
fn execute_plan(
    session: &mut Session,
    plan: &Plan,
    workspace: &backend_runtime::WorkspacePaths,
) -> Result<(Answer, ExitCode), Fault> {
    execute_plan_with_recovery(session, plan, workspace, None)
}

fn execute_plan_with_recovery(
    session: &mut Session,
    plan: &Plan,
    workspace: &backend_runtime::WorkspacePaths,
    mut recovery: Option<&mut crate::owner_recovery::OwnerRecovery>,
) -> Result<(Answer, ExitCode), Fault> {
    let answer = match recovery.as_deref_mut() {
        Some(recovery) => run::execute_with_recovery(session, &plan.request, workspace, recovery)?,
        None => run::execute_in_workspace(session, &plan.request, workspace)?,
    };
    if let Some(recovery) = recovery.as_deref() {
        recovery.detach(session).map_err(|error| {
            Fault::from_client_error(&error, Operand::Text("owner replacement".to_owned()))
        })?;
    }
    let (answer, override_exit) = match plan.wait {
        Some(policy) => match recovery.as_deref_mut() {
            Some(recovery) => crate::index_wait::finish_with_recovery(
                session,
                workspace,
                answer,
                policy,
                Some(recovery),
            )?,
            None => crate::index_wait::finish(session, workspace, answer, policy)?,
        },
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
    let mut invocation = invoke::parse(words, options.limit())?;
    if invocation.grammar().name() == "embedding-refresh" && invocation.at(0).is_none() {
        let paths = workspace(options)?;
        let path = paths.data().join("compiler").join("embedding.config");
        invocation.push(installed_embedding_refresh_request(&path)?);
    }
    let wait = crate::index_wait::IndexWaitPolicy::from_invocation(&invocation)?;
    lower(&invocation, project).map(|request| Plan { request, wait })
}

fn installed_embedding_refresh_request(path: &std::path::Path) -> Result<String, Fault> {
    // This bounded CLI capture proves only persisted bytes. The owner performs its own
    // original-deadline capture, quiescence, activation and commit; no owner is started here.
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let deadline = std::time::Instant::now()
        .checked_add(backend_runtime::COMMAND_STARTUP_TIMEOUT)
        .ok_or_else(|| {
            Fault::usage(
                "embedding-refresh",
                "configuration capture deadline cannot be represented",
            )
        })?;
    let control = backend_compile::EmbeddingActivationControl::new(&cancelled, deadline);
    let prepared = backend_engine::application::PreparedEmbeddingRuntime::capture_until(path, &control)
        .map_err(|error| Fault::new(FaultSlug::Rejected, Operand::Argument("embedding-refresh".to_owned()), Cause::new(CauseSlug::Refused, error.to_string()), Affordance::None))?
        .ok_or_else(|| Fault::usage("embedding-refresh", "No installed local embedding configuration exists; run nudox embedding-setup --help and install the matched artifacts first."))?;
    serde_json::to_string(&backend_library::EmbeddingRefreshAction::Start {
        configuration: prepared.identity().as_bytes(),
        timeout_seconds: u16::try_from(backend_runtime::COMMAND_STARTUP_TIMEOUT.as_secs())
            .map_err(|_| {
                Fault::usage(
                    "embedding-refresh",
                    "default operation budget cannot be represented",
                )
            })?,
    })
    .map_err(|error| {
        Fault::usage(
            "embedding-refresh",
            format!("configuration request encoding failed: {error}"),
        )
    })
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

    #[test]
    fn status_help_exposes_passive_observation_before_workspace_startup() {
        for name in ["status", "health"] {
            let help = help_output(&[name.to_owned(), "--help".to_owned()]).expect("status help");
            assert!(help.contains("nudox --passive status"));
            assert!(help.contains("unsent startup observation"));
        }
    }
}

#[cfg(all(test, unix))]
mod embedding_refresh_tests {
    use super::*;
    use backend_engine::application::{
        EmbeddingRequirement, EmbeddingRuntimeInstall, EmbeddingRuntimeLimits,
        install_embedding_runtime,
    };
    use std::{
        num::{NonZeroU16, NonZeroU32},
        os::unix::fs::PermissionsExt,
    };

    #[test]
    fn cli_refresh_captures_installed_bytes_without_activation_and_uses_shared_grammar()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root =
            std::path::PathBuf::from("/tmp").join(format!("ncr-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root)?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        struct Owned(std::path::PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _owned = Owned(root.clone());
        let root = root.canonicalize()?;
        let helper = root.join("capture-fixture.sh");
        let marker = root.join("activation-marker");
        std::fs::write(
            &helper,
            b"#!/bin/sh\nprintf unexpected > \"$FIXTURE_ACTIVATION_MARKER\"\nexit 77\n",
        )?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        let compiler = root.join("compiler");
        backend_platform::durable::ensure_private_directory(&compiler)?;
        let path = compiler.join("embedding.config");
        install_embedding_runtime(
            &path,
            EmbeddingRuntimeInstall {
                requirement: EmbeddingRequirement::Optional,
                program: helper,
                arguments: Vec::new(),
                dependencies: Vec::new(),
                environment: vec![(
                    "FIXTURE_ACTIVATION_MARKER".to_owned(),
                    marker.to_str().ok_or("fixture UTF-8 path")?.to_owned(),
                )],
                limits: EmbeddingRuntimeLimits {
                    stdout_bytes: 128 * 1024,
                    stderr_bytes: 1024,
                    output_bytes: 129 * 1024,
                    input_bytes: 1024 * 1024,
                    wall_time_ms: 5000,
                    workspace_bytes: 1024 * 1024,
                    process_count: 8,
                    memory_bytes: None,
                    cpu_time_ms: None,
                    resident_credit_bytes: 128 * 1024 * 1024,
                },
                model_bytes: b"capture-only fixture, not a genuine model".to_vec(),
                tokenizer_bytes: b"capture-only fixture tokenizer".to_vec(),
                dimension: NonZeroU16::new(2).ok_or("dimension")?,
                normalization: backend_compile::EmbeddingNormalization::L2,
                maximum_text_bytes: NonZeroU32::new(4096).ok_or("text")?,
                options_digest: [9; 32],
            },
        )?;
        let text = installed_embedding_refresh_request(&path)
            .map_err(|fault| format!("capture: {fault:?}"))?;
        assert!(
            !marker.exists(),
            "CLI capture must never invoke the helper or claim activation"
        );
        let request: backend_library::EmbeddingRefreshAction = serde_json::from_str(&text)?;
        let backend_library::EmbeddingRefreshAction::Start {
            configuration,
            timeout_seconds,
        } = request
        else {
            return Err("CLI capture did not produce a start observation".into());
        };
        let flag = std::sync::atomic::AtomicBool::new(false);
        let control = backend_compile::EmbeddingActivationControl::new(
            &flag,
            std::time::Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(
            configuration,
            backend_engine::application::PreparedEmbeddingRuntime::capture_until(&path, &control)?
                .ok_or("installed config")?
                .identity()
                .as_bytes()
        );
        assert_eq!(
            u64::from(timeout_seconds),
            backend_runtime::COMMAND_STARTUP_TIMEOUT.as_secs()
        );
        let invocation = invoke::parse(&["embedding-refresh".to_owned(), text.clone()], None)
            .map_err(|fault| format!("parse: {fault:?}"))?;
        let Request::Surface(command) = lower(&invocation, "/unchanged/caller/project")
            .map_err(|fault| format!("lower: {fault:?}"))?
        else {
            return Err("shared lowering lost refresh action".into());
        };
        assert_eq!(
            *command,
            backend_library::SurfaceCommand::EmbeddingRefresh {
                request: serde_json::from_str(&text)?
            }
        );
        let arguments = vec![
            "--project".to_owned(),
            root.to_str().ok_or("project UTF-8")?.to_owned(),
            "--workspace".to_owned(),
            root.to_str().ok_or("workspace UTF-8")?.to_owned(),
            "--endpoint".to_owned(),
            root.join("owner.sock")
                .to_str()
                .ok_or("endpoint UTF-8")?
                .to_owned(),
            "embedding-refresh".to_owned(),
        ];
        let (options, words) =
            options::split(&arguments).map_err(|fault| format!("options: {fault:?}"))?;
        let planned = plan(&words, &options, root.to_str().ok_or("project UTF-8")?)
            .map_err(|fault| format!("plan: {fault:?}"))?;
        assert!(
            planned.wait.is_none(),
            "refresh never starts an index wait policy"
        );
        let Request::Surface(planned) = planned.request else {
            return Err("normal CLI plan lost refresh".into());
        };
        assert_eq!(
            *planned, *command,
            "omitted action captures the exact current config through the registered command"
        );
        assert!(
            !marker.exists(),
            "normal planning performs no activation or owner startup"
        );
        let missing = installed_embedding_refresh_request(&root.join("missing.config"))
            .expect_err("missing config is not acceptance");
        assert!(format!("{missing:?}").contains("embedding-setup --help"));
        assert!(!marker.exists());
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod startup_tests {
    use super::*;

    #[test]
    fn cold_status_response_horizon_does_not_change_command_or_passive_semantics() {
        for words in [vec!["status"], vec!["health"]] {
            let words = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let plan =
                plan(&words, &Options::fallback(), "/repo").expect("registered command plan");
            assert!(matches!(plan.request, Request::Status));
            assert_eq!(
                startup_response_budget(&plan.request),
                Duration::from_secs(1)
            );
            assert!(plan.wait.is_none());
        }
        for words in [vec!["add", "."], vec!["search", "symbols"]] {
            let words = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let plan =
                plan(&words, &Options::fallback(), "/repo").expect("registered command plan");
            assert_eq!(
                startup_response_budget(&plan.request),
                STARTUP_COMMAND_BUDGET
            );
        }
        let (options, words) = options::split(&["--passive".to_owned(), "status".to_owned()])
            .expect("passive options");
        assert!(options.passive(), "passive remains the connect-only branch");
        assert!(matches!(
            plan(&words, &options, "/repo")
                .expect("passive status plan")
                .request,
            Request::Status
        ));
    }

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
        let json = render_startup_pending(&pending, &Options::plain(options::Format::Json), None)
            .expect("startup response");
        let value: serde_json::Value = serde_json::from_str(&json).expect("typed JSON");
        assert_eq!(value["kind"], "local-startup-pending");
        assert_eq!(value["phase"], "starting");
        assert_eq!(value["cause"], "response-budget");
        assert_eq!(value["action"], "retry-original-command");
        assert_eq!(value["command_submitted"], false);
        assert!(value["candidate_pid"].is_null());
        assert_eq!(value["contender_exited"], false);
        assert!(value.get("retry_command").is_none());
        assert!(value.get("operation").is_none());
        assert!(value.get("readiness").is_none());
        let text =
            render_startup_pending(&pending, &Options::fallback(), None).expect("human pending");
        assert!(text.contains("no candidate was started"));
        assert!(text.contains("it has not been submitted"));
        assert!(!workspace.data().exists());
        let retry = pending_status_retry(&Request::Status, &workspace);
        let value: serde_json::Value = serde_json::from_str(
            &render_startup_pending(
                &pending,
                &Options::plain(options::Format::Json),
                retry.as_deref(),
            )
            .expect("pending retry response"),
        )
        .expect("pending retry JSON");
        if let Some(retry) = retry {
            assert_eq!(value["retry_command"], retry);
        } else {
            assert!(value.get("retry_command").is_none());
        }
        assert_eq!(value["command_submitted"], false);
        assert!(value.get("readiness").is_none());
        assert!(
            !workspace.data().exists(),
            "rendering a retry never prepares state"
        );
        let error = backend_runtime::RuntimeError::StartupPending(pending);
        assert_eq!(
            error.startup_recovery(),
            backend_client::StartupRecovery::RetryOriginalCommand
        );
        let fault =
            Fault::from_client_error(&backend_client::ClientError::from(error), Operand::Whole);
        assert_eq!(fault.affordance(), &Affordance::Retry);
        assert!(
            fault
                .cause()
                .sentence()
                .contains("it has not been submitted")
        );
    }

    #[test]
    fn pending_status_retry_never_projects_lossy_paths_or_replays_mutations() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let root = std::env::temp_dir().join(format!("ncsr-{}", std::process::id()));
        let workspace = backend_runtime::WorkspacePaths::discover(
            Some(root.join("project with spaces")),
            Some(root.join("state'$(sentinel)")),
            Some(root.join("owner.sock")),
        )
        .expect("read-only workspace discovery");
        if let Some(retry) = pending_status_retry(&Request::Status, &workspace) {
            assert!(retry.contains("--project '"));
            assert!(retry.contains("--workspace '"));
            assert!(retry.contains("'\\''$(sentinel)"));
            assert!(retry.ends_with(" health"));
        }
        assert!(pending_status_retry(&Request::Index(".".to_owned()), &workspace).is_none());
        let lossless = backend_runtime::WorkspacePaths::discover(
            Some(root.join(OsString::from_vec(vec![b'p', 0xff]))),
            Some(root.join("state")),
            Some(root.join("owner.sock")),
        )
        .expect("read-only workspace discovery");
        assert!(pending_status_retry(&Request::Status, &lossless).is_none());
        assert!(!root.exists());
    }

    // Separate test-process discovery avoids global environment mutation and
    // exercises the same option split and runtime selection as the CLI.
    #[test]
    fn startup_recovery_discovery_probe() {
        let Ok(encoded) = std::env::var("NUDOX_TEST_RECOVERY_ARGV") else {
            return;
        };
        let words: Vec<String> = serde_json::from_str(&encoded).expect("probe arguments");
        let (options, words) = options::split(&words).expect("actual recovery option split");
        let paths = workspace(&options).expect("actual read-only recovery discovery");
        let planned = plan(
            &words,
            &options,
            paths.project().to_str().expect("probe project"),
        )
        .expect("actual read-only recovery command plan")
        .request;
        let replay_action = std::env::var_os("NUDOX_TEST_REPLAY_ACTION").is_some();
        if !replay_action {
            assert!(matches!(planned, Request::Status));
        }
        let (id, observation) = match &planned {
            Request::Status => (
                backend_library::CommandId::Health,
                serde_json::json!("status"),
            ),
            Request::Index(path) => (
                backend_library::CommandId::Add,
                serde_json::json!({ "add": path }),
            ),
            Request::Surface(command) => {
                assert!(matches!(
                    &**command,
                    backend_library::SurfaceCommand::EmbeddingRefresh { .. }
                ));
                (
                    backend_library::CommandId::EmbeddingRefresh,
                    serde_json::to_value(&**command).expect("exact refresh request"),
                )
            }
            _ => panic!("unexpected read-only replay probe action"),
        };
        let rendered = if replay_action {
            crate::connection_replay::context(&paths).and_then(|context| {
                context.command_shell(&backend_present::ConnectionReplayCommand::new(
                    id,
                    options.passive(),
                    words[1..].to_vec(),
                ))
            })
        } else {
            pending_status_retry(&Request::Status, &paths)
        };
        let selection = paths.connection_replay_selection().expect("Unix selection");
        let (boundary, anchor) = match selection.workspace {
            backend_runtime::ConnectionWorkspaceSelection::Explicit(_) => ("explicit", None),
            backend_runtime::ConnectionWorkspaceSelection::Home(anchor) => ("home", Some(anchor)),
            backend_runtime::ConnectionWorkspaceSelection::XdgState(anchor) => {
                ("xdg-state", Some(anchor))
            }
            backend_runtime::ConnectionWorkspaceSelection::XdgData(anchor) => {
                ("xdg-data", Some(anchor))
            }
        };
        let value = serde_json::json!({
            "project": paths.project().to_str(),
            "data": paths.data().to_str(),
            "endpoint": paths.endpoint().to_str(),
            "authority": paths.authority_secret().to_str(),
            "boundary": boundary,
            "anchor": anchor.and_then(std::path::Path::to_str),
            "retry": rendered,
            "planned": observation,
            "passive": options.passive(),
        });
        let mut value = value;
        if std::env::var_os("NUDOX_TEST_NO_OWNER_OBSERVE").is_some() {
            let fault = run_words(&words, &options).expect_err("passive Observe has no owner");
            assert_eq!(fault.slug(), FaultSlug::Endpoint);
            assert_eq!(fault.cause().slug(), CauseSlug::Unreachable);
            assert!(fault.cause().sentence().contains("without starting it"));
            value["fault"] = serde_json::to_value(backend_present::FaultDto::new(&fault))
                .expect("actual no-owner fault projection");
            value["fault_human"] = render::fault(&fault, &Options::plain(options::Format::Human));
            value["fault_markdown"] =
                render::fault(&fault, &Options::plain(options::Format::Markdown));
        }
        println!("\nRECOVERY_SELECTION={value}");
    }

    fn recovery_observation(command: &mut std::process::Command) -> serde_json::Value {
        let output = command.output().expect("owned read-only discovery probe");
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let stdout = std::str::from_utf8(&output.stdout).expect("probe UTF-8");
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix("RECOVERY_SELECTION="))
            .expect("actual discovery observation");
        serde_json::from_str(line).expect("selection JSON")
    }

    fn replay_startup_selection(executable: &std::path::Path, shell: &str) -> serde_json::Value {
        use std::process::Command;
        const PROBE: &str = "process::startup_tests::startup_recovery_discovery_probe";
        const UNSET: &str = "__UNSET__";
        let script = format!(
            r#"nudox() {{ printf '%s\000' "$@" __ENV__ "${{HOME-__UNSET__}}" "${{XDG_STATE_HOME-__UNSET__}}" "${{XDG_DATA_HOME-__UNSET__}}" "${{BACKEND_LOCALD_WORKSPACE-__UNSET__}}" "${{BACKEND_LOCALD_AUTHORITY_SECRET_FILE-__UNSET__}}"; }}; {shell}"#
        );
        let parsed = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("HOME", "/wrong-home")
            .env("XDG_STATE_HOME", "/wrong-xdg-state")
            .env("XDG_DATA_HOME", "/wrong-xdg-data")
            .env(backend_runtime::DATA_ENV, "/wrong-workspace")
            .env(backend_runtime::AUTHORITY_SECRET_ENV, "/wrong-authority")
            .output()
            .expect("actual POSIX argv and environment parsing");
        assert!(parsed.status.success(), "{parsed:?}");
        assert!(parsed.stderr.is_empty(), "{parsed:?}");
        let values = parsed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| {
                std::str::from_utf8(bytes)
                    .expect("exact UTF-8 operand")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let split = values
            .iter()
            .position(|value| value == "__ENV__")
            .expect("env boundary");
        assert_eq!(values.len(), split + 6);
        let mut replay = Command::new(executable);
        replay
            .args(["--exact", PROBE, "--nocapture"])
            .env("NUDOX_TEST_REPLAY_ACTION", "1")
            .env(
                "NUDOX_TEST_RECOVERY_ARGV",
                serde_json::to_string(&values[..split]).expect("actual parsed argv"),
            );
        for (name, value) in [
            "HOME",
            "XDG_STATE_HOME",
            "XDG_DATA_HOME",
            backend_runtime::DATA_ENV,
            backend_runtime::AUTHORITY_SECRET_ENV,
        ]
        .into_iter()
        .zip(&values[split + 1..])
        {
            if value == UNSET {
                replay.env_remove(name);
            } else {
                replay.env(name, value);
            }
        }
        recovery_observation(&mut replay)
    }

    #[test]
    fn pending_status_retry_replays_actual_discovery_with_authority_and_default_boundary() {
        use std::process::Command;
        const PROBE: &str = "process::startup_tests::startup_recovery_discovery_probe";
        let executable = std::env::current_exe().expect("current test executable");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!("nrri-{}-{nonce}", std::process::id()));
        let project = root.join("project ' $(sentinel)");
        let endpoint = root.join("owner.sock");
        let state = root.join("explicit `sentinel`; state");
        let home = root.join("home ' $(sentinel)");
        let xdg = root.join("xdg ' $(sentinel)");
        let authority = root.join("authority ' $(sentinel)");
        #[cfg(target_os = "macos")]
        let origins = ["explicit", "environment", "home"];
        #[cfg(not(target_os = "macos"))]
        let origins = ["explicit", "environment", "home", "xdg-state", "xdg-data"];
        for origin in origins {
            for custom_authority in [false, true] {
                for action in ["health", "status", "observe", "start", "add"] {
                    let mut argv = vec![
                        "--project".to_owned(),
                        project.to_str().expect("project").to_owned(),
                        "--endpoint".to_owned(),
                        endpoint.to_str().expect("endpoint").to_owned(),
                    ];
                    if origin == "explicit" {
                        argv.extend([
                            "--workspace".to_owned(),
                            state.to_str().expect("state").to_owned(),
                        ]);
                    }
                    let ticket = backend_library::EmbeddingRefreshTicket {
                        sequence: std::num::NonZeroU64::new(1).expect("sequence"),
                        owner_epoch: 19,
                        owner_fence: [0x51; 32],
                        configuration: [0x73; 32],
                    };
                    match action {
                        "health" | "status" => argv.push(action.to_owned()),
                        "observe" => {
                            argv.extend([
                                "--passive".to_owned(),
                                "embedding-refresh".to_owned(),
                                serde_json::to_string(
                                    &backend_library::EmbeddingRefreshAction::Observe { ticket },
                                )
                                .expect("exact Observe"),
                            ]);
                        }
                        "start" => argv.extend([
                            "embedding-refresh".to_owned(),
                            serde_json::to_string(
                                &backend_library::EmbeddingRefreshAction::Start {
                                    configuration: ticket.configuration,
                                    timeout_seconds: 30,
                                },
                            )
                            .expect("exact Start"),
                        ]),
                        "add" => argv.extend([
                            "add".to_owned(),
                            project.to_str().expect("project").to_owned(),
                        ]),
                        _ => unreachable!(),
                    }
                    let mut original = Command::new(&executable);
                    original
                        .args(["--exact", PROBE, "--nocapture"])
                        .env(
                            "NUDOX_TEST_RECOVERY_ARGV",
                            serde_json::to_string(&argv).expect("original argv"),
                        )
                        .env("NUDOX_TEST_REPLAY_ACTION", "1")
                        .env("HOME", &home)
                        .env_remove("XDG_STATE_HOME")
                        .env_remove("XDG_DATA_HOME")
                        .env_remove(backend_runtime::DATA_ENV)
                        .env_remove(backend_runtime::AUTHORITY_SECRET_ENV);
                    match origin {
                        "environment" => {
                            original.env(backend_runtime::DATA_ENV, &state);
                        }
                        "xdg-state" => {
                            original.env("XDG_STATE_HOME", &xdg);
                        }
                        "xdg-data" => {
                            original.env("XDG_DATA_HOME", &xdg);
                        }
                        _ => {}
                    }
                    if custom_authority {
                        original.env(backend_runtime::AUTHORITY_SECRET_ENV, &authority);
                    }
                    let before = recovery_observation(&mut original);
                    assert_eq!(
                        before["boundary"],
                        if origin == "environment" {
                            "explicit"
                        } else {
                            origin
                        }
                    );
                    let shell = before["retry"].as_str().expect("complete captured retry");
                    assert_eq!(
                        before,
                        replay_startup_selection(&executable, shell),
                        "{origin}/{custom_authority}/{action}"
                    );
                }
            }
        }
        assert!(
            !root.exists(),
            "discovery and shell parsing never prepare state"
        );
    }

    #[test]
    fn incomplete_authority_or_default_anchor_keeps_only_original_command_retry() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        use std::process::Command;
        const PROBE: &str = "process::startup_tests::startup_recovery_discovery_probe";
        let executable = std::env::current_exe().expect("test executable");
        let argv = serde_json::to_string(&[
            "--project",
            "/tmp/nrri-incomplete-project",
            "--endpoint",
            "/tmp/nrri-incomplete.sock",
            "--workspace",
            "/tmp/nrri-incomplete-state",
            "status",
        ])
        .expect("explicit arguments");
        for authority in [
            OsString::from("relative authority ' $(sentinel)"),
            OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]),
        ] {
            let mut command = Command::new(&executable);
            command
                .args(["--exact", PROBE, "--nocapture"])
                .env("NUDOX_TEST_RECOVERY_ARGV", &argv)
                .env(backend_runtime::AUTHORITY_SECRET_ENV, authority);
            let value = recovery_observation(&mut command);
            assert_eq!(value["boundary"], "explicit");
            assert!(
                value["retry"].is_null(),
                "no lossy or CWD-guessed credential"
            );
        }
        let argv = serde_json::to_string(&[
            "--project",
            "/tmp/nrri-incomplete-project",
            "--endpoint",
            "/tmp/nrri-incomplete.sock",
            "status",
        ])
        .expect("default arguments");
        for home in [
            OsString::from("relative home"),
            OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]),
        ] {
            let mut command = Command::new(&executable);
            command
                .args(["--exact", PROBE, "--nocapture"])
                .env("NUDOX_TEST_RECOVERY_ARGV", &argv)
                .env("HOME", home)
                .env_remove("XDG_STATE_HOME")
                .env_remove("XDG_DATA_HOME")
                .env_remove(backend_runtime::DATA_ENV)
                .env_remove(backend_runtime::AUTHORITY_SECRET_ENV);
            let value = recovery_observation(&mut command);
            assert_eq!(value["boundary"], "home");
            assert!(
                value["retry"].is_null(),
                "no lossy or CWD-guessed default anchor"
            );
        }
    }

    #[test]
    fn refresh_and_add_replay_suppress_incomplete_authority_without_guessing() {
        use std::process::Command;
        let executable = std::env::current_exe().expect("test executable");
        let ticket = backend_library::EmbeddingRefreshTicket {
            sequence: std::num::NonZeroU64::new(1).expect("sequence"),
            owner_epoch: 7,
            owner_fence: [0x51; 32],
            configuration: [0x73; 32],
        };
        for action in [
            vec![
                "--passive".to_owned(),
                "embedding-refresh".to_owned(),
                serde_json::to_string(&backend_library::EmbeddingRefreshAction::Observe {
                    ticket: ticket.clone(),
                })
                .expect("Observe"),
            ],
            vec![
                "embedding-refresh".to_owned(),
                serde_json::to_string(&backend_library::EmbeddingRefreshAction::Start {
                    configuration: ticket.configuration,
                    timeout_seconds: 30,
                })
                .expect("Start"),
            ],
            vec!["add".to_owned(), "/tmp/nrri-incomplete-project".to_owned()],
        ] {
            let mut argv = vec![
                "--project".to_owned(),
                "/tmp/nrri-incomplete-project".to_owned(),
                "--workspace".to_owned(),
                "/tmp/nrri-incomplete-state".to_owned(),
                "--endpoint".to_owned(),
                "/tmp/nrri-incomplete.sock".to_owned(),
            ];
            argv.extend(action);
            let mut command = Command::new(&executable);
            command
                .args([
                    "--exact",
                    "process::startup_tests::startup_recovery_discovery_probe",
                    "--nocapture",
                ])
                .env(
                    "NUDOX_TEST_RECOVERY_ARGV",
                    serde_json::to_string(&argv).expect("argv"),
                )
                .env("NUDOX_TEST_REPLAY_ACTION", "1")
                .env(backend_runtime::AUTHORITY_SECRET_ENV, "relative authority");
            let value = recovery_observation(&mut command);
            assert_eq!(value["authority"], "relative authority");
            assert_eq!(value["boundary"], "explicit");
            assert!(value["retry"].is_null());
            assert!(
                !value["planned"].is_null(),
                "typed caller action is retained without a fabricated connection command"
            );
        }
    }

    #[test]
    fn passive_observe_no_owner_front_door_preserves_exact_replay_or_suppresses_command() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _, process::Command};
        let executable = std::env::current_exe().expect("test executable");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!("nopf-{}-{nonce}", std::process::id()));
        let ticket = backend_library::EmbeddingRefreshTicket {
            sequence: std::num::NonZeroU64::new(7).expect("sequence"),
            owner_epoch: 29,
            owner_fence: [0x51; 32],
            configuration: [0x73; 32],
        };
        let action =
            serde_json::to_string(&backend_library::EmbeddingRefreshAction::Observe { ticket })
                .expect("Observe");
        for explicit in [true, false] {
            for authority in [
                None,
                Some(root.join("authority ' $(sentinel)").into_os_string()),
                Some(OsString::from("relative authority")),
                Some(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff])),
            ] {
                let mut argv = vec![
                    "--project".to_owned(),
                    root.join("project ' $(sentinel)")
                        .to_str()
                        .expect("project")
                        .to_owned(),
                    "--endpoint".to_owned(),
                    root.join("owner.sock").to_str().expect("socket").to_owned(),
                ];
                if explicit {
                    argv.extend([
                        "--workspace".to_owned(),
                        root.join("state ' $(sentinel)")
                            .to_str()
                            .expect("state")
                            .to_owned(),
                    ]);
                }
                argv.extend([
                    "--passive".to_owned(),
                    "embedding-refresh".to_owned(),
                    action.clone(),
                ]);
                let mut original = Command::new(&executable);
                original
                    .args([
                        "--exact",
                        "process::startup_tests::startup_recovery_discovery_probe",
                        "--nocapture",
                    ])
                    .env(
                        "NUDOX_TEST_RECOVERY_ARGV",
                        serde_json::to_string(&argv).expect("argv"),
                    )
                    .env("NUDOX_TEST_REPLAY_ACTION", "1")
                    .env("NUDOX_TEST_NO_OWNER_OBSERVE", "1")
                    .env("HOME", root.join("home ' $(sentinel)"))
                    .env_remove("XDG_STATE_HOME")
                    .env_remove("XDG_DATA_HOME")
                    .env_remove(backend_runtime::DATA_ENV)
                    .env_remove(backend_runtime::AUTHORITY_SECRET_ENV);
                if let Some(authority) = authority {
                    original.env(backend_runtime::AUTHORITY_SECRET_ENV, authority);
                }
                let mut before = recovery_observation(&mut original);
                assert_eq!(before["passive"], true);
                assert_eq!(
                    before["boundary"],
                    if explicit { "explicit" } else { "home" }
                );
                let fault = before
                    .as_object_mut()
                    .expect("observation")
                    .remove("fault")
                    .expect("actual fault");
                let human = before
                    .as_object_mut()
                    .expect("observation")
                    .remove("fault_human")
                    .expect("human");
                let markdown = before
                    .as_object_mut()
                    .expect("observation")
                    .remove("fault_markdown")
                    .expect("Markdown");
                assert!(
                    fault["call"].is_null(),
                    "CLI selection is not a fabricated MCP scope"
                );
                if let Some(shell) = before["retry"].as_str() {
                    assert_eq!(fault["shell"], shell);
                    assert!(human.as_str().expect("human").contains(shell));
                    assert!(markdown.as_str().expect("Markdown").contains(shell));
                    assert_eq!(before, replay_startup_selection(&executable, shell));
                } else {
                    assert_eq!(fault["shell"], "re-run the same command");
                    assert!(!human.as_str().expect("human").contains("--workspace"));
                    assert!(!markdown.as_str().expect("Markdown").contains("--workspace"));
                    assert!(
                        !human
                            .as_str()
                            .expect("human")
                            .contains("BACKEND_LOCALD_AUTHORITY_SECRET_FILE=")
                    );
                    assert!(
                        !markdown
                            .as_str()
                            .expect("Markdown")
                            .contains("BACKEND_LOCALD_AUTHORITY_SECRET_FILE=")
                    );
                }
                assert!(
                    !root.exists(),
                    "passive no-owner fault never starts or prepares an owner"
                );
            }
        }
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
