//! Execution: the shared driver, wired to a connected local session.
//!
//! Every decision about *which* round trips one command needs — a page is a
//! document plus the rows around it, an outline is a tree of keys plus the rows
//! that name them, a probe that fails becomes a note rather than a failure —
//! lives in [`backend_present::answer`], because the MCP surface must make the
//! same ones. The CLI's optional owner replacement policy only repeats
//! unpinned reads or reconciles a journaled start under its original key.

use backend_client::{ClientError, Session};
use backend_library::{
    encode_id, HealthReport, PageContinuation, ReplyDto, SurfaceCommand, SurfaceReply,
    ViewStateRoot,
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
        DependencyFacts, DependencyScope, IndexedCheckedPackageGraph, PackageDependencyRecord,
        PackageDependencyTarget, PackageGraphCursor, PackageGraphDirection,
        PackageGraphIndexLimits, PackageGraphKnowledge, PackageGraphPage, PackageGraphPageRequest,
        PackageGraphPageTerminal, PackageGraphSourceAuthority, PackageGraphSourceKey,
        PackageReference, RegistryEcosystem, VerifiedPackageGraphPageRequest,
    };
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct CheckedGraphFixture {
        graph: Arc<IndexedCheckedPackageGraph>,
        source: PackageGraphSourceKey,
        rows: Arc<[PackageDependencyRecord]>,
        owner_key: [u8; 32],
        view_root: [u8; 32],
    }

    fn checked_graph_fixture(row_count: usize) -> CheckedGraphFixture {
        let package = PackageReference::parse("pkg:cargo/fixture@1").expect("source");
        let authority = PackageGraphSourceAuthority::Local([7; 32]);
        let source = PackageGraphSourceKey::new(package.clone(), authority);
        let mut rows = (0..row_count)
            .map(|index| {
                let target = format!("{}{index}", "a".repeat(2_000));
                PackageDependencyRecord::new_with_source_authority(
                    package.clone(),
                    authority,
                    PackageDependencyTarget::new(
                        RegistryEcosystem::Cargo,
                        target.clone(),
                        "a".repeat(4_000),
                        Some(
                            PackageReference::parse(&format!("pkg:cargo/{target}@1"))
                                .expect("admitted target coordinate"),
                        ),
                    )
                    .expect("complete wide requirement"),
                    DependencyScope::Runtime,
                    false,
                    DependencyEvidence {
                        authority: DependencyAuthority::LocalManifest,
                        frontier: [7; 32],
                        provenance: [8; 32],
                    },
                )
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|row| row.facts_version);
        let graph = Arc::new(
            IndexedCheckedPackageGraph::new(
                vec![(
                    source.clone(),
                    DependencyFacts::Known(rows.clone().into_boxed_slice()),
                )],
                PackageGraphIndexLimits {
                    max_sources: 128,
                    max_fact_bytes: 256 * 1024 * 1024,
                    max_total_rows: 4_096,
                    max_reverse_edges: 4_096,
                    max_index_key_bytes: 8 * 1024 * 1024,
                },
            )
            .expect("owner-indexed checked facts"),
        );
        CheckedGraphFixture {
            graph,
            source,
            rows: Arc::from(rows.into_boxed_slice()),
            owner_key: [0x51; 32],
            view_root: [0x19; 32],
        }
    }

    fn graph_request(fixture: &CheckedGraphFixture, limit: u16) -> PackageGraphPageRequest {
        PackageGraphPageRequest::new(
            fixture.source.coordinate.clone(),
            PackageGraphDirection::Dependencies,
            Some(fixture.source.authority),
            limit,
        )
        .expect("bounded typed graph request")
    }

    /// Builds exactly one canonical checked-fact page, then lets the public
    /// owner API verify and seal its continuation before a client sees it.
    fn checked_graph_page(
        request: &PackageGraphPageRequest,
        fixture: &CheckedGraphFixture,
    ) -> PackageGraphPage {
        request.admit().expect("request admission");
        assert_eq!(request.direction, PackageGraphDirection::Dependencies);
        assert_eq!(request.package, fixture.source.coordinate);
        assert_eq!(request.authority, Some(fixture.source.authority));

        let start = request.cursor.as_ref().map_or(0, |cursor| {
            fixture
                .rows
                .partition_point(|row| row.facts_version <= cursor.after_edge_id)
        });
        let end = start
            .saturating_add(usize::from(request.limit))
            .min(fixture.rows.len());
        let rows = fixture.rows[start..end].to_vec().into_boxed_slice();
        let terminal = if end < fixture.rows.len() {
            let after_edge_id = rows.last().expect("nonempty continuing page").facts_version;
            PackageGraphPageTerminal::More(PackageGraphCursor {
                schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
                view_root: fixture.view_root,
                facts_witness: fixture.graph.witness(),
                recipe: request.recipe(Some(&fixture.source)),
                catalog_snapshot: request.catalog_snapshot,
                source: Some(fixture.source.clone()),
                limit: request.limit,
                after_edge_id,
                // This unsigned candidate is never returned: the owner API
                // checks its exact facts and issues the authenticated seal.
                seal: [0; 32],
            })
        } else {
            PackageGraphPageTerminal::Complete
        };
        let mut page = PackageGraphPage {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: fixture.view_root,
            facts_witness: fixture.graph.witness(),
            catalog_snapshot: request.catalog_snapshot,
            package: request.package.clone(),
            direction: request.direction,
            source: Some(fixture.source.clone()),
            limit: request.limit,
            knowledge: PackageGraphKnowledge::Known,
            rows,
            terminal,
        };
        let verified = VerifiedPackageGraphPageRequest::verify(request, &fixture.owner_key)
            .expect("public owner verification of exact request");
        page.seal_against_checked_facts(&verified, fixture.view_root, &fixture.graph)
            .expect("public owner verifies exact rows and issues any cursor seal");
        page
    }

    #[test]
    fn cli_graph_adaptation_preserves_observed_attachment_input_and_original_scope() {
        const PROBE: &str = "run::graph_admission_tests::cli_graph_adaptation_probe";
        for custom in [false, true] {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"));
            child
                .args(["--exact", PROBE, "--nocapture"])
                .env("NUDOX_TEST_GRAPH_ADAPTATION", "1");
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
        fixture: CheckedGraphFixture,
        seen: Arc<Mutex<Vec<Command>>>,
    }
    impl backend_client::CommandTransport for GraphTransport {
        fn request(&mut self, command: CommandDto) -> Result<ReplyDto, ClientError> {
            let Command::Surface(SurfaceCommand::PackageGraphPage { request }) = &command.command
            else {
                return Err(ClientError::Protocol(
                    "graph fixture received an unrelated command".to_owned(),
                ));
            };
            let page = checked_graph_page(request, &self.fixture);
            self.seen
                .lock()
                .expect("commands")
                .push(command.command.clone());
            Ok(ReplyDto::new(
                command.request_id,
                CommandReply::Surface(SurfaceReply::PackageGraphPage(page)),
            ))
        }
    }

    #[test]
    fn cli_graph_adaptation_probe() {
        if std::env::var("NUDOX_TEST_GRAPH_ADAPTATION").is_err() {
            return;
        }
        let base = std::env::temp_dir().join(format!("graph-refusal-probe-{}", std::process::id()));
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(base.join("project ' literal\nnext")),
            Some(base.join("state")),
            Some(base.join("owner.sock")),
        )
        .expect("uncreated frozen attachment");
        // Two wide, complete checked-fact rows force the normal 5-to-2-to-1
        // output adaptation. The final one-row page must retain a genuine
        // More cursor for the unreturned edge.
        let fixture = checked_graph_fixture(2);
        let request = graph_request(&fixture, 5);
        let source = fixture.source.coordinate.clone();
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
                    fixture: fixture.clone(),
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
            let answer = execute_with_output_admission(
                &mut session,
                &original,
                Some(&paths),
                None,
                backend_present::Detail::Summary,
                context.as_ref(),
                |answer| crate::render::admit_graph_answer(answer, &options),
            )
            .expect("one-row complete page fits after valid adaptive reduction");
            let Answer::Product(view) = answer else {
                panic!("complete graph product answer");
            };
            let final_page = view.package_graph_page().expect("complete graph page");
            assert_eq!(final_page.rows.len(), 1);
            let PackageGraphPageTerminal::More(cursor) = &final_page.terminal else {
                panic!("adaptive one-row page preserves the remaining edge");
            };
            assert_eq!(cursor.after_edge_id, final_page.rows[0].facts_version);
            assert_eq!(
                *seen.lock().expect("exact commands"),
                [
                    Command::Surface(command.clone()),
                    Command::Surface(SurfaceCommand::PackageGraphPage {
                        request: PackageGraphPageRequest {
                            limit: 2,
                            ..request.clone()
                        },
                    }),
                    Command::Surface(SurfaceCommand::PackageGraphPage {
                        request: PackageGraphPageRequest {
                            limit: 1,
                            ..request.clone()
                        },
                    }),
                ],
                "the valid graph adapts at the published page-size sequence"
            );
            let remaining =
                checked_graph_page(&request.clone().with_cursor(cursor.clone()), &fixture);
            assert_eq!(remaining.rows.as_ref(), &fixture.rows[1..]);
            assert_eq!(remaining.terminal, PackageGraphPageTerminal::Complete);
            let restored = session.request_context().expect("restored original scope");
            assert_eq!(
                restored
                    .read_deadline(std::time::Duration::from_secs(90))
                    .expect("same deadline"),
                deadline
            );
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

    #[test]
    fn cli_fixed_cursor_refusal_preserves_observed_attachment_and_original_scope() {
        const PROBE: &str = "run::graph_admission_tests::cli_fixed_cursor_refusal_probe";
        for custom in [false, true] {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"));
            child
                .args(["--exact", PROBE, "--nocapture"])
                .env("NUDOX_TEST_GRAPH_FIXED_CURSOR", "1");
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

    #[test]
    fn cli_fixed_cursor_refusal_probe() {
        if std::env::var("NUDOX_TEST_GRAPH_FIXED_CURSOR").is_err() {
            return;
        }
        let base =
            std::env::temp_dir().join(format!("graph-fixed-cursor-refusal-{}", std::process::id()));
        let paths = backend_runtime::WorkspacePaths::discover(
            Some(base.join("project ' literal\nnext")),
            Some(base.join("state")),
            Some(base.join("owner.sock")),
        )
        .expect("uncreated frozen attachment");
        let fixture = checked_graph_fixture(6);
        let initial = graph_request(&fixture, 3);
        let initial_page = checked_graph_page(&initial, &fixture);
        let PackageGraphPageTerminal::More(cursor) = initial_page.terminal else {
            panic!("three-row initial page issues a real continuation");
        };
        assert_eq!(
            cursor.after_edge_id,
            initial_page
                .rows
                .last()
                .expect("initial page row")
                .facts_version
        );
        let request = initial.with_cursor(cursor.clone());
        VerifiedPackageGraphPageRequest::verify(&request, &fixture.owner_key)
            .expect("continuation verifies against the same owner key");
        let resumed_page = checked_graph_page(&request, &fixture);
        assert_eq!(resumed_page.rows.len(), 3);
        assert_eq!(resumed_page.terminal, PackageGraphPageTerminal::Complete);
        assert_eq!(
            resumed_page.rows.as_ref(),
            &fixture.rows[3..],
            "the issued boundary resumes at the exact next edge"
        );

        for mode in [
            backend_present::StartupRecoveryMode::Active,
            backend_present::StartupRecoveryMode::Existing,
        ] {
            let context = crate::process::observed_product_cli_context(&paths, mode);
            let custom = std::env::var_os(backend_runtime::AUTHORITY_SECRET_ENV).is_some();
            assert_eq!(context.is_none(), custom);
            let seen = Arc::new(Mutex::new(Vec::new()));
            let mut session = Session::from_transport(
                paths.endpoint(),
                GraphTransport {
                    fixture: fixture.clone(),
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
            let human = crate::options::Options::plain(crate::options::Format::Human);
            let json = crate::options::Options::plain(crate::options::Format::Json);
            let fault = execute_with_output_admission(
                &mut session,
                &Request::Surface(Box::new(SurfaceCommand::PackageGraphPage {
                    request: request.clone(),
                })),
                Some(&paths),
                None,
                backend_present::Detail::Summary,
                context.as_ref(),
                |answer| {
                    crate::render::admit_graph_answer(answer, &json)
                        .expect("complete JSON fits unchanged byte and token budgets");
                    crate::render::admit_graph_answer(answer, &human)
                },
            )
            .expect_err("the exact resumed page exceeds the human preview budget");
            assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
            assert_eq!(
                *seen.lock().expect("exact commands"),
                [Command::Surface(SurfaceCommand::PackageGraphPage {
                    request: request.clone(),
                })],
                "a resumed cursor is fixed-size and is never shrunk or replayed"
            );
            let restored = session.request_context().expect("restored original scope");
            assert_eq!(
                restored
                    .read_deadline(std::time::Duration::from_secs(90))
                    .expect("same deadline"),
                deadline
            );
            scope.cancel(backend_client::RequestCancellationReason::Explicit);
            assert_eq!(
                session
                    .request_context()
                    .expect("original scope")
                    .cancellation_reason(),
                Some(backend_client::RequestCancellationReason::Explicit)
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
                    panic!("complete JSON recovery affordance for the exact resume");
                };
                assert_eq!(exact.as_ref(), &request);
                assert_eq!(exact.authority, Some(fixture.source.authority));
                assert_eq!(exact.cursor.as_ref(), Some(&cursor));
                assert_eq!(exact.limit, 3);
                assert_eq!(*detail, backend_present::Detail::Summary);
                VerifiedPackageGraphPageRequest::verify(exact, &fixture.owner_key)
                    .expect("recovery keeps the genuine owner-issued cursor");
                let shell = fault
                    .affordance()
                    .shell()
                    .expect("observed CLI executable and attachment");
                assert!(shell.contains(" --format json "));
                assert!(shell.contains(&paths.project().to_string_lossy().to_string()));
                assert!(shell.contains(&paths.data().to_string_lossy().to_string()));
                assert!(shell.contains(&paths.endpoint().to_string_lossy().to_string()));
                assert!(shell.contains("unset BACKEND_LOCALD_AUTHORITY_SECRET_FILE"));
                assert!(!shell.contains("authority.secret"));
                assert!(!shell.contains("not-displayed-graph-authority"));
            }
        }
        assert!(
            !paths.data().exists(),
            "advice never creates a workspace or authority"
        );
        assert!(!paths.endpoint().exists(), "advice never starts an owner");
    }
}
