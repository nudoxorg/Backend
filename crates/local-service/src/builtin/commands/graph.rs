use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::super::{read_indexed_sources, view_build};
use super::semantic_query::{SemanticQueryJob, semantic_query_executor};
use backend_engine::application::LocalCompilerClient;
use std::collections::BTreeMap;
use std::sync::mpsc;

fn execute_graph_query(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    request: &backend_engine::GraphQueryRequest,
) -> Result<backend_engine::GraphQueryPage, BuiltinModelError> {
    let owner_cursor = daemon.engine().daemon().library().cursor();
    let owner_view = daemon.engine().daemon().library().view().clone();
    let start = request
        .start_offset(owner_cursor)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    if request.control() == backend_engine::GraphQueryControl::Cancel {
        return Ok(backend_engine::GraphQueryPage {
            revision: request.page().basis(),
            source: owner_view.basis().object,
            rows: Box::new([]),
            terminal: backend_engine::PageTerminal::Cancelled,
        });
    }
    let variables = request
        .variables()
        .iter()
        .map(|(name, value)| graph_value_to_trustfall(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = super::super::read_indexed_sources(&snapshot)?;
    let corpus = super::super::view_build::semantic_query_corpus(&snapshot, compiler, &sources)?;
    let (cancellation, control) = backend_extension_trustfall::SemanticQueryCancellation::new();
    let query = backend_extension_trustfall::SemanticQueryRequest::admit_page(
        corpus,
        request.query().to_owned(),
        variables,
        start,
        usize::from(request.page().limit().get()),
        cancellation,
    )
    .map_err(|error| BuiltinModelError(error.to_string()))?;
    let admitted_identity = query.identity().clone();
    let mut rows = Vec::new();
    let mut terminal = None;
    // Trustfall owns a genuinely asynchronous, lazy stream. Run that stream on
    // one reusable local executor and cross back into locald's synchronous
    // single-owner command loop through a one-event rendezvous. The capacity of
    // one is deliberate: the producer cannot evaluate an unbounded result set
    // ahead of the protocol page consumer, and dropping the receiver publishes
    // cancellation before the asynchronous producer can do more work.
    (|| -> Result<(), BuiltinModelError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        semantic_query_executor()?
            .send(SemanticQueryJob {
                query,
                identity: admitted_identity,
                events: sender,
            })
            .map_err(|_| BuiltinModelError("semantic query executor stopped".to_owned()))?;
        let consumed = (|| {
            while let Ok(event) = receiver.recv() {
                match event.map_err(BuiltinModelError)? {
                    backend_extension_trustfall::SemanticQueryEvent::Row(row)
                        if terminal.is_none() =>
                    {
                        rows.push(graph_row_from_trustfall(row.into_row())?);
                    }
                    backend_extension_trustfall::SemanticQueryEvent::Terminal(value)
                        if terminal.is_none() =>
                    {
                        terminal = Some(value);
                    }
                    _ => {
                        return Err(BuiltinModelError(
                            "structured graph query emitted events after its terminal".to_owned(),
                        ));
                    }
                }
            }
            Ok(())
        })();
        if consumed.is_err() {
            control.cancel();
        }
        drop(receiver);
        consumed
    })()?;
    let terminal = terminal.ok_or_else(|| {
        BuiltinModelError("structured graph query omitted its terminal".to_owned())
    })?;
    if terminal.rows() != rows.len() {
        return Err(BuiltinModelError(
            "structured graph query terminal row count mismatch".to_owned(),
        ));
    }
    let terminal = if terminal.is_complete() {
        backend_engine::PageTerminal::Complete
    } else if terminal.is_cancelled() {
        backend_engine::PageTerminal::Cancelled
    } else if terminal.is_limit_reached() {
        backend_engine::PageTerminal::More(
            request
                .next_continuation(owner_cursor, start.saturating_add(rows.len()))
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        )
    } else {
        return Err(BuiltinModelError(
            "structured graph query returned an unknown terminal".to_owned(),
        ));
    };
    Ok(backend_engine::GraphQueryPage {
        revision: request.page().basis(),
        source: owner_view.basis().object,
        rows: rows.into_boxed_slice(),
        terminal,
    })
}

fn graph_value_to_trustfall(
    value: &backend_engine::GraphValue,
) -> Result<backend_extension_trustfall::FieldValue, BuiltinModelError> {
    Ok(match value {
        backend_engine::GraphValue::Null => backend_extension_trustfall::FieldValue::Null,
        backend_engine::GraphValue::Boolean(value) => (*value).into(),
        backend_engine::GraphValue::Signed(value) => {
            backend_extension_trustfall::FieldValue::Int64(*value)
        }
        backend_engine::GraphValue::Unsigned(value) => {
            backend_extension_trustfall::FieldValue::Uint64(*value)
        }
        backend_engine::GraphValue::Float(bits) => {
            backend_extension_trustfall::FieldValue::Float64(f64::from_bits(*bits))
        }
        backend_engine::GraphValue::String(value) => value.as_str().into(),
        backend_engine::GraphValue::List(values) => backend_extension_trustfall::FieldValue::List(
            values
                .iter()
                .map(graph_value_to_trustfall)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
    })
}

fn graph_row_from_trustfall(
    row: backend_extension_trustfall::SemanticQueryRow,
) -> Result<backend_engine::GraphQueryRow, BuiltinModelError> {
    let fields = row
        .into_iter()
        .map(|(name, value)| {
            graph_value_from_trustfall(value).map(|value| (name.to_string(), value))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    backend_engine::GraphQueryRow::new(fields).map_err(|error| BuiltinModelError(error.to_string()))
}

fn graph_value_from_trustfall(
    value: backend_extension_trustfall::FieldValue,
) -> Result<backend_engine::GraphValue, BuiltinModelError> {
    Ok(match value {
        backend_extension_trustfall::FieldValue::Null => backend_engine::GraphValue::Null,
        backend_extension_trustfall::FieldValue::Boolean(value) => {
            backend_engine::GraphValue::Boolean(value)
        }
        backend_extension_trustfall::FieldValue::Int64(value) => {
            backend_engine::GraphValue::Signed(value)
        }
        backend_extension_trustfall::FieldValue::Uint64(value) => {
            backend_engine::GraphValue::Unsigned(value)
        }
        backend_extension_trustfall::FieldValue::Float64(value) => {
            backend_engine::GraphValue::finite_float(value)
                .map_err(|error| BuiltinModelError(error.to_string()))?
        }
        backend_extension_trustfall::FieldValue::String(value)
        | backend_extension_trustfall::FieldValue::Enum(value) => {
            backend_engine::GraphValue::String(value.to_string())
        }
        backend_extension_trustfall::FieldValue::List(values) => backend_engine::GraphValue::List(
            values
                .iter()
                .cloned()
                .map(graph_value_from_trustfall)
                .collect::<Result<Vec<_>, _>>()?
                .into_boxed_slice(),
        ),
        _ => {
            return Err(BuiltinModelError(
                "structured graph query returned an unsupported value".to_owned(),
            ));
        }
    })
}

pub(super) fn execute_search(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    snapshots: &mut super::super::query::SearchSnapshotOwner,
    remote_semantic: &mut super::super::query::RemoteSemantic,
    query: &backend_engine::Query,
) -> Result<CommandReply, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let coverage = super::super::admitted_coverage()?;
    let sources = super::super::read_indexed_sources(&snapshot)?;
    let semantic_evidence =
        super::super::view_build::semantic_query_corpus(&snapshot, compiler, &sources)?;
    let coordinator = snapshots
        .select(
            snapshot.root(),
            daemon.engine().daemon().library().view().clone(),
            coverage,
            semantic_evidence,
        )
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let local_query =
        super::super::query::LocalQuery::prefix(query.text(), usize::from(query.limit().get()))
            .map_err(|error| BuiltinModelError(error.to_string()))?;
    let local = coordinator
        .search_local(local_query)
        .map_err(|error| BuiltinModelError(error.to_string()))?;
    let _ = remote_semantic.reconcile(coordinator, coverage);
    let result = remote_semantic.search(coordinator, coverage, local, query.text());
    let ranked_ids = result
        .rows
        .iter()
        .map(|ranked| ranked.row.id)
        .collect::<Vec<_>>();
    daemon
        .engine()
        .daemon()
        .library()
        .search_from_ranked_ids(query, &ranked_ids)
        .map(CommandReply::Search)
        .map_err(|error| BuiltinModelError(error.to_string()))
}

pub(super) fn execute_certified_graph_query(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    compiler: &LocalCompilerClient,
    request: &backend_engine::GraphQueryRequest,
    base: Option<WireCertificate>,
) -> Result<(CommandReply, Option<WireCertificate>), BuiltinModelError> {
    let command = Command::GraphQuery(request.clone());
    let reply = execute_graph_query(daemon, compiler, request).map_or_else(
        |error| {
            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                error.to_string(),
            ))
        },
        CommandReply::GraphQueryPage,
    );
    let certificate = projection::reply_certificate(
        &command,
        &reply,
        daemon.engine().daemon().library().view(),
        daemon.engine().daemon().library().cursor(),
        base,
    )?;
    Ok((reply, certificate))
}
