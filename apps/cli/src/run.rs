//! Execution: the shared driver, wired to a connected local session.
//!
//! Every decision about *which* round trips one command needs — a page is a
//! document plus the rows around it, an outline is a tree of keys plus the rows
//! that name them, failed section probes retain a useful page and typed fault —
//! lives in [`backend_present::answer`], because the MCP surface must make the
//! same ones. The CLI's optional owner replacement policy only repeats
//! unpinned reads or reconciles a journaled start under its original key.

use backend_client::{ClientError, Session};
use backend_library::{
    HealthReport, PageContinuation, ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot,
    encode_id,
};
use backend_present::{Engine, Fault, Probe, Request};

pub use backend_present::Answer;

/// One connected local daemon session, seen as the shared driver's engine.
pub struct SessionEngine<'a> {
    session: &'a mut Session,
    paths: Option<&'a backend_runtime::WorkspacePaths>,
    recovery: Option<&'a mut crate::owner_recovery::OwnerRecovery>,
}

impl<'a> SessionEngine<'a> {
    /// Borrows one connected session as an engine.
    pub const fn new(session: &'a mut Session) -> Self {
        Self {
            session,
            paths: None,
            recovery: None,
        }
    }
}

impl Engine for SessionEngine<'_> {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.session.revision().map(|revision| revision.root)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        match &mut self.recovery {
            Some(recovery) => recovery.read(
                self.session,
                crate::owner_recovery::RecoverableRead::Health,
                Session::health,
            ),
            None => self.session.health(),
        }
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Packages => self.session.packages(),
            Probe::Index(path) => self.session.index(path).map_err(nonrepeatable),
            Probe::IndexWithExecutionIntent {
                path,
                execution_intent,
            } => self
                .session
                .index_with_execution_intent(path, execution_intent)
                .map_err(nonrepeatable),
            Probe::Remove(path) => self.session.remove(path).map_err(nonrepeatable),
            Probe::Document(at) => self.session.document(at),
            Probe::Source(at) => self.session.source(at),
            Probe::Related(at) => self.session.related(at),
            Probe::Graph(at) => self.session.graph(at),
            Probe::Search { text, limit } => match &mut self.recovery {
                Some(recovery) => recovery.read(
                    self.session,
                    crate::owner_recovery::RecoverableRead::UnselectedSearch,
                    |session| session.search(text, limit),
                ),
                None => self.session.search(text, limit),
            },
            Probe::Names { text, limit } => self.session.names(text, limit),
            Probe::Outline(path) => self.session.outline(path),
            Probe::OutlinePage { path, limit } => self.session.outline_page(path, limit, None),
        }
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Search { text, limit } if continuation.is_none() => match &mut self.recovery {
                Some(recovery) => recovery.read(
                    self.session,
                    crate::owner_recovery::RecoverableRead::UnselectedSearch,
                    |session| session.search_page(text, limit, None),
                ),
                None => self.session.search_page(text, limit, None),
            },
            Probe::Search { text, limit } => self.session.search_page(text, limit, continuation),
            Probe::Names { text, limit } => self.session.names_page(text, limit, continuation),
            Probe::OutlinePage { path, limit } => {
                self.session.outline_page(path, limit, continuation)
            }
            _ if continuation.is_none() => self.probe(probe),
            _ => Err(ClientError::Protocol(
                "this CLI command does not support continuation pages".to_owned(),
            )),
        }
    }

    fn prepare_query_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.session.decode_page_continuation(token)
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let session = &mut *self.session;
        let recovery = &mut self.recovery;
        match self.paths {
            Some(paths) => {
                backend_client::surface_with_index_journal(paths, command, |command| match recovery
                    .as_deref_mut()
                {
                    Some(recovery) => recovery.surface(session, command, None),
                    None => session.surface(command),
                })
            }
            None => surface_once(session, command),
        }
    }

    fn index(
        &mut self,
        package: backend_library::PackageReference,
        execution_intent: backend_library::CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        let session = &mut *self.session;
        let recovery = &mut self.recovery;
        match self.paths {
            Some(paths) => backend_client::index_with_journal_recovery(
                paths,
                package,
                execution_intent,
                |dispatch| match recovery.as_deref_mut() {
                    Some(recovery) => recovery.journal(session, dispatch, None),
                    None => session.surface(dispatch.command()),
                },
            ),
            None => session.surface(SurfaceCommand::IndexStart {
                package,
                execution_intent,
            }),
        }
    }
}

/// Executes one typed request against a connected session.
///
/// # Errors
///
/// Returns the typed fault the engine or the transport produced, with the
/// operand the caller supplied already attached.
pub fn execute(session: &mut Session, request: &Request) -> Result<Answer, Fault> {
    backend_present::answer(&mut SessionEngine::new(session), request)
}

/// Executes against the frozen runtime attachment, retaining default index
/// claims across command exits, interrupted delivery and owner restarts.
///
/// # Errors
/// Returns the typed owner, journal or transport fault.
pub fn execute_in_workspace(
    session: &mut Session,
    request: &Request,
    paths: &backend_runtime::WorkspacePaths,
) -> Result<Answer, Fault> {
    backend_present::answer(
        &mut SessionEngine {
            session,
            paths: Some(paths),
            recovery: None,
        },
        request,
    )
}

#[cfg(test)]
pub(crate) fn execute_with_recovery(
    session: &mut Session,
    request: &Request,
    paths: &backend_runtime::WorkspacePaths,
    recovery: &mut crate::owner_recovery::OwnerRecovery,
) -> Result<Answer, Fault> {
    backend_present::answer(
        &mut SessionEngine {
            session,
            paths: Some(paths),
            recovery: Some(recovery),
        },
        request,
    )
}

/// Executes complete native graph candidates under the caller's actual
/// rendering admission, preserving the same connected engine and recovery.
pub(crate) fn execute_with_output_admission(
    session: &mut Session,
    request: &Request,
    paths: Option<&backend_runtime::WorkspacePaths>,
    recovery: Option<&mut crate::owner_recovery::OwnerRecovery>,
    detail: backend_present::Detail,
    cli_context: Option<&backend_present::StartupRecoveryContext>,
    mut admit: impl FnMut(&Answer) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    let mut engine = SessionEngine {
        session,
        paths,
        recovery,
    };
    let Request::Surface(command) = request else {
        return backend_present::answer(&mut engine, request);
    };
    let backend_library::SurfaceCommand::PackageGraphPage { request } = command.as_ref() else {
        return backend_present::answer(&mut engine, &Request::Surface(command.clone()));
    };
    let mut selection = backend_present::PackageGraphPageSelection::new(request)?;
    let original_context = engine.session.request_context().cloned();
    let lifetime = original_context.clone().unwrap_or_default();
    lifetime
        .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
        .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
    engine
        .session
        .set_request_context(Some(lifetime.clone()))
        .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
    let result = (|| loop {
        lifetime
            .check()
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        let request = Request::Surface(Box::new(
            backend_library::SurfaceCommand::PackageGraphPage {
                request: selection.request().clone(),
            },
        ));
        let answer = backend_present::answer(&mut engine, &request)?;
        let page = match &answer {
            Answer::Product(view) => view.package_graph_page(),
            _ => None,
        };
        if let Some(page) = page {
            selection.admit_page(page)?;
        } else if answer.fault().is_none() {
            return Err(Fault::from_client_error(
                &ClientError::Protocol("native graph page changed shape".to_owned()),
                backend_present::Operand::Whole,
            ));
        }
        match admit(&answer) {
            Ok(()) => return Ok(answer),
            Err(fault) if selection.shrink_after(&fault) => continue,
            Err(fault) => {
                let advice = page.and_then(|page| {
                    backend_present::package_graph_json_alternative(
                        page,
                        selection.request(),
                        detail,
                        cli_context.cloned(),
                    )
                });
                return Err(match advice {
                    Some(advice)
                        if fault.cause().slug() == backend_present::CauseSlug::Oversized =>
                    {
                        fault.with_affordance(advice)
                    }
                    _ => fault,
                });
            }
        }
    })();
    let restored = engine
        .session
        .set_request_context(original_context)
        .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole));
    result.and_then(|answer| restored.map(|()| answer))
}

/// One surface exchange with no owner replacement. A command that changes
/// state cannot be proven undelivered once its connection is lost, so the
/// loss is reported as an unknown outcome; a bare disconnect then always
/// means a read, which is what its fault wording promises.
fn surface_once(
    session: &mut Session,
    command: SurfaceCommand,
) -> Result<SurfaceReply, ClientError> {
    let writes = backend_library::command_spec(command.id()).mutation
        == backend_library::CommandMutation::Write;
    let result = session.surface(command);
    if writes {
        result.map_err(nonrepeatable)
    } else {
        result
    }
}

fn nonrepeatable(error: ClientError) -> ClientError {
    match error {
        ClientError::Disconnected(kind) => ClientError::MutationOutcomeUnknown(kind),
        error => error,
    }
}

/// Returns the exact revision an answer was read at, for a caption.
#[must_use]
pub fn revision_tag(session: &mut Session) -> Option<String> {
    session
        .revision()
        .ok()
        .map(|revision| encode_id(revision.root.as_bytes()))
}

#[cfg(all(test, unix))]
mod graph_admission_tests {
    use super::*;
    use backend_library::{
        Command, CommandDto, CommandReply, DependencyAuthority, DependencyEvidence,
        DependencyScope, PackageDependencyRecord, PackageDependencyTarget, PackageGraphDirection,
        PackageGraphKnowledge, PackageGraphPage, PackageGraphPageRequest, PackageGraphPageTerminal,
        PackageGraphSourceAuthority, PackageGraphSourceKey, PackageReference, RegistryEcosystem,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn cli_graph_refusal_preserves_observed_attachment_input_and_original_scope() {
        const PROBE: &str = "run::graph_admission_tests::cli_graph_refusal_probe";
        for custom in [false, true] {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"));
            child
                .args(["--exact", PROBE, "--nocapture"])
                .env("NUDOX_TEST_GRAPH_REFUSAL", "1");
            if custom {
                child.env(
                    backend_runtime::AUTHORITY_SECRET_ENV,
                    "/private/not-displayed-graph-authority",
                );
            } else {
                child.env_remove(backend_runtime::AUTHORITY_SECRET_ENV);
            }
            let result = child.output().expect("isolated CLI execution test");
            assert!(result.status.success(), "{result:?}");
        }
    }

    struct GraphTransport {
        page: PackageGraphPage,
        seen: Arc<Mutex<Vec<Command>>>,
    }
    impl backend_client::CommandTransport for GraphTransport {
        fn request(&mut self, command: CommandDto) -> Result<ReplyDto, ClientError> {
            self.seen
                .lock()
                .expect("commands")
                .push(command.command.clone());
            Ok(ReplyDto::new(
                command.request_id,
                CommandReply::Surface(SurfaceReply::PackageGraphPage(self.page.clone())),
            ))
        }
    }

    #[test]
    fn cli_graph_refusal_probe() {
        if std::env::var("NUDOX_TEST_GRAPH_REFUSAL").is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("graph-refusal-probe-{}", std::process::id()));
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(base.join("project ' literal\nnext")),
            Some(base.join("state")),
            Some(base.join("owner.sock")),
        )
        .expect("uncreated frozen attachment");
        let source =
            PackageReference::parse(&format!("pkg:cargo/{}@1", "s".repeat(2000))).expect("source");
        let authority = PackageGraphSourceAuthority::Local([7; 32]);
        let target = "t".repeat(3000);
        let edge = PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            authority,
            PackageDependencyTarget::new(
                RegistryEcosystem::Cargo,
                target.clone(),
                "r".repeat(4000),
                Some(PackageReference::parse(&format!("pkg:cargo/{target}@1")).expect("target")),
            )
            .expect("complete wide requirement"),
            DependencyScope::Runtime,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::LocalManifest,
                frontier: [7; 32],
                provenance: [8; 32],
            },
        );
        let mut request = PackageGraphPageRequest::new(
            source.clone(),
            PackageGraphDirection::Dependencies,
            Some(authority),
            1,
        )
        .expect("fixed native request");
        request.catalog_snapshot = Some([6; 32]);
        let page = PackageGraphPage {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [9; 32],
            facts_witness: [10; 32],
            catalog_snapshot: request.catalog_snapshot,
            package: source.clone(),
            direction: request.direction,
            source: Some(PackageGraphSourceKey::new(source, authority)),
            limit: 1,
            knowledge: PackageGraphKnowledge::Known,
            rows: vec![edge].into_boxed_slice(),
            terminal: PackageGraphPageTerminal::Complete,
        };
        page.admit_for(&request)
            .expect("actual native request-bound fixture");
        let command = SurfaceCommand::PackageGraphPage {
            request: request.clone(),
        };
        let original = Request::Surface(Box::new(command.clone()));
        for mode in [
            backend_present::StartupRecoveryMode::Active,
            backend_present::StartupRecoveryMode::Existing,
        ] {
            let context = crate::process::observed_product_cli_context(&paths, mode);
            let custom = std::env::var_os(backend_runtime::AUTHORITY_SECRET_ENV).is_some();
            assert_eq!(
                context.is_none(),
                custom,
                "custom authority never grants a displayed/rebound command"
            );
            let seen = Arc::new(Mutex::new(Vec::new()));
            let mut session = Session::from_transport(
                paths.endpoint(),
                GraphTransport {
                    page: page.clone(),
                    seen: seen.clone(),
                },
            );
            let scope = backend_client::RequestContext::new();
            let deadline = scope
                .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
                .expect("original deadline");
            session
                .set_request_context(Some(scope.clone()))
                .expect("original request scope");
            let options = crate::options::Options::plain(crate::options::Format::Human);
            let fault = execute_with_output_admission(
                &mut session,
                &original,
                Some(&paths),
                None,
                backend_present::Detail::Summary,
                context.as_ref(),
                |answer| crate::render::admit_graph_answer(answer, &options),
            )
            .expect_err("real complete human graph exceeds its preview budget");
            assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
            assert_eq!(
                *seen.lock().expect("exact commands"),
                [Command::Surface(command.clone())],
                "fixed graph input is neither resized nor replayed"
            );
            let restored = session.request_context().expect("restored original scope");
            assert_eq!(
                restored
                    .read_deadline(std::time::Duration::from_secs(90))
                    .expect("same deadline"),
                deadline
            );
            if custom {
                assert!(fault.affordance().shell().is_none());
            } else {
                let backend_present::Affordance::ReadPackageGraphJson {
                    request: exact,
                    detail,
                    ..
                } = fault.affordance()
                else {
                    panic!("complete JSON recovery affordance")
                };
                assert_eq!(exact.as_ref(), &request);
                assert_eq!(*detail, backend_present::Detail::Summary);
                let shell = fault
                    .affordance()
                    .shell()
                    .expect("observed current executable");
                assert_eq!(
                    shell.contains(" --passive "),
                    mode == backend_present::StartupRecoveryMode::Existing
                );
                assert!(shell.contains(" --format json "));
                assert!(!shell.contains("authority.secret"));
                assert!(!shell.contains("not-displayed-graph-authority"));
                assert!(fault.affordance().tool_call().is_none());
            }
            scope.cancel(backend_client::RequestCancellationReason::Explicit);
            assert_eq!(
                session
                    .request_context()
                    .expect("original scope")
                    .cancellation_reason(),
                Some(backend_client::RequestCancellationReason::Explicit)
            );
        }
        assert!(
            !paths.data().exists(),
            "advice never creates a workspace or authority"
        );
        assert!(!paths.endpoint().exists(), "advice never starts an owner");
    }
}
