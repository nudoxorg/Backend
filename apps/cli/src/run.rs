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
    fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.session.members_page(document, limit, continuation)
    }

    fn declaration_graph_window(
        &mut self,
        selector: &backend_library::DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        match selector {
            backend_library::DeclarationSelector::Coordinate(coordinate) => self
                .session
                .graph_page_window(coordinate, window, continuation),
            selected => self
                .session
                .graph_page_declaration(selected, window, continuation),
        }
    }

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
            Probe::Declaration { selector, read } => self.session.declaration(selector, read),
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

    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::OutlinePage { path, limit } if limit == window.issued().get() => {
                self.session.outline_page_window(path, window, continuation)
            }
            _ => Err(ClientError::Protocol(
                "CLI outline delivery changed query family or credit".to_owned(),
            )),
        }
    }

    fn prepare_query_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.session.decode_page_continuation(token)
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let session = &mut *self.session;
        let recovery = &mut self.recovery;
        let context = session.request_context().cloned();
        match self.paths {
            Some(paths) => {
                backend_client::surface_with_index_journal(paths, command, |command| match recovery
                    .as_deref_mut()
                {
                    Some(recovery) => recovery.surface(session, command, context.as_ref()),
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
        let context = session.request_context().cloned();
        match self.paths {
            Some(paths) => backend_client::index_with_journal_recovery_wait(
                paths,
                package,
                execution_intent,
                context.as_ref(),
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
    answer_with_compiler_progress(&mut SessionEngine::new(session), request)
}

// Explicit CLI index_progress uses the same admitted composition as MCP and
// add --wait. Bind all reads to the pre-existing absolute session scope.
fn answer_with_compiler_progress(
    engine: &mut SessionEngine<'_>,
    request: &Request,
) -> Result<Answer, Fault> {
    if !matches!(request, Request::Surface(command) if matches!(command.as_ref(), SurfaceCommand::IndexOperationStatus { .. }))
    {
        return backend_present::answer(engine, request);
    }
    let operand = match request {
        Request::Surface(command) => match command.as_ref() {
            SurfaceCommand::IndexOperationStatus { operation_key } => {
                backend_present::Operand::Text(operation_key.to_hex())
            }
            _ => backend_present::Operand::Whole,
        },
        _ => backend_present::Operand::Whole,
    };
    let original = engine.session.request_context().cloned();
    let context = original.clone().unwrap_or_default();
    context
        .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
        .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
    engine
        .session
        .set_request_context(Some(context.clone()))
        .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
    let result = backend_present::answer(engine, request).and_then(|answer| {
        let Answer::Product(view) = answer else {
            return Ok(answer);
        };
        let Some(observation) = view.index_operation().cloned() else {
            return Ok(Answer::Product(view));
        };
        let observed =
            backend_client::observe_index_operation_progress(observation, &context, |command| {
                engine.surface(command)
            })
            .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
        let view = backend_present::product_view(&SurfaceReply::IndexOperationStatus(
            observed.observation,
        ))
        .with_index_profile_progress(observed.profile_progress)
        .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
        Ok(Answer::Product(Box::new(view)))
    });
    let restored = engine
        .session
        .set_request_context(original)
        .map_err(|error| Fault::from_client_error(&error, operand.clone()));
    result.and_then(|answer| restored.map(|()| answer))
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
    answer_with_compiler_progress(
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
    answer_with_compiler_progress(
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
    mut admit: impl FnMut(&Answer, Option<&str>) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    let mut engine = SessionEngine {
        session,
        paths,
        recovery,
    };
    let graph = match request {
        Request::GraphPage { selector, limit } => Some((selector, *limit, None)),
        Request::ResumeQuery { query, cursor } => match query.as_ref() {
            Request::GraphPage { selector, limit } => {
                Some((selector, *limit, Some(cursor.as_str())))
            }
            _ => None,
        },
        _ => None,
    };
    if let Some((selector, issued, cursor)) = graph {
        let original = engine.session.request_context().cloned();
        let lifetime = original.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        engine
            .session
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        let result = (|| {
            let selector = match selector {
                backend_library::DeclarationSelector::Coordinate(value) => {
                    backend_library::DeclarationSelector::Coordinate(
                        backend_present::resolve_declaration(&mut engine, value, "graph")?,
                    )
                }
                selected => selected.clone(),
            };
            graph_with_output_admission(
                &mut engine,
                &selector,
                issued,
                cursor,
                || {
                    lifetime.check().map_err(|error| {
                        Fault::from_client_error(&error, backend_present::Operand::Whole)
                    })
                },
                |engine, answer| {
                    let exported = answer
                        .continuation()
                        .map(|continuation| {
                            if !engine.session.has_portable_query_continuation(continuation) {
                                return Err(ClientError::Protocol(
                                    "graph successor lacks its portable owner proof".to_owned(),
                                ));
                            }
                            engine.session.encode_query_continuation(continuation)
                        })
                        .transpose()
                        .map_err(|error| {
                            Fault::from_client_error(
                                &error,
                                backend_present::Operand::Argument("cursor".to_owned()),
                            )
                        })?;
                    admit(answer, exported.as_deref())
                },
            )
        })();
        let restored = engine
            .session
            .set_request_context(original)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole));
        return result.and_then(|answer| restored.map(|()| answer));
    }
    let outline = match request {
        Request::Outline(path) => Some((path.as_str(), backend_library::QueryLimit::MAX, None)),
        Request::OutlinePage { path, limit } => Some((path.as_str(), *limit, None)),
        Request::ResumeQuery { query, cursor } => match query.as_ref() {
            Request::OutlinePage { path, limit } => {
                Some((path.as_str(), *limit, Some(cursor.as_str())))
            }
            _ => None,
        },
        _ => None,
    };
    if let Some((path, issued, cursor)) = outline {
        let original = engine.session.request_context().cloned();
        let lifetime = original.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| {
                Fault::from_client_error(&error, backend_present::Operand::Path(path.to_owned()))
            })?;
        engine
            .session
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| {
                Fault::from_client_error(&error, backend_present::Operand::Path(path.to_owned()))
            })?;
        let result = outline_with_output_admission(
            &mut engine,
            path,
            issued,
            cursor,
            || {
                lifetime.check().map_err(|error| {
                    Fault::from_client_error(
                        &error,
                        backend_present::Operand::Path(path.to_owned()),
                    )
                })
            },
            |engine, answer| {
                let exported = answer
                    .continuation()
                    .map(|continuation| {
                        if !engine.session.has_portable_query_continuation(continuation) {
                            return Err(ClientError::Protocol(
                                "outline successor lacks its portable owner proof".to_owned(),
                            ));
                        }
                        engine.session.encode_query_continuation(continuation)
                    })
                    .transpose()
                    .map_err(|error| {
                        Fault::from_client_error(
                            &error,
                            backend_present::Operand::Argument("cursor".to_owned()),
                        )
                    })?;
                admit(answer, exported.as_deref())
            },
        );
        let restored = engine
            .session
            .set_request_context(original)
            .map_err(|error| {
                Fault::from_client_error(&error, backend_present::Operand::Path(path.to_owned()))
            });
        return result.and_then(|answer| restored.map(|()| answer));
    }
    let Request::Surface(command) = request else {
        return answer_with_compiler_progress(&mut engine, request);
    };
    if let SurfaceCommand::SemanticVersions {
        package,
        limit,
        cursor,
    } = command.as_ref()
    {
        let original_context = engine.session.request_context().cloned();
        let lifetime = original_context.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        engine
            .session
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        let result = versions_with_output_admission(
            &mut engine,
            package,
            *limit,
            cursor.as_ref(),
            || {
                lifetime.check().map_err(|error| {
                    Fault::from_client_error(&error, backend_present::Operand::Whole)
                })
            },
            |answer| admit(answer, None),
        );
        let restored = engine
            .session
            .set_request_context(original_context)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole));
        return result.and_then(|answer| restored.map(|()| answer));
    }
    if let SurfaceCommand::PackageSourceMembership { request } = command.as_ref() {
        let mut selection = backend_present::PackageSourceMembershipSelection::new(request)?;
        let original_context = engine.session.request_context().cloned();
        let lifetime = original_context.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        engine
            .session
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        let result = membership_with_output_admission(
            &mut engine,
            &mut selection,
            || {
                lifetime.check().map_err(|error| {
                    Fault::from_client_error(&error, backend_present::Operand::Whole)
                })
            },
            |answer| admit(answer, None),
        );
        let restored = engine
            .session
            .set_request_context(original_context)
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole));
        return result.and_then(|answer| restored.map(|()| answer));
    }
    let backend_library::SurfaceCommand::PackageGraphPage { request } = command.as_ref() else {
        return answer_with_compiler_progress(&mut engine, &Request::Surface(command.clone()));
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
        let answer = answer_with_compiler_progress(&mut engine, &request)?;
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
        match admit(&answer, None) {
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

/// Selects complete native outline pages under the original issued query credit.
/// An oversized candidate never advances the authenticated input cursor. Each
/// retry reimports that exact input and changes only its delivery span.
fn outline_with_output_admission<E: Engine>(
    engine: &mut E,
    path: &str,
    issued: u16,
    cursor: Option<&str>,
    mut check: impl FnMut() -> Result<(), Fault>,
    mut admit: impl FnMut(&mut E, &Answer) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    let limit = backend_library::QueryLimit::new(issued)
        .ok_or_else(|| Fault::usage("limit", "invalid outline query credit"))?;
    let moved = || {
        Fault::new(
            backend_present::FaultSlug::Freshness,
            backend_present::Operand::Path(path.to_owned()),
            backend_present::Cause::new(
                backend_present::CauseSlug::Moved,
                "outline source changed during bounded delivery; restart explicitly",
            ),
            backend_present::Affordance::None,
        )
    };
    let mut delivery = issued;
    let mut pinned_revision = None;
    let mut pinned_basis = None;
    let mut pinned_input = None;
    let request = Request::OutlinePage {
        path: path.to_owned(),
        limit: issued,
    };
    let probe = Probe::OutlinePage {
        path,
        limit: issued,
    };
    loop {
        check()?;
        let continuation = cursor
            .map(|token| engine.prepare_query_continuation(token))
            .transpose()
            .map_err(|error| {
                Fault::from_client_error(
                    &error,
                    backend_present::Operand::Argument("cursor".to_owned()),
                )
            })?;
        if pinned_input.is_some_and(|previous| previous != continuation) {
            return Err(Fault::usage(
                "cursor",
                "outline retry changed its authenticated input frontier",
            ));
        }
        pinned_input = Some(continuation);
        let span = backend_library::QueryLimit::new(delivery)
            .ok_or_else(|| Fault::usage("limit", "invalid outline delivery span"))?;
        let window = backend_library::QueryWindow::new(limit, span)
            .ok_or_else(|| Fault::usage("limit", "outline delivery exceeds original credit"))?;
        let reply = engine.probe_page_window(probe, continuation, window);
        check()?;
        let reply = match reply {
            Err(ClientError::ResponseTooLarge { basis: Some(basis) }) => {
                if pinned_revision.is_some_and(|previous| previous != basis) {
                    return Err(moved());
                }
                pinned_revision = Some(basis);
                if delivery > 1 {
                    delivery = delivery.div_ceil(2);
                    continue;
                }
                return Err(backend_present::probe_fault(
                    &ClientError::ResponseTooLarge { basis: Some(basis) },
                    probe,
                ));
            }
            Err(error) => return Err(backend_present::probe_fault(&error, probe)),
            Ok(reply) => reply,
        };
        let answer = backend_present::catalog_page_answer(&request, reply)?;
        let Answer::Outline(tree) = &answer else {
            return Err(Fault::usage("outline", "native outline page changed shape"));
        };
        let basis = tree
            .page_basis()
            .ok_or_else(|| Fault::usage("outline", "native outline omitted its source basis"))?;
        let revision = basis.0.root.into();
        if pinned_revision.is_some_and(|previous| previous != revision)
            || pinned_basis.is_some_and(|previous| previous != basis)
        {
            return Err(moved());
        }
        pinned_revision = Some(revision);
        pinned_basis = Some(basis);
        let result = admit(engine, &answer);
        check()?;
        match result {
            Ok(()) => return Ok(answer),
            Err(fault)
                if delivery > 1
                    && fault.cause().slug() == backend_present::CauseSlug::Oversized =>
            {
                delivery = delivery.div_ceil(2);
            }
            Err(fault) => return Err(fault),
        }
    }
}

/// Preserves native graph credit, input frontier and source through full output admission.
fn graph_with_output_admission<E: Engine>(
    engine: &mut E,
    selector: &backend_library::DeclarationSelector,
    issued: u16,
    cursor: Option<&str>,
    mut check: impl FnMut() -> Result<(), Fault>,
    mut admit: impl FnMut(&mut E, &Answer) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    let limit = backend_library::QueryLimit::new(issued)
        .ok_or_else(|| Fault::usage("limit", "invalid graph query credit"))?;
    let moved = || {
        Fault::new(
            backend_present::FaultSlug::Freshness,
            Probe::Declaration {
                selector,
                read: backend_library::DeclarationRead::Graph,
            }
            .operand(),
            backend_present::Cause::new(
                backend_present::CauseSlug::Moved,
                "graph source changed during bounded delivery; restart explicitly",
            ),
            backend_present::Affordance::None,
        )
    };
    let mut delivery = issued;
    let mut pinned_revision = None;
    let mut pinned_basis = None;
    let mut pinned_input = None;
    let request = Request::GraphPage {
        selector: selector.clone(),
        limit: issued,
    };
    let probe = Probe::Declaration {
        selector,
        read: backend_library::DeclarationRead::Graph,
    };
    loop {
        check()?;
        let continuation = cursor
            .map(|token| engine.prepare_query_continuation(token))
            .transpose()
            .map_err(|error| {
                Fault::from_client_error(
                    &error,
                    backend_present::Operand::Argument("cursor".to_owned()),
                )
            })?;
        if pinned_input.is_some_and(|previous| previous != continuation) {
            return Err(Fault::usage(
                "cursor",
                "graph retry changed its authenticated input frontier",
            ));
        }
        pinned_input = Some(continuation);
        let span = backend_library::QueryLimit::new(delivery)
            .ok_or_else(|| Fault::usage("limit", "invalid graph delivery span"))?;
        let window = backend_library::QueryWindow::new(limit, span)
            .ok_or_else(|| Fault::usage("limit", "graph delivery exceeds original credit"))?;
        let reply = engine.declaration_graph_window(selector, window, continuation);
        check()?;
        let reply = match reply {
            Err(ClientError::ResponseTooLarge { basis: Some(basis) }) => {
                if pinned_revision.is_some_and(|previous| previous != basis) {
                    return Err(moved());
                }
                pinned_revision = Some(basis);
                if delivery > 1 {
                    delivery = delivery.div_ceil(2);
                    continue;
                }
                return Err(backend_present::probe_fault(
                    &ClientError::ResponseTooLarge { basis: Some(basis) },
                    probe,
                ));
            }
            Err(error) => return Err(backend_present::probe_fault(&error, probe)),
            Ok(reply) => reply,
        };
        let answer = backend_present::catalog_page_answer(&request, reply)?;
        let Answer::Records(records) = &answer else {
            return Err(Fault::usage("graph", "native graph page changed shape"));
        };
        let basis = records
            .page_basis()
            .ok_or_else(|| Fault::usage("graph", "native graph omitted its source basis"))?;
        let revision = basis.0.root.into();
        if pinned_revision.is_some_and(|previous| previous != revision)
            || pinned_basis.is_some_and(|previous| previous != basis)
        {
            return Err(moved());
        }
        pinned_revision = Some(revision);
        pinned_basis = Some(basis);
        let result = admit(engine, &answer);
        check()?;
        match result {
            Ok(()) => return Ok(answer),
            Err(fault)
                if delivery > 1
                    && fault.cause().slug() == backend_present::CauseSlug::Oversized =>
            {
                delivery = delivery.div_ceil(2);
            }
            Err(fault) => return Err(fault),
        }
    }
}

#[cfg(test)]
#[path = "tests/outline_admission.rs"]
mod outline_admission_tests;

#[cfg(test)]
#[path = "tests/graph_window_admission.rs"]
mod graph_window_admission_tests;

/// The first native descriptor page may shrink under actual output admission;
/// a continuation's authenticated credit is never changed. Each attempt keeps
/// the same source/compiler root pair, total and original request lifetime.
fn versions_with_output_admission(
    engine: &mut dyn Engine,
    package: &backend_library::PackageReference,
    limit: u16,
    cursor: Option<&backend_library::SemanticVersionsCursor>,
    mut check: impl FnMut() -> Result<(), Fault>,
    mut admit: impl FnMut(&Answer) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    let mut effective = limit;
    let mut pinned = None;
    loop {
        check()?;
        let command = SurfaceCommand::SemanticVersions {
            package: package.clone(),
            limit: effective,
            cursor: cursor.cloned(),
        };
        command
            .admit()
            .map_err(|error| Fault::admission(error, backend_present::Operand::Whole))?;
        let reply = engine
            .surface(command.clone())
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        backend_library::admit_surface_reply(&command, &reply).map_err(|error| {
            Fault::from_client_error(
                &ClientError::Protocol(error.to_string()),
                backend_present::Operand::Whole,
            )
        })?;
        let SurfaceReply::SemanticVersionsPage(page) = &reply else {
            return Err(Fault::from_client_error(
                &ClientError::Protocol("native semantic versions page changed shape".to_owned()),
                backend_present::Operand::Whole,
            ));
        };
        let basis = (
            page.source_relation_root,
            page.semantic_relation_root,
            page.total,
        );
        if pinned.is_some_and(|expected| expected != basis) {
            return Err(Fault::from_client_error(&ClientError::Protocol("semantic version roots changed while selecting a bounded page; restart discovery explicitly".to_owned()), backend_present::Operand::Whole));
        }
        pinned = Some(basis);
        let answer = Answer::Product(Box::new(backend_present::product_view_for_owned_command(
            &command, reply,
        )));
        let result = admit(&answer);
        check()?;
        match result {
            Ok(()) => return Ok(answer),
            Err(fault)
                if cursor.is_none()
                    && effective > 1
                    && fault.cause().slug() == backend_present::CauseSlug::Oversized =>
            {
                effective = effective.div_ceil(2);
            }
            Err(fault) => return Err(fault),
        }
    }
}

fn membership_with_output_admission(
    engine: &mut dyn Engine,
    selection: &mut backend_present::PackageSourceMembershipSelection,
    mut check: impl FnMut() -> Result<(), Fault>,
    mut admit: impl FnMut(&Answer) -> Result<(), Fault>,
) -> Result<Answer, Fault> {
    loop {
        check()?;
        let command = SurfaceCommand::PackageSourceMembership {
            request: selection.request().clone(),
        };
        let reply = engine
            .surface(command.clone())
            .map_err(|error| Fault::from_client_error(&error, backend_present::Operand::Whole))?;
        selection.admit_reply(&reply)?;
        let answer = Answer::Product(Box::new(backend_present::product_view_for_owned_command(
            &command, reply,
        )));
        let result = admit(&answer);
        check()?;
        match result {
            Ok(()) => return Ok(answer),
            Err(fault) if selection.shrink_after(&fault) => {}
            Err(fault) => return Err(fault),
        }
    }
}

#[cfg(test)]
#[path = "tests/membership_admission.rs"]
mod membership_admission_tests;

#[cfg(test)]
#[path = "tests/versions_admission.rs"]
mod versions_admission_tests;

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
                |answer, cursor| crate::render::admit_answer_with_cursor(answer, &options, cursor),
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
