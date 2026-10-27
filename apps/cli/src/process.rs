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
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand, grammar_for};
use backend_present::{Request, lower, lower_surface_json};
use std::io::Write as _;
use std::path::{Path, PathBuf};
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
    if words.first().is_some_and(|word| word == "auth") {
        return match crate::auth::run(&words[1..], &options) {
            Ok(output) => emit(&output),
            Err(fault) => report(&fault, &options),
        };
    }
    // Setup output stays available signed out; every product command needs
    // the account in a build that requires one.
    if words.first().is_none_or(|word| word != "mcp-config")
        && let Err(fault) = crate::auth::require_signed_in()
    {
        return report(&fault, &options);
    }
    if words.first().is_some_and(|word| word == "mcp-config") {
        return match crate::mcp_config::run(&words[1..], &options) {
            Ok(output) => emit(&output),
            Err(fault) => report(&fault, &options),
        };
    }
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
        Some("mcp-config") => crate::mcp_config::help_text().to_owned(),
        Some("auth") => crate::auth::help_text().to_owned(),
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
                    .unwrap_or(STARTUP_COMMAND_BUDGET),
                |phase| {
                    if let Some(progress) = &progress {
                        let _ = progress.try_send(format!("local startup: {}", phase.as_str()));
                    }
                },
                || startup_scope.check().is_ok(),
            ) {
                Ok(backend_runtime::LocaldStartup::Ready(endpoint)) => Ok(endpoint),
                Ok(backend_runtime::LocaldStartup::Pending(pending)) => {
                    let retry = pending_status_retry(
                        &plan.request,
                        &workspace,
                        original_status_command(words),
                    );
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
                #[cfg(unix)]
                {
                    std::env::current_exe()
                        .ok()
                        .and_then(|executable| checked_cli_executable(&executable))
                        .and_then(|executable| {
                            startup_recovery_context(
                                &plan.request,
                                &workspace,
                                original_status_command(words),
                                backend_present::StartupRecoveryMode::Existing,
                                &executable,
                            )
                        })
                        .map_or(Affordance::Retry, |context| Affordance::RecoverStartup {
                            context,
                        })
                }
                #[cfg(not(unix))]
                {
                    Affordance::Retry
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
    #[cfg(unix)]
    let answer = match answer {
        Answer::Product(view) => {
            let context = std::env::current_exe()
                .ok()
                .and_then(|executable| checked_cli_executable(&executable))
                .and_then(|executable| {
                    startup_recovery_context(
                        &Request::Status,
                        &workspace,
                        backend_present::StartupRecoveryCommand::Status,
                        connection_mode(options),
                        &executable,
                    )
                });
            Answer::Product(Box::new((*view).with_cli_context(context)))
        }
        answer => answer,
    };
    render_admitted_answer(&session, &answer, options).map(|output| (output, outcome))
}

#[cfg(unix)]
fn original_status_command(words: &[String]) -> backend_present::StartupRecoveryCommand {
    if words.first().is_some_and(|word| word == "health") {
        backend_present::StartupRecoveryCommand::Health
    } else {
        backend_present::StartupRecoveryCommand::Status
    }
}

fn connection_mode(options: &Options) -> backend_present::StartupRecoveryMode {
    if options.passive() {
        backend_present::StartupRecoveryMode::Existing
    } else {
        backend_present::StartupRecoveryMode::Active
    }
}

pub(crate) fn observed_product_cli_context(
    workspace: &backend_runtime::WorkspacePaths,
    mode: backend_present::StartupRecoveryMode,
) -> Option<backend_present::StartupRecoveryContext> {
    #[cfg(unix)]
    {
        let executable = checked_cli_executable(&std::env::current_exe().ok()?)?;
        startup_recovery_context(
            &Request::Status,
            workspace,
            backend_present::StartupRecoveryCommand::Status,
            mode,
            &executable,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (workspace, mode);
        None
    }
}

#[cfg(unix)]
fn checked_cli_executable(path: &std::path::Path) -> Option<PathBuf> {
    backend_runtime::observed_cli_executable(path)
}

#[cfg(unix)]
fn pending_status_retry(
    request: &Request,
    workspace: &backend_runtime::WorkspacePaths,
    command: backend_present::StartupRecoveryCommand,
) -> Option<String> {
    let executable = checked_cli_executable(&std::env::current_exe().ok()?)?;
    pending_status_retry_for_executable(
        request,
        workspace,
        command,
        backend_present::StartupRecoveryMode::Active,
        &executable,
    )
}

#[cfg(unix)]
fn pending_status_retry_for_executable(
    request: &Request,
    workspace: &backend_runtime::WorkspacePaths,
    command: backend_present::StartupRecoveryCommand,
    mode: backend_present::StartupRecoveryMode,
    executable: &std::path::Path,
) -> Option<String> {
    startup_recovery_context(request, workspace, command, mode, executable)
        .map(|context| context.shell())
}

#[cfg(unix)]
fn startup_recovery_context(
    request: &Request,
    workspace: &backend_runtime::WorkspacePaths,
    command: backend_present::StartupRecoveryCommand,
    mode: backend_present::StartupRecoveryMode,
    executable: &std::path::Path,
) -> Option<backend_present::StartupRecoveryContext> {
    fn operand(path: &std::path::Path) -> Option<String> {
        let text = path.to_str()?;
        (path.is_absolute() && !text.contains('\0')).then(|| text.to_owned())
    }
    if !matches!(request, Request::Status) {
        return None;
    }
    let selected = workspace.startup_recovery_selection()?;
    // Do not display a custom authority path or silently choose another key.
    // The original command remains retryable in its configured environment.
    if !selected.workspace_authority {
        return None;
    }
    operand(selected.data)?;
    let directory = match selected.workspace {
        backend_runtime::StartupWorkspaceSelection::Explicit(path) => {
            backend_present::StartupRecoveryWorkspace::Explicit(operand(path)?)
        }
        backend_runtime::StartupWorkspaceSelection::Home(anchor) => {
            backend_present::StartupRecoveryWorkspace::Home(operand(anchor)?)
        }
        backend_runtime::StartupWorkspaceSelection::XdgState(anchor) => {
            backend_present::StartupRecoveryWorkspace::XdgState(operand(anchor)?)
        }
        backend_runtime::StartupWorkspaceSelection::XdgData(anchor) => {
            backend_present::StartupRecoveryWorkspace::XdgData(operand(anchor)?)
        }
    };
    Some(backend_present::StartupRecoveryContext::new(
        operand(selected.project)?,
        operand(selected.endpoint)?,
        operand(executable)?,
        command,
        mode,
        directory,
    ))
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
        "local startup pending: {} ({candidate})\ncause: {}\naction: retry the original command in its configured environment; it has not been submitted\n{retry}",
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
    output: Options,
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
    let cli_context = observed_product_cli_context(workspace, connection_mode(&plan.output));
    let answer = run::execute_with_output_admission(
        session,
        &plan.request,
        Some(workspace),
        recovery.as_deref_mut(),
        plan.output.detail(),
        cli_context.as_ref(),
        |answer, cursor| render::admit_answer_with_cursor(answer, &plan.output, cursor),
    )?;
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
            output: options.clone(),
        });
    }
    let invocation = invoke::parse(words, options.limit())?;
    let wait = crate::index_wait::IndexWaitPolicy::from_invocation(&invocation)?;
    let request = lower(&invocation, project)?;
    let request = normalize_package_profile_path(request, project)?;
    Ok(Plan {
        request,
        wait,
        output: options.clone(),
    })
}

// A local directory passed to `add` is canonicalized by the owner before it
// becomes a Project label. Resolve explicit relative path operands for the CLI
// package-profile command against the same selected workspace root so its
// lookup uses that exact local identity. PURLs and bare local labels stay
// untouched, and the JSON surface escape hatch above keeps protocol operands
// exact.
fn normalize_package_profile_path(request: Request, project: &str) -> Result<Request, Fault> {
    let command = match request {
        Request::Surface(command) => command,
        other => return Ok(other),
    };
    let (label, compiler_outcome) = match *command {
        backend_library::SurfaceCommand::PackageProfile {
            package: backend_library::PackageReference::Local(label),
            compiler_outcome,
        } => (label, compiler_outcome),
        other => return Ok(Request::Surface(Box::new(other))),
    };
    let value = label.as_str();
    let is_dot_path =
        value == "." || value.starts_with("./") || (cfg!(windows) && value.starts_with(".\\"));
    let is_parent_path =
        value == ".." || value.starts_with("../") || (cfg!(windows) && value.starts_with("..\\"));
    let is_relative_path = !Path::new(value).is_absolute() && (is_dot_path || is_parent_path);
    if !is_relative_path {
        return Ok(Request::Surface(Box::new(
            backend_library::SurfaceCommand::PackageProfile {
                package: backend_library::PackageReference::Local(label),
                compiler_outcome,
            },
        )));
    }
    let path = backend_runtime::normalize_surface_path(Path::new(project).join(value));
    let resolved = path.to_str().ok_or_else(|| {
        Fault::admission(
            backend_library::ProductAdmissionError::PackageReference,
            Operand::Argument(value.to_owned()),
        )
    })?;
    let package = backend_library::PackageReference::from_kind(
        backend_library::PackageReferenceKind::Local,
        resolved.to_owned(),
    )
    .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))?;
    let command = backend_library::SurfaceCommand::PackageProfile {
        package,
        compiler_outcome,
    };
    command
        .admit()
        .map_err(|error| Fault::admission(error, Operand::Argument(value.to_owned())))?;
    Ok(Request::Surface(Box::new(command)))
}

#[cfg(test)]
mod package_profile_path_tests {
    use super::*;
    use backend_library::{PackageCompilerOutcomeRead, PackageReference, SurfaceCommand};

    fn package_profile(request: Request) -> (PackageReference, Option<PackageCompilerOutcomeRead>) {
        let Request::Surface(command) = request else {
            panic!("package-profile lowers to a surface request");
        };
        let SurfaceCommand::PackageProfile {
            package,
            compiler_outcome,
        } = *command
        else {
            panic!("package-profile command");
        };
        (package, compiler_outcome)
    }

    fn planned_package(
        words: &[&str],
        project: &str,
    ) -> (PackageReference, Option<PackageCompilerOutcomeRead>) {
        let words = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let plan = plan(&words, &Options::fallback(), project).expect("package-profile plan");
        package_profile(plan.request)
    }

    fn canonical_surface_path(path: &Path) -> PathBuf {
        let canonical = std::fs::canonicalize(path).expect("canonical filesystem path");
        backend_runtime::normalize_surface_path(canonical)
    }

    #[test]
    fn relative_local_package_profile_paths_resolve_to_selected_project_identity() {
        let temp = tempfile::tempdir().expect("temporary workspace");
        let project_path = temp.path().join("project");
        let nested_path = project_path.join("nested/project");
        let sibling_path = temp.path().join("sibling");
        std::fs::create_dir_all(&nested_path).expect("nested project directory");
        std::fs::create_dir_all(&sibling_path).expect("sibling directory");
        let project = project_path.to_str().expect("UTF-8 temporary path");
        let dot = canonical_surface_path(&project_path);
        let nested = canonical_surface_path(&nested_path);
        let parent = canonical_surface_path(&sibling_path);

        let cases = vec![
            (".", dot),
            ("./nested/project", nested),
            ("../sibling", parent),
        ];
        #[cfg(unix)]
        let cases = {
            let mut cases = cases;
            use std::os::unix::fs::symlink;

            let link_path = project_path.join("nested-link");
            symlink(&nested_path, &link_path).expect("create project symlink");
            cases.push(("./nested-link", canonical_surface_path(&nested_path)));
            cases
        };

        for (input, expected) in cases {
            let (package, selector) = planned_package(
                &[
                    "package-profile",
                    input,
                    "--compiler-outcome",
                    r#"{"kind":"current"}"#,
                ],
                project,
            );
            let PackageReference::Local(label) = package else {
                panic!("relative local path remains a local package reference");
            };
            assert_eq!(label.as_str(), expected.to_string_lossy().as_ref());
            assert_eq!(selector, Some(PackageCompilerOutcomeRead::Current));
        }
    }

    #[test]
    fn package_profile_keeps_registry_and_bare_local_coordinates_exact() {
        let (registry, selector) =
            planned_package(&["package-profile", "pkg:cargo/demo@1.0.0"], "/repo");
        assert_eq!(registry.as_str(), "pkg:cargo/demo@1.0.0");
        assert!(matches!(registry, PackageReference::Purl(_)));
        assert_eq!(selector, None);

        let (local, selector) = planned_package(&["package-profile", "demo"], "/repo");
        assert_eq!(local, PackageReference::parse("demo").expect("local label"));
        assert_eq!(selector, None);

        let (hierarchical, selector) = planned_package(&["package-profile", "scope/name"], "/repo");
        assert_eq!(
            hierarchical,
            PackageReference::parse("scope/name").expect("hierarchical local label")
        );
        assert_eq!(selector, None);
    }

    #[test]
    fn surface_json_keeps_the_exact_local_package_operand() {
        let words = [
            "surface",
            r#"{"operation":"package-profile","package":{"kind":"local","value":"."}}"#,
        ]
        .map(str::to_owned);
        let plan = plan(&words, &Options::fallback(), "/repo").expect("surface request");
        let (package, selector) = package_profile(plan.request);
        assert_eq!(
            package,
            PackageReference::parse(".").expect("local operand")
        );
        assert_eq!(selector, None);
    }
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
    run::execute_with_output_admission(
        session,
        &plan.request,
        None,
        None,
        options.detail(),
        None,
        |answer, cursor| render::admit_answer_with_cursor(answer, options, cursor),
    )
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
            ("mcp-config", crate::mcp_config::help_text()),
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
mod startup_tests {
    use super::*;

    #[test]
    fn cold_status_uses_the_existing_bounded_startup_and_passive_stays_connect_only() {
        for words in [vec!["status"], vec!["health"]] {
            let words = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let plan =
                plan(&words, &Options::fallback(), "/repo").expect("registered command plan");
            assert!(matches!(plan.request, Request::Status));
            assert_eq!(
                STARTUP_COMMAND_BUDGET,
                backend_runtime::COMMAND_STARTUP_TIMEOUT
            );
            assert!(
                STARTUP_COMMAND_BUDGET > Duration::from_secs(8),
                "the observed cold owner opening must not be cut off after one second"
            );
            assert!(plan.wait.is_none());
        }
        for words in [vec!["add", "."], vec!["search", "symbols"]] {
            let words = words.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let plan =
                plan(&words, &Options::fallback(), "/repo").expect("registered command plan");
            assert!(!matches!(plan.request, Request::Status));
            assert!(plan.wait.is_none());
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
        let retry = pending_status_retry(
            &Request::Status,
            &workspace,
            backend_present::StartupRecoveryCommand::Status,
        );
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
        if let Some(retry) = pending_status_retry(
            &Request::Status,
            &workspace,
            backend_present::StartupRecoveryCommand::Status,
        ) {
            assert!(retry.contains("--project '"));
            assert!(retry.contains("--workspace '"));
            assert!(retry.contains("'\\''$(sentinel)"));
            assert!(retry.ends_with(" status)"));
            assert!(!retry.contains("authority.secret"));
        }
        assert!(
            pending_status_retry(
                &Request::Index(".".to_owned()),
                &workspace,
                backend_present::StartupRecoveryCommand::Status
            )
            .is_none()
        );
        let lossless = backend_runtime::WorkspacePaths::discover(
            Some(root.join(OsString::from_vec(vec![b'p', 0xff]))),
            Some(root.join("state")),
            Some(root.join("owner.sock")),
        )
        .expect("read-only workspace discovery");
        assert!(
            pending_status_retry(
                &Request::Status,
                &lossless,
                backend_present::StartupRecoveryCommand::Status
            )
            .is_none()
        );
        for executable in [
            PathBuf::from("relative-backend-cli"),
            root.join(OsString::from_vec(vec![b'c', 0xff])),
            root.join(OsString::from_vec(vec![b'c', 0])),
        ] {
            assert!(
                pending_status_retry_for_executable(
                    &Request::Status,
                    &workspace,
                    backend_present::StartupRecoveryCommand::Status,
                    backend_present::StartupRecoveryMode::Active,
                    &executable,
                )
                .is_none()
            );
        }
        assert!(!root.exists());
    }

    #[test]
    fn recovery_does_not_invent_a_missing_or_non_executable_cli() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        assert!(
            checked_cli_executable(&PathBuf::from(OsString::from_vec(vec![b'/', 0xff]))).is_none()
        );
        assert!(checked_cli_executable(std::path::Path::new("/tmp/CLI\0argument")).is_none());
        assert!(checked_cli_executable(std::path::Path::new("/nonexistent/backend-cli")).is_none());
        assert!(checked_cli_executable(std::path::Path::new("relative/backend-cli")).is_none());
        assert!(checked_cli_executable(std::path::Path::new("/tmp")).is_none());
        let executable = std::env::current_exe().expect("observed test executable");
        assert_eq!(checked_cli_executable(&executable), Some(executable));
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
        assert!(matches!(
            plan(
                &words,
                &options,
                paths.project().to_str().expect("probe project")
            )
            .expect("actual read-only recovery command plan")
            .request,
            Request::Status
        ));
        let selection = paths.startup_recovery_selection().expect("Unix selection");
        let (boundary, anchor) = match selection.workspace {
            backend_runtime::StartupWorkspaceSelection::Explicit(_) => ("explicit", None),
            backend_runtime::StartupWorkspaceSelection::Home(anchor) => ("home", Some(anchor)),
            backend_runtime::StartupWorkspaceSelection::XdgState(anchor) => {
                ("xdg-state", Some(anchor))
            }
            backend_runtime::StartupWorkspaceSelection::XdgData(anchor) => {
                ("xdg-data", Some(anchor))
            }
        };
        let passive_fault_shell = if options.passive() {
            let fault =
                run_words(&words, &options).expect_err("uncreated endpoint refuses a passive read");
            assert!(
                !paths.data().exists(),
                "a passive failure never starts or prepares an owner"
            );
            match fault.affordance() {
                Affordance::RecoverStartup { context } => Some(context.shell()),
                Affordance::Retry => None,
                other => panic!("unexpected passive missing-endpoint recovery: {other:?}"),
            }
        } else {
            None
        };
        let value = serde_json::json!({
            "project": paths.project().to_str(),
            "data": paths.data().to_str(),
            "endpoint": paths.endpoint().to_str(),
            "authority": paths.authority_secret().to_str(),
            "boundary": boundary,
            "anchor": anchor.and_then(std::path::Path::to_str),
            "command": words.first(),
            "passive": options.passive(),
            "passive_fault_shell": passive_fault_shell,
            "retry": match std::env::var_os("NUDOX_TEST_RECOVERY_EXECUTABLE") {
                Some(path) => checked_cli_executable(&PathBuf::from(path)).and_then(|executable|
                    pending_status_retry_for_executable(&Request::Status, &paths, original_status_command(&words), connection_mode(&options), &executable)),
                None => std::env::current_exe().ok().and_then(|executable| checked_cli_executable(&executable)).and_then(|executable| pending_status_retry_for_executable(&Request::Status, &paths, original_status_command(&words), connection_mode(&options), &executable)),
            },
        });
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

    fn replay_startup_selection(
        executable: &std::path::Path,
        capture: &std::path::Path,
        shell: &str,
    ) -> serde_json::Value {
        use std::process::Command;
        const PROBE: &str = "process::startup_tests::startup_recovery_discovery_probe";
        const UNSET: &str = "__UNSET__";
        let parsed = Command::new("/bin/sh")
            .args(["-c", shell])
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
        replay.env("NUDOX_TEST_RECOVERY_EXECUTABLE", capture);
        replay.args(["--exact", PROBE, "--nocapture"]).env(
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
        let project = root.join("project ' $(sentinel)\nnext");
        let endpoint = root.join("owner.sock");
        let state = root.join("explicit ' `sentinel`; state\nnext");
        let home = root.join("home ' $(sentinel)\nnext");
        let xdg = root.join("xdg ' $(sentinel)\nnext");
        let authority = root.join("authority ' $(sentinel)");
        let capture_root = root.with_extension("capture");
        std::fs::create_dir(&capture_root).expect("owned argv capture directory");
        let capture = capture_root.join("CLI ' executable\nline");
        std::fs::write(&capture, r#"#!/bin/sh
printf '%s\000' "$@" __ENV__ "${HOME-__UNSET__}" "${XDG_STATE_HOME-__UNSET__}" "${XDG_DATA_HOME-__UNSET__}" "${BACKEND_LOCALD_WORKSPACE-__UNSET__}" "${BACKEND_LOCALD_AUTHORITY_SECRET_FILE-__UNSET__}"
"#).expect("owned capture executable");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&capture, std::fs::Permissions::from_mode(0o700))
            .expect("executable capture");
        #[cfg(target_os = "macos")]
        let origins = ["explicit", "environment", "home"];
        #[cfg(not(target_os = "macos"))]
        let origins = ["explicit", "environment", "home", "xdg-state", "xdg-data"];
        for origin in origins {
            for passive in [false, true] {
                for (custom_authority, action) in
                    [(false, "status"), (false, "health"), (true, "status")]
                {
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
                    if passive {
                        argv.push("--passive".to_owned());
                    }
                    argv.push(action.to_owned());
                    let mut original = Command::new(&executable);
                    original
                        .env("NUDOX_TEST_RECOVERY_EXECUTABLE", &capture)
                        .args(["--exact", PROBE, "--nocapture"])
                        .env(
                            "NUDOX_TEST_RECOVERY_ARGV",
                            serde_json::to_string(&argv).expect("original argv"),
                        )
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
                    assert_eq!(before["passive"], passive);
                    if passive && !custom_authority {
                        let fault_shell = before["passive_fault_shell"]
                            .as_str()
                            .expect("actual passive refusal recovery");
                        assert!(fault_shell.contains(" --passive "));
                        assert!(fault_shell.ends_with(&format!(" {action})")));
                        assert!(!fault_shell.contains("authority.secret"));
                    } else {
                        assert!(before["passive_fault_shell"].is_null());
                    }
                    if custom_authority {
                        assert!(
                            before["retry"].is_null(),
                            "custom authority cannot be rebound or displayed"
                        );
                    } else {
                        let shell = before["retry"].as_str().expect("complete captured retry");
                        assert!(!shell.contains("authority.secret"));
                        assert!(!shell.contains(" nudox "));
                        assert!(!shell.contains(authority.to_str().expect("fixture authority")));
                        assert!(shell.ends_with(&format!(" {action})")));
                        assert_eq!(
                            before,
                            replay_startup_selection(&executable, &capture, shell),
                            "{origin}/{custom_authority}/{action}/passive={passive}"
                        );
                    }
                }
            }
        }
        assert!(
            !root.exists(),
            "discovery and shell parsing never prepare state"
        );
        std::fs::remove_dir_all(capture_root).expect("remove owned argv capture");
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
