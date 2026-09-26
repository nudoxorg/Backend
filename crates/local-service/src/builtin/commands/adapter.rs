use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use super::diff::execute_semantic_diff;
use super::graph::{execute_certified_graph_query, execute_search};
use super::index::{
    index_project_intent, index_project_intent_at, remove_project_intent, semantic_version_record,
    semantic_versions,
};
use super::semantic_query::{
    execute_references, execute_semantic_graph, execute_structural_call_graph,
};
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{ProductSemanticPublicationKey, ProductSemanticPublicationRecord};
use backend_library::interface::PackageUrl;
use std::path::Path;

type ProductDaemon = crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>;
type AdmittedReply = (CommandReply, Option<WireCertificate>);

pub(in crate::builtin) struct CommandAdapter {
    sql_projection: backend_extension_turso::TursoProjection,
    registry: Option<RegistryGateway>,
    product_state: super::super::ProductState,
    compiler: LocalCompilerClient,
    search_snapshots: super::super::query::SearchSnapshotOwner,
    remote_semantic: super::super::query::RemoteSemantic,
    published: Option<super::super::view_publish::PublishedRoots>,
}

impl CommandAdapter {
    pub(in crate::builtin) fn new(
        sql_projection: backend_extension_turso::TursoProjection,
        registry: Option<RegistryGateway>,
        product_state: super::super::ProductState,
        compiler: LocalCompilerClient,
        search_snapshots: super::super::query::SearchSnapshotOwner,
        remote_semantic: super::super::query::RemoteSemantic,
        published: Option<super::super::view_publish::PublishedRoots>,
    ) -> Self {
        Self {
            sql_projection,
            registry,
            product_state,
            compiler,
            search_snapshots,
            remote_semantic,
            published,
        }
    }

    pub(in crate::builtin) fn execute(
        &mut self,
        daemon: &mut ProductDaemon,
        body: &[u8],
    ) -> Result<Vec<u8>, BuiltinModelError> {
        let owner = daemon.engine().daemon().library().cursor();
        let request = backend_engine::decode_command_dto_for_owner(body, owner)
            .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
        let request_id = request.request_id;
        let certificate = request.certificate().cloned();
        let (reply, certificate) =
            self.dispatch(daemon, request.command, certificate, request_id)?;
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

    fn dispatch(
        &mut self,
        daemon: &mut ProductDaemon,
        command: Command,
        certificate: Option<WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        match command {
            Command::Add { package } => self.add(daemon, package, certificate.as_ref(), request_id),
            Command::Remove { package } => {
                self.remove(daemon, package, certificate.as_ref(), request_id)
            }
            Command::Search(query) => self.search(daemon, &query, certificate),
            Command::Graph(query) => self.graph(daemon, query, certificate, false),
            Command::Related(query) => self.graph(daemon, query, certificate, true),
            Command::GraphQuery(request) => {
                execute_certified_graph_query(daemon, &self.compiler, &request, certificate)
            }
            Command::Surface(surface) => self.surface(daemon, surface, request_id),
            command => self.standard(daemon, &command, certificate),
        }
    }

    fn add(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let intent = if Path::new(&label).is_dir() {
            index_project_intent(daemon, package, &label, request_id, &self.compiler)?
        } else if label.starts_with("pkg:") || label.starts_with("PKG:") {
            self.registry_intent(daemon, package, &label, request_id)?
        } else {
            Some(BuiltinIntent::add(package, label.clone())?)
        };
        let committed = if let Some(intent) = intent {
            commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                BuiltinModelError(format!("commit product source intent: {error}"))
            })?;
            Some(intent)
        } else {
            None
        };
        self.publish_view(daemon, committed.as_ref())?;
        let intent_id = backend_engine::intent_id("request_package", requested_package.as_bytes());
        let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: backend_engine::encode_id(intent_id.as_bytes()),
            token: "request_package".to_owned(),
            payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
        });
        Ok((CommandReply::Added(intent_id), Some(certificate)))
    }

    fn registry_intent(
        &mut self,
        daemon: &ProductDaemon,
        package: backend_engine::PackageKey,
        label: &str,
        request_id: u64,
    ) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
        let coordinate =
            backend_engine::registry::PackageCoordinate::parse(label).map_err(|_| {
                BuiltinModelError(
                    "add target must be an admitted local directory or version-pinned package URL"
                        .to_owned(),
                )
            })?;
        if coordinate.as_str() != label {
            return Err(BuiltinModelError(
                "package URL is not in canonical form".to_owned(),
            ));
        }
        let Some(gateway) = self.registry.as_mut() else {
            // The coordinate itself is still an admitted, exact package
            // identity. Retain it with an explicit compiler-authority
            // unavailable terminal so every downstream surface observes the
            // semantic plane and no source-shaped placeholder is presented as
            // compiler truth. A configured registry continues through the
            // acquisition and compiler-authority path below.
            return BuiltinIntent::add(package, label.to_owned()).map(Some);
        };
        let archive = gateway
            .acquire(&coordinate)
            .map_err(|error| BuiltinModelError(format!("registry add: {error}")))?;
        let staged = gateway
            .stage_archive(&coordinate, &archive)
            .map_err(|error| BuiltinModelError(format!("stage registry archive: {error}")))?;
        index_project_intent_at(
            daemon,
            package,
            label,
            staged.path(),
            Some(&coordinate),
            request_id,
            &self.compiler,
        )
    }

    /// Resolves a document by the canonical coordinate carried in the
    /// caller's admitted key claim. Semantic rows use a compiler-owned symbol
    /// identity rather than `symbol_key(label)`, while the public document
    /// command intentionally accepts the coordinate an agent copied from a
    /// search page. The claim supplies that preimage, so resolve the row by
    /// exact label after checking the request root instead of returning a
    /// false not-found for a published semantic declaration.
    fn canonical_claim_document(
        daemon: &ProductDaemon,
        query: &backend_library::DocumentQuery,
        certificate: Option<&WireCertificate>,
    ) -> Option<backend_library::Document> {
        let label = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && query.symbol().matches(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        })?;
        let library = daemon.engine().daemon().library();
        let root = library.revision_root();
        if !query.basis().matches(root) {
            return None;
        }
        let row =
            library.view().rows().iter().find(|row| {
                row.label == label && matches!(row.id, backend_engine::RowId::Symbol(_))
            })?;
        let backend_engine::RowId::Symbol(_) = row.id else {
            return None;
        };
        let symbol = backend_library::symbol_key(label);
        let source_basis = backend_library::Basis {
            root,
            ..library.view().basis()
        };
        let mut document = backend_library::Document::new(symbol, root, row.document.clone())
            .with_source_basis(source_basis)
            .with_location(row.source.clone())
            .with_excerpt(row.excerpt.clone());
        document.signature.clone_from(&row.signature);
        Some(document)
    }

    fn remove(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let committed = if let Some(intent) = remove_project_intent(daemon, package, &label)? {
            commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                BuiltinModelError(format!("commit product source intent: {error}"))
            })?;
            Some(intent)
        } else {
            None
        };
        self.publish_view(daemon, committed.as_ref())?;
        let intent_id = backend_engine::intent_id("remove_package", requested_package.as_bytes());
        let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
            id: backend_engine::encode_id(intent_id.as_bytes()),
            token: "remove_package".to_owned(),
            payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
        });
        Ok((CommandReply::Removed(intent_id), Some(certificate)))
    }

    fn publish_view(
        &mut self,
        daemon: &mut ProductDaemon,
        edit: Option<&BuiltinIntent>,
    ) -> Result<(), BuiltinModelError> {
        // Reconcile even after a no-op source intent so a retry heals a crash
        // between the durable source commit and its derived view publication.
        // A missing witness, or an edit that is not the transition adjacent to
        // it, still hydrates the selected relations.
        let deployment = super::super::SemanticDeployment::from_remote(&self.remote_semantic);
        let filesystem_workspace = self
            .product_state
            .workspace_path()
            .map_err(BuiltinModelError)?;
        let outcome = publish_builtin_view(
            daemon,
            &self.compiler,
            deployment,
            filesystem_workspace,
            self.published.as_ref(),
            edit,
        )
        .map_err(|error| BuiltinModelError(format!("publish product source view: {error}")))?;
        self.published = Some(outcome.roots);
        project_view_deltas(&mut self.sql_projection, daemon, &outcome.deltas)
    }

    fn search(
        &mut self,
        daemon: &ProductDaemon,
        query: &backend_engine::Query,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = Command::Search(query.clone());
        let reply = execute_search(
            daemon,
            &self.compiler,
            &mut self.search_snapshots,
            &mut self.remote_semantic,
            query,
        )
        .unwrap_or_else(|error| CommandReply::Error(error.to_string()));
        Self::certify(daemon, &command, reply, certificate)
    }

    fn graph(
        &self,
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<WireCertificate>,
        include_incoming: bool,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = if include_incoming {
            Command::Related(query)
        } else {
            Command::Graph(query)
        };
        let query = Self::claimed_graph_source(daemon, query, certificate.as_ref());
        let reply = match execute_semantic_graph(daemon, &self.compiler, query, include_incoming)? {
            Some(snapshot) => CommandReply::Graph(snapshot),
            None => match execute_structural_call_graph(daemon, query, include_incoming)? {
                Some(snapshot) => CommandReply::Graph(snapshot),
                None => daemon
                    .engine()
                    .daemon()
                    .library()
                    .execute(command.clone())
                    .unwrap_or_else(|error| CommandReply::Failed(error.into())),
            },
        };
        Self::certify(daemon, &command, reply, certificate)
    }

    /// Resolves a graph source named by the canonical coordinate a caller
    /// copied from a result page.
    ///
    /// A client addresses a declaration by `symbol_key(coordinate)`, which is
    /// the row key of a structural declaration but not of a semantic one: a
    /// compiler-backed row is keyed by its compiler-owned identity. Like
    /// `canonical_claim_document`, this reads the coordinate from the
    /// caller's admitted key claim and, when no view row carries the
    /// requested key, selects the one row whose label is exactly that
    /// coordinate. Without it every `graph` and `related` request for a
    /// semantic declaration failed with "semantic graph source is absent".
    fn claimed_graph_source(
        daemon: &ProductDaemon,
        query: backend_engine::GraphNeighborhoodQuery,
        certificate: Option<&WireCertificate>,
    ) -> backend_engine::GraphNeighborhoodQuery {
        let library = daemon.engine().daemon().library();
        let view = library.view();
        let requested = query.resolve_symbol(view);
        if requested.is_some_and(|symbol| view.row(backend_engine::RowId::Symbol(symbol)).is_some())
            || !query.basis().matches(library.revision_root())
        {
            return query;
        }
        let Some(label) = certificate.and_then(|certificate| {
            certificate.claims.iter().find_map(|claim| match claim {
                WireClaim::Key {
                    schema: backend_engine::WireSchema::Symbol,
                    id,
                    value,
                } if id
                    == &backend_engine::encode_id(backend_engine::symbol_key(value).as_bytes())
                    && requested == Some(backend_engine::symbol_key(value)) =>
                {
                    Some(value.as_str())
                }
                _ => None,
            })
        }) else {
            return query;
        };
        view.rows()
            .iter()
            .find_map(|row| match row.id {
                backend_engine::RowId::Symbol(symbol) if row.label == label => Some(symbol),
                _ => None,
            })
            .map_or(query, |symbol| query.with_resolved_symbol(symbol))
    }

    fn surface(
        &mut self,
        daemon: &mut ProductDaemon,
        surface: backend_engine::SurfaceCommand,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match surface {
            backend_engine::SurfaceCommand::References { target } => {
                execute_references(daemon, &self.compiler, &target).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    CommandReply::Surface,
                )
            }
            backend_engine::SurfaceCommand::Diff { from, to } => {
                execute_semantic_diff(daemon, &self.compiler, &from, &to).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |rows| CommandReply::Surface(backend_engine::SurfaceReply::Diff(rows)),
                )
            }
            backend_engine::SurfaceCommand::SemanticVersions { package } => {
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root());
                semantic_versions(daemon, &package, workspace)
            }
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                |records| {
                    CommandReply::Surface(backend_engine::SurfaceReply::SemanticVersions(records))
                },
            ),
            backend_engine::SurfaceCommand::SelectSemanticVersion {
                package,
                coordinate,
                profile,
                generation,
            } => self
                .select_semantic_version(
                    daemon, package, coordinate, profile, generation, request_id,
                )
                .map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(
                            backend_engine::SurfaceReply::SemanticVersionSelected(record),
                        )
                    },
                ),
            surface => {
                let catalog = self
                    .registry
                    .as_mut()
                    .map_or(Ok(Vec::new()), RegistryGateway::catalog)
                    .map_err(BuiltinModelError)?;
                let mut dependency_facts = self
                    .registry
                    .as_mut()
                    .map_or_else(Vec::new, RegistryGateway::dependency_facts);
                let indexed = super::super::read_indexed_sources(
                    &daemon.engine().daemon().owner().snapshot(),
                )?;
                for project in indexed.projects.values() {
                    if project.label.starts_with("pkg:") {
                        continue;
                    }
                    let project_root = std::path::Path::new(&project.label);
                    if !project_root.is_dir() {
                        continue;
                    }
                    match super::super::local_manifest::local_dependency_facts(project_root) {
                        Ok(Some(fact)) => dependency_facts.push(fact),
                        Ok(None) => {}
                        Err(error) => return Err(BuiltinModelError(error)),
                    }
                }
                futures_executor::block_on(self.sql_projection.synchronize_package_graph(
                    daemon.engine().daemon().library().view().root(),
                    &dependency_facts,
                ))
                .map_err(|error| {
                    BuiltinModelError(format!("align package graph projection: {error}"))
                })?;
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root());
                self.product_state
                    .execute(
                        surface,
                        daemon.engine().daemon().library().view(),
                        &catalog,
                        &dependency_facts,
                        workspace,
                    )
                    .map_or_else(
                        |error| {
                            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                                error,
                            ))
                        },
                        CommandReply::Surface,
                    )
            }
        };
        Ok((reply, None))
    }

    fn select_semantic_version(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageReference,
        coordinate: PackageUrl,
        profile: backend_engine::SemanticLanguageProfile,
        generation: backend_engine::SemanticGenerationId,
        request_id: u64,
    ) -> Result<backend_engine::SemanticVersionRecord, BuiltinModelError> {
        let profile = profile
            .profile()
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let coordinate_text = coordinate.as_str().to_owned();
        let package_key = backend_engine::package_key(package.as_str());
        let selected_key = ProductSemanticPublicationKey::new(package.clone(), coordinate, profile)
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
        if let Ok(history_key) = selected_key.for_generation_bytes(generation.to_bytes()) {
            let relation = daemon
                .engine()
                .daemon()
                .owner()
                .snapshot()
                .relation::<BuiltinSemanticRelation>()
                .map_err(|error| {
                    BuiltinModelError(format!("open semantic version history: {error}"))
                })?;
            let history_record = relation.lookup(&history_key).map_err(|error| {
                BuiltinModelError(format!("read semantic version history: {error}"))
            })?;
            if let Some(record) = history_record {
                history_key.admit_record(&record).map_err(|error| {
                    BuiltinModelError(format!("admit selected semantic generation: {error}"))
                })?;
                let ProductSemanticPublicationRecord::Published { coverage, claim } =
                    record.clone()
                else {
                    return Err(BuiltinModelError(
                        "semantic generation history contains an unavailable terminal".to_owned(),
                    ));
                };
                let before = relation.lookup(&selected_key).map_err(|error| {
                    BuiltinModelError(format!("read selected semantic generation: {error}"))
                })?;
                let committed = if before != Some(record.clone()) {
                    let intent = BuiltinIntent::select_semantic_generation(
                        package_key,
                        package.as_str(),
                        selected_key.clone(),
                        history_key,
                        before,
                        record,
                    )?;
                    commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                        BuiltinModelError(format!("commit semantic generation selection: {error}"))
                    })?;
                    Some(intent)
                } else {
                    None
                };
                self.publish_view(daemon, committed.as_ref())?;
                return Ok(semantic_version_record(
                    &selected_key,
                    coverage,
                    claim,
                    true,
                ));
            }
        }
        let workspace = self
            .registry
            .as_ref()
            .map(|gateway| gateway.workspace_root());
        let view = daemon.engine().daemon().library().view();
        let indexed =
            super::super::product_state::indexed_semantic_versions(view, &package, workspace)
                .map_err(BuiltinModelError)?;
        let selected_language = profile.language();
        indexed
            .iter()
            .find(|row| {
                row.coordinate.as_str() == coordinate_text
                    && row.generation == generation
                    && row
                        .profile
                        .profile()
                        .ok()
                        .is_some_and(|row_profile| row_profile.language() == selected_language)
            })
            .cloned()
            .ok_or_else(|| BuiltinModelError("semantic generation is not retained".to_owned()))
    }

    fn standard(
        &self,
        daemon: &ProductDaemon,
        command: &Command,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let reply = match command {
            Command::Document(query) | Command::Source(query) => {
                Self::canonical_claim_document(daemon, query, certificate.as_ref())
                    .map_or_else(
                        || daemon.engine().daemon().library().execute(command.clone()),
                        |document| Ok(CommandReply::Document(document)),
                    )
                    .unwrap_or_else(|error| CommandReply::Error(error.to_string()))
            }
            _ => daemon
                .engine()
                .daemon()
                .library()
                .execute(command.clone())
                .unwrap_or_else(|error| CommandReply::Error(error.to_string())),
        };
        let reply = semantic_readiness(reply, daemon, &self.remote_semantic, &self.compiler)?;
        Self::certify(daemon, command, reply, certificate)
    }

    fn certify(
        daemon: &ProductDaemon,
        command: &Command,
        reply: CommandReply,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let certificate = projection::reply_certificate(
            command,
            &reply,
            daemon.engine().daemon().library().view(),
            daemon.engine().daemon().library().cursor(),
            certificate,
        )?;
        Ok((reply, certificate))
    }
}

fn semantic_readiness(
    reply: CommandReply,
    daemon: &ProductDaemon,
    remote: &super::super::query::RemoteSemantic,
    compiler: &LocalCompilerClient,
) -> Result<CommandReply, BuiltinModelError> {
    let CommandReply::Readiness(report) = reply else {
        return Ok(reply);
    };
    let local = ingest::semantic_capabilities(report.capabilities(), compiler)
        .map_err(|error| BuiltinModelError(format!("local semantic readiness: {error}")))?;
    let capabilities = remote
        .inventory(&local)
        .map_err(|error| BuiltinModelError(format!("project semantic readiness: {error}")))?;
    // A deployment that configured no embedding provider has a terminally
    // unavailable semantic lane, and a health reply must say so instead of
    // dropping the fact on the floor. The published view root already folds the
    // same classification in, so this reconciliation is a fixed point for every
    // reply the owner certifies: `readiness_certificate` rejects a report whose
    // coverage differs from that root, and the lane is described exactly once.
    let coverage = super::super::reconcile_semantic_lane(
        report.coverage(),
        super::super::SemanticDeployment::from_remote(remote),
    );
    // Progress is read out of the committed source relation rather than out of
    // a counter the scan kept, so it describes the revision this very report
    // names. `readiness_certificate` compares revision, basis, coverage, and
    // row count; the counts are derived from the same owner snapshot, so they
    // cannot disagree with the root the certificate commits to.
    let progress = super::super::ingest_progress(&daemon.engine().daemon().owner().snapshot())?;
    Ok(CommandReply::Readiness(
        backend_engine::HealthReport::from_admitted_parts(
            report.revision(),
            report.basis(),
            coverage.into_boxed_slice(),
            report.row_count(),
            capabilities,
        )
        .with_progress(progress),
    ))
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

fn canonical_local_package(
    package: backend_engine::PackageKey,
    label: String,
) -> Result<(backend_engine::PackageKey, String), BuiltinModelError> {
    let path = Path::new(&label);
    if !path.is_dir() {
        return Ok((package, label));
    }
    let canonical = path.canonicalize().map_err(|error| {
        BuiltinModelError(format!("canonicalize local package {label}: {error}"))
    })?;
    let label = canonical.to_string_lossy().into_owned();
    Ok((backend_engine::package_key(&label), label))
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
