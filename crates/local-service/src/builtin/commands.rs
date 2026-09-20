//! Product command admission and the atomic intent/view/projection pipeline.

use super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError, BuiltinSourceChange,
    BuiltinValidator, BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord,
    WireCertificate, WireClaim, WorkspaceModel, ingest, projection, publish_builtin_view,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn index_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let project_key = package.to_bytes();
    let before = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?;
    let (old_files, old_facts, old_graph_version) = match before.as_ref() {
        Some(record) => record
            .project_fields()
            .map(|fields| {
                (
                    fields.files.to_vec(),
                    fields.facts.to_vec(),
                    fields.graph_version,
                )
            })
            .ok_or_else(|| {
                BuiltinModelError("project key contains a source file record".to_owned())
            })?,
        None => (Vec::new(), Vec::new(), [0; 32]),
    };
    let mut reusable = BTreeMap::new();
    for key in &old_files {
        let record = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read reusable source file: {error}")))?
            .ok_or_else(|| {
                BuiltinModelError("project frontier refers to a missing source file".to_owned())
            })?;
        if record.file_fields().is_none() {
            return Err(BuiltinModelError(
                "project frontier refers to a non-file record".to_owned(),
            ));
        }
        reusable.insert(*key, record);
    }
    let mut reusable_facts = BTreeMap::new();
    for key in &old_facts {
        let record = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read reusable package fact: {error}")))?
            .ok_or_else(|| {
                BuiltinModelError("project frontier refers to a missing package fact".to_owned())
            })?;
        if record.project_fields().is_some() || record.file_fields().is_some() {
            return Err(BuiltinModelError(
                "project fact frontier refers to a source record".to_owned(),
            ));
        }
        reusable_facts.insert(*key, record);
    }
    let scan = ingest::scan_project_reusing_graph(
        label,
        project_key,
        &reusable,
        old_graph_version,
        &reusable_facts,
    )
    .map_err(BuiltinModelError)?;
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let fact_keys = scan.facts.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project_with_graph_facts(
        label,
        scan.source_version,
        scan.graph_version,
        file_keys.clone(),
        fact_keys.clone(),
    )
    .map_err(BuiltinModelError)?;
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project),
        });
    }
    for (key, record) in scan.files {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed source file: {error}")))?;
        if current.as_ref() != Some(&record) {
            changes.push(BuiltinSourceChange {
                key,
                after: Some(record),
            });
        }
    }
    for (key, record) in scan.facts {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed package fact: {error}")))?;
        if current.as_ref() != Some(&record) {
            changes.push(BuiltinSourceChange {
                key,
                after: Some(record),
            });
        }
    }
    let selected = file_keys
        .into_iter()
        .chain(fact_keys)
        .collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .into_iter()
            .chain(old_facts)
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    if changes.is_empty() {
        return Ok(None);
    }
    BuiltinIntent::index(package, label, changes).map(Some)
}

fn remove_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let Some(record) = relation
        .lookup(&package.to_bytes())
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?
    else {
        return Ok(None);
    };
    let frontiers = record
        .project_fields()
        .map(|fields| {
            fields
                .files
                .iter()
                .chain(fields.facts)
                .copied()
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| BuiltinModelError("project key contains a source file record".to_owned()))?;
    BuiltinIntent::remove_project(package, label, &frontiers).map(Some)
}

pub(super) fn command_adapter(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    sql_projection: &mut backend_extension_turso::TursoProjection,
    body: &[u8],
) -> Result<Vec<u8>, BuiltinModelError> {
    let request = backend_engine::decode_command_dto(body)
        .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
    let request_id = request.request_id;
    let request_certificate = request.certificate().cloned();
    let command = request.command;
    let (reply, certificate) = match command {
        Command::Add { package } => {
            let label = certified_package_label(request_certificate.as_ref(), package)?;
            let intent = if Path::new(&label).is_dir() {
                index_project_intent(daemon, package, &label)?
            } else {
                Some(BuiltinIntent::add(package, label.clone())?)
            };
            if let Some(intent) = intent {
                commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                    BuiltinModelError(format!("commit product source intent: {error}"))
                })?;
            }
            // View publication is a separately durable projection. Reconcile
            // it even when the source intent is already present so a retry
            // heals a crash or bounded projection failure after source commit.
            let deltas = publish_builtin_view(daemon).map_err(|error| {
                BuiltinModelError(format!("publish product source view: {error}"))
            })?;
            project_view_deltas(sql_projection, daemon, &deltas)?;
            let intent_id = backend_engine::intent_id("request_package", package.as_bytes());
            let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
                id: backend_engine::encode_id(intent_id.as_bytes()),
                token: "request_package".to_owned(),
                payload: package.as_bytes().to_vec().into_boxed_slice(),
            });
            (CommandReply::Added(intent_id), Some(certificate))
        }
        Command::Remove { package } => {
            let label = certified_package_label(request_certificate.as_ref(), package)?;
            if let Some(intent) = remove_project_intent(daemon, package, &label)? {
                commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                    BuiltinModelError(format!("commit product source intent: {error}"))
                })?;
            }
            let deltas = publish_builtin_view(daemon).map_err(|error| {
                BuiltinModelError(format!("publish product source view: {error}"))
            })?;
            project_view_deltas(sql_projection, daemon, &deltas)?;
            let intent_id = backend_engine::intent_id("remove_package", package.as_bytes());
            let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
                id: backend_engine::encode_id(intent_id.as_bytes()),
                token: "remove_package".to_owned(),
                payload: package.as_bytes().to_vec().into_boxed_slice(),
            });
            (CommandReply::Removed(intent_id), Some(certificate))
        }
        command => {
            let reply = match daemon.engine().daemon().library().execute(command.clone()) {
                Ok(reply) => reply,
                Err(error) => CommandReply::Error(error.to_string()),
            };
            let certificate = projection::reply_certificate(
                &command,
                &reply,
                daemon.engine().daemon().library().view(),
                daemon.engine().daemon().library().cursor(),
                request_certificate,
            )?;
            (reply, certificate)
        }
    };
    let mut reply = match reply {
        CommandReply::Health(root) => backend_engine::ReplyDto::health(
            request_id,
            root,
            daemon.engine().daemon().library().cursor(),
        ),
        reply => backend_engine::ReplyDto::new(request_id, reply),
    };
    if let Some(certificate) = certificate {
        reply = reply.with_certificate(certificate);
    }
    backend_engine::encode_reply_dto(&reply).map_err(BuiltinModelError)
}

fn project_view_deltas(
    projection: &mut backend_extension_turso::TursoProjection,
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    deltas: &[backend_engine::CommittedViewDelta],
) -> Result<(), BuiltinModelError> {
    if deltas.is_empty() {
        return Ok(());
    }
    if let Err(incremental_error) = futures_executor::block_on(projection.apply_all(deltas)) {
        futures_executor::block_on(
            projection.synchronize(daemon.engine().daemon().library().view()),
        )
        .map_err(|rebuild_error| {
            BuiltinModelError(format!(
                "Turso projection delta failed ({incremental_error}); rebuild failed: {rebuild_error}"
            ))
        })?;
    }
    Ok(())
}

fn certified_package_label(
    certificate: Option<&WireCertificate>,
    package: backend_engine::PackageKey,
) -> Result<String, BuiltinModelError> {
    let id = backend_engine::encode_id(package.as_bytes());
    let certificate = certificate
        .ok_or_else(|| BuiltinModelError("package command certificate is missing".to_owned()))?;
    let mut value = None;
    for claim in &certificate.claims {
        if let WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: claimed,
            value: text,
        } = claim
            && claimed == &id
        {
            if value.is_some() {
                return Err(BuiltinModelError(
                    "package command certificate has duplicate claims".to_owned(),
                ));
            }
            value = Some(text.clone());
        }
    }
    value.ok_or_else(|| {
        BuiltinModelError("package command certificate has no canonical package text".to_owned())
    })
}

fn commit_builtin_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    request_id: u64,
    intent: &BuiltinIntent,
) -> Result<(), BuiltinModelError> {
    let request = BuiltinModel.request_id(intent);
    let expected = daemon.engine().daemon().owner().head().expectation();
    let receiver = daemon
        .client()
        .request(
            request_id,
            crate::Request::Commit {
                request,
                expected,
                intent: intent.clone(),
            },
        )
        .map_err(|error| BuiltinModelError(format!("queue builtin intent: {error:?}")))?;
    if !daemon.serve_one() {
        return Err(BuiltinModelError(
            "builtin intent owner did not make progress".to_owned(),
        ));
    }
    match crate::service::wait_for_daemon_reply(daemon, &receiver)
        .map_err(|error| BuiltinModelError(error.to_string()))?
    {
        backend_engine::DaemonReply::Commit(Ok(_)) => Ok(()),
        backend_engine::DaemonReply::Commit(Err(error)) => {
            Err(BuiltinModelError(error.to_string()))
        }
        _ => Err(BuiltinModelError(
            "builtin intent was sent to the wrong owner lane".to_owned(),
        )),
    }
}
