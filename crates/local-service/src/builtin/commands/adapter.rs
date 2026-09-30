use super::super::product_state::CatalogLookupIndex;
use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ForgeGateway, ProductSourceRecord,
    RegistryGateway, WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication,
    ingest, projection, publish_builtin_view,
};
use super::diff::execute_semantic_diff;
use super::graph::{execute_certified_graph_query, execute_search};
use super::index::{
    DeferredIndex, PreparedIndex, PreparedProductSelection, deferred_compile_was_cancelled,
    finish_deferred_index, index_project_intent, index_project_intent_at,
    index_project_intent_with_cluster_and_intent, prepare_index_project, remove_project_intent,
    run_deferred_compile, semantic_version_record, semantic_versions,
};
use super::semantic_query::{
    execute_references, execute_semantic_graph, execute_structural_call_graph,
};
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{ProductSemanticPublicationKey, ProductSemanticPublicationRecord};
use backend_library::CompileExecutionIntent;
use backend_library::interface::PackageUrl;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const MAX_RETAINED_INDEX_TERMINALS: usize = 64;

fn new_index_owner_epoch() -> [u8; 16] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-service.index-owner-epoch.v1\0");
    hasher.update(&std::process::id().to_be_bytes());
    hasher.update(&nanos.to_be_bytes());
    let digest = hasher.finalize();
    let mut epoch = [0_u8; 16];
    epoch.copy_from_slice(&digest.as_bytes()[..16]);
    epoch
}

type ProductDaemon = crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>;
type AdmittedReply = (CommandReply, Option<WireCertificate>);

fn bounded_index_detail(value: impl std::fmt::Display) -> backend_library::ProductText {
    let mut value = value.to_string().replace('\0', " ");
    if value.len() > backend_library::MAX_PRODUCT_TEXT_BYTES {
        let mut boundary = backend_library::MAX_PRODUCT_TEXT_BYTES;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    backend_library::ProductText::new(value)
        .unwrap_or_else(|_| backend_library::ProductText::from_static("index job failed"))
}

const ADD_TARGET_REQUIRED: &str =
    "add target must be an admitted local directory or version-pinned package URL";

#[derive(Debug)]
enum AddTarget {
    LocalDirectory,
    PackageUrl,
}

/// Classifies an add label before any project record is built.
///
/// A directory is indexed in place. A version-pinned package URL goes through
/// registry acquisition. A file, a symlink to a file, or a missing path is
/// refused here so it cannot become an empty project record.
fn classify_add_target(label: &str) -> Result<AddTarget, BuiltinModelError> {
    if Path::new(label).is_dir() {
        Ok(AddTarget::LocalDirectory)
    } else if label.starts_with("pkg:") || label.starts_with("PKG:") {
        Ok(AddTarget::PackageUrl)
    } else {
        Err(BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))
    }
}

pub(in crate::builtin) struct CommandAdapter {
    sql_projection: backend_extension_turso::TursoProjection,
    registry: Option<RegistryGateway>,
    forge: ForgeGateway,
    discovery: Option<crate::discovery::DiscoveryGateway>,
    product_state: super::super::ProductState,
    compiler: LocalCompilerClient,
    search_snapshots: super::super::query::SearchSnapshotOwner,
    remote_semantic: super::super::query::RemoteSemantic,
    pending_semantic_search: Option<backend_library::SemanticSearchStatus>,
    published: Option<super::super::view_publish::PublishedRoots>,
    manifests: super::super::local_manifest::LocalManifestResidence,
    project_roots: super::super::project_root_residence::ProjectRootResidence,
    image_rows: super::super::view_build::ImageRowResidence,
    generations: super::super::generation_residence::SemanticGenerationResidence,
    semantic_authority: super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<Arc<super::super::cluster_dispatch::OwnerCompilerClusterRuntime>>,
    pending_stored_acks: Option<Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
    dependencies: Option<ResidentDependencies>,
    browse: super::super::browse::BrowseCache,
    /// The index job whose compile runs off the owner loop, if one does.
    indexing: Option<IndexJob>,
    /// Recently completed owner-issued index tickets, for late await/cancel requests.
    index_terminals: std::collections::VecDeque<backend_library::IndexJobTerminal>,
    /// Next owner-local index job identity.
    next_index_ticket: u64,
    /// Process epoch included in every ticket so stale client tickets cannot match after restart.
    index_owner_epoch: [u8; 16],
    /// Commands that change state, waiting for that job: one writer at a
    /// time, in arrival order. Reads never wait here.
    waiting: std::collections::VecDeque<(u64, Vec<u8>)>,
}

/// An `Add` of a local folder whose compile runs off the owner loop.
struct IndexJob {
    owner_ticket: backend_library::IndexJobTicket,
    /// Legacy Add request waiting for its committed Added reply.
    legacy_add: Option<(u64, u64)>,
    /// Owner-issued IndexAwait request listeners waiting for this terminal.
    awaiters: Vec<(u64, u64)>,
    /// Cancellation token registered to this exact compiler request only.
    cancelled: Arc<AtomicBool>,
    request_id: u64,
    requested_package: backend_engine::PackageKey,
    job: DeferredIndex,
    compiled: std::sync::mpsc::Receiver<
        Vec<
            Result<
                backend_engine::application::StagedSemanticPackage,
                backend_engine::application::PackageSemanticRuntimeError,
            >,
        >,
    >,
}

/// What the owner loop answers while an index job compiles: reads, from the
/// last publication. A command not named here changes state (or might) and
/// waits for the job.
fn answers_while_indexing(command: &Command) -> bool {
    use backend_library::SurfaceCommand as S;
    match command {
        Command::Packages
        | Command::PackagePage(_)
        | Command::Document(_)
        | Command::Source(_)
        | Command::Show { .. }
        | Command::Outline(_)
        | Command::OutlinePage { .. }
        | Command::Name(_)
        | Command::Resolve { .. }
        | Command::Search(_)
        | Command::Graph(_)
        | Command::Related(_)
        | Command::GraphPage { .. }
        | Command::GraphQuery(_)
        | Command::Health
        | Command::Revision => true,
        Command::Surface(surface) => matches!(
            surface,
            S::Read { .. }
                | S::References { .. }
                | S::Diff { .. }
                | S::Explore { .. }
                | S::Package { .. }
                | S::Dependents { .. }
                | S::Dependencies { .. }
                | S::PackageGraphPage { .. }
                | S::Owner { .. }
                | S::IndexSearch { .. }
                | S::PackageVersions { .. }
                | S::SemanticVersions { .. }
                | S::PackageProfile { .. }
                | S::Subscriptions
                | S::Releases { .. }
                | S::Projects
                | S::Tree
                | S::ProjectTree { .. }
                | S::IndexAwait { .. }
                | S::IndexCancel { .. }
        ),
        Command::Add { .. } | Command::Remove { .. } => false,
    }
}

/// The reply to an `Add` of `requested_package`, as the add path gives it.
fn added_reply(requested_package: backend_engine::PackageKey) -> AdmittedReply {
    let intent_id = backend_engine::intent_id("request_package", requested_package.as_bytes());
    let certificate = WireCertificate::new().with_claim(WireClaim::Intent {
        id: backend_engine::encode_id(intent_id.as_bytes()),
        token: "request_package".to_owned(),
        payload: requested_package.as_bytes().to_vec().into_boxed_slice(),
    });
    (CommandReply::Added(intent_id), Some(certificate))
}

/// What executing one command came to.
pub(in crate::builtin) enum Executed {
    /// The reply body.
    Reply(Vec<u8>),
    /// Answered later under its ticket ([`CommandAdapter::poll_deferred`]).
    Deferred,
}

struct ResidentDependencies {
    stamp: [u8; 32],
    local_witness: [u8; 32],
    catalog: Vec<backend_engine::RegistryPackageRecord>,
    selected_catalog_snapshot: [u8; 32],
    catalog_index: Arc<super::super::product_state::CatalogLookupIndex>,
    registry_facts: Vec<backend_engine::PackageDependencySourceFacts>,
    graph_facts: backend_library::CheckedPackageGraphFacts,
    index: backend_library::PackageGraphIndex,
    synced_root: Option<[u8; 32]>,
    synced_facts_witness: Option<[u8; 32]>,
}

impl CommandAdapter {
    pub(in crate::builtin) fn new(
        sql_projection: backend_extension_turso::TursoProjection,
        registry: Option<RegistryGateway>,
        forge: ForgeGateway,
        discovery: Option<crate::discovery::DiscoveryGateway>,
        product_state: super::super::ProductState,
        compiler: LocalCompilerClient,
        search_snapshots: super::super::query::SearchSnapshotOwner,
        remote_semantic: super::super::query::RemoteSemantic,
        published: Option<super::super::view_publish::PublishedRoots>,
        image_rows: super::super::view_build::ImageRowResidence,
        generations: super::super::generation_residence::SemanticGenerationResidence,
        semantic_authority: super::super::semantic_authority::SemanticAuthority,
        owner_cluster: Option<Arc<super::super::cluster_dispatch::OwnerCompilerClusterRuntime>>,
        pending_stored_acks: Option<
            Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
        >,
    ) -> Self {
        Self {
            sql_projection,
            registry,
            forge,
            discovery,
            product_state,
            compiler,
            search_snapshots,
            remote_semantic,
            pending_semantic_search: None,
            published,
            manifests: super::super::local_manifest::LocalManifestResidence::default(),
            project_roots: super::super::project_root_residence::ProjectRootResidence::default(),
            image_rows,
            generations,
            semantic_authority,
            owner_cluster,
            pending_stored_acks,
            dependencies: None,
            browse: super::super::browse::BrowseCache::default(),
            indexing: None,
            index_terminals: std::collections::VecDeque::new(),
            next_index_ticket: 1,
            index_owner_epoch: new_index_owner_epoch(),
            waiting: std::collections::VecDeque::new(),
        }
    }

    /// [`Self::execute`], except that indexing a local folder hands its
    /// compile off the owner loop and answers under `ticket` once it is
    /// published, and that a command that changes state waits while such a
    /// compile runs. Reads are answered at once, from the last publication.
    pub(in crate::builtin) fn execute_or_defer(
        &mut self,
        daemon: &mut ProductDaemon,
        body: &[u8],
        transport_ticket: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let owner = daemon.engine().daemon().library().cursor();
        let request = backend_engine::decode_command_dto_for_owner(body, owner)
            .map_err(|error| BuiltinModelError(format!("decode command DTO: {error}")))?;
        if let Command::Surface(backend_library::SurfaceCommand::IndexAwait { ticket }) =
            &request.command
        {
            return self.await_index_job(
                daemon,
                ticket.clone(),
                request.request_id,
                transport_ticket,
            );
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexCancel { ticket }) =
            &request.command
        {
            return self.cancel_index_job(daemon, ticket.clone(), request.request_id);
        }
        if self.indexing.is_some() && !answers_while_indexing(&request.command) {
            self.waiting.push_back((transport_ticket, body.to_vec()));
            return Ok(Executed::Deferred);
        }
        if let Command::Surface(backend_library::SurfaceCommand::IndexStart {
            package,
            execution_intent,
        }) = request.command
        {
            return self.start_owner_index_job(
                daemon,
                package,
                execution_intent,
                request.request_id,
            );
        }
        if let Command::Add {
            package,
            execution_intent,
        } = request.command
            && self.owner_cluster.is_none()
        {
            let certificate = request.certificate().cloned();
            let label = certified_package_label(certificate.as_ref(), package)?;
            let owner_ticket = self.issue_index_ticket(
                backend_library::PackageReference::parse(label)
                    .map_err(|error| BuiltinModelError(error.to_string()))?,
            )?;
            if let Some(started) = self.start_index_job(
                daemon,
                package,
                execution_intent,
                certificate.as_ref(),
                request.request_id,
                owner_ticket,
                Some(transport_ticket),
            )? {
                return Self::encode(daemon, request.request_id, started, None)
                    .map(Executed::Reply);
            }
            return Ok(Executed::Deferred);
        }
        self.execute(daemon, body).map(Executed::Reply)
    }

    fn start_owner_index_job(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_library::PackageReference,
        execution_intent: CompileExecutionIntent,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let owner_ticket = self.issue_index_ticket(package.clone())?;
        let package_key = backend_engine::package_key(package.as_str());
        let certificate = WireCertificate::new().with_claim(WireClaim::Key {
            schema: backend_engine::WireSchema::Package,
            id: backend_engine::encode_id(package_key.as_bytes()),
            value: package.as_str().to_owned(),
        });
        let result = if self.owner_cluster.is_some() {
            self.add(
                daemon,
                package_key,
                execution_intent,
                Some(&certificate),
                request_id,
            )
            .map(|_| Some(()))
        } else {
            self.start_index_job(
                daemon,
                package_key,
                execution_intent,
                Some(&certificate),
                request_id,
                owner_ticket.clone(),
                None,
            )
            .map(|reply| reply.map(|_| ()))
        };
        let result = match result {
            Ok(Some(())) => {
                let terminal = backend_library::IndexJobTerminal {
                    ticket: owner_ticket,
                    outcome: backend_library::IndexJobOutcome::Published,
                };
                self.retain_index_terminal(terminal.clone());
                backend_library::IndexStartResult::Terminal(terminal)
            }
            Ok(None) => {
                return Self::encode(
                    daemon,
                    request_id,
                    (
                        CommandReply::Surface(backend_library::SurfaceReply::IndexStarted(
                            backend_library::IndexStartResult::Started {
                                ticket: owner_ticket,
                                stage: backend_library::IndexJobStage::Compiling,
                            },
                        )),
                        None,
                    ),
                    None,
                )
                .map(Executed::Reply);
            }
            Err(error) => {
                let terminal = backend_library::IndexJobTerminal {
                    ticket: owner_ticket,
                    outcome: backend_library::IndexJobOutcome::Failed(bounded_index_detail(error)),
                };
                self.retain_index_terminal(terminal.clone());
                backend_library::IndexStartResult::Terminal(terminal)
            }
        };
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Surface(backend_library::SurfaceReply::IndexStarted(result)),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    fn issue_index_ticket(
        &mut self,
        package: backend_library::PackageReference,
    ) -> Result<backend_library::IndexJobTicket, BuiltinModelError> {
        let id = NonZeroU64::new(self.next_index_ticket).ok_or_else(|| {
            BuiltinModelError("owner index ticket sequence is exhausted".to_owned())
        })?;
        self.next_index_ticket = self.next_index_ticket.checked_add(1).unwrap_or(0);
        Ok(backend_library::IndexJobTicket::new(
            id,
            self.index_owner_epoch,
            package,
        ))
    }

    fn retain_index_terminal(&mut self, terminal: backend_library::IndexJobTerminal) {
        self.index_terminals.push_back(terminal);
        while self.index_terminals.len() > MAX_RETAINED_INDEX_TERMINALS {
            let _ = self.index_terminals.pop_front();
        }
    }

    fn await_index_job(
        &mut self,
        daemon: &ProductDaemon,
        ticket: backend_library::IndexJobTicket,
        request_id: u64,
        transport_ticket: u64,
    ) -> Result<Executed, BuiltinModelError> {
        if let Some(indexing) = &mut self.indexing
            && indexing.owner_ticket == ticket
        {
            indexing.awaiters.push((transport_ticket, request_id));
            return Ok(Executed::Deferred);
        }
        if let Some(terminal) = self
            .index_terminals
            .iter()
            .find(|terminal| terminal.ticket == ticket)
            .cloned()
        {
            return Self::encode(
                daemon,
                request_id,
                (
                    CommandReply::Surface(backend_library::SurfaceReply::IndexTerminal(terminal)),
                    None,
                ),
                None,
            )
            .map(Executed::Reply);
        }
        Err(BuiltinModelError(
            "index job ticket is unknown, expired, or belongs to another package".to_owned(),
        ))
    }

    fn cancel_index_job(
        &mut self,
        daemon: &ProductDaemon,
        ticket: backend_library::IndexJobTicket,
        request_id: u64,
    ) -> Result<Executed, BuiltinModelError> {
        let status = if let Some(indexing) = &self.indexing
            && indexing.owner_ticket == ticket
        {
            indexing.cancelled.store(true, Ordering::Release);
            backend_library::IndexCancelStatus::Requested
        } else if let Some(terminal) = self
            .index_terminals
            .iter()
            .find(|terminal| terminal.ticket == ticket)
            .cloned()
        {
            backend_library::IndexCancelStatus::Terminal(terminal)
        } else {
            backend_library::IndexCancelStatus::Unknown
        };
        Self::encode(
            daemon,
            request_id,
            (
                CommandReply::Surface(backend_library::SurfaceReply::IndexCancellation(status)),
                None,
            ),
            None,
        )
        .map(Executed::Reply)
    }

    /// Publishes the index job's compile once it is done, then runs the
    /// commands that waited for it, in order, until one defers again.
    /// Returns every reply that is ready, by ticket.
    pub(in crate::builtin) fn poll_deferred(
        &mut self,
        daemon: &mut ProductDaemon,
    ) -> Vec<(u64, Result<Vec<u8>, BuiltinModelError>)> {
        let mut ready = Vec::new();
        if let Some(indexing) = &self.indexing {
            let compiled = match indexing.compiled.try_recv() {
                Ok(compiled) => Some(compiled),
                Err(std::sync::mpsc::TryRecvError::Empty) => return ready,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            };
            let Some(IndexJob {
                owner_ticket,
                legacy_add,
                awaiters,
                cancelled,
                request_id,
                requested_package,
                job,
                ..
            }) = self.indexing.take()
            else {
                return ready;
            };
            let mut legacy_reply = None;
            let outcome = match compiled {
                Some(_) if cancelled.load(Ordering::Acquire) => {
                    backend_library::IndexJobOutcome::Cancelled
                }
                Some(compiled) if deferred_compile_was_cancelled(&compiled) => {
                    backend_library::IndexJobOutcome::Cancelled
                }
                Some(compiled) => {
                    match finish_deferred_index(daemon, &mut self.semantic_authority, job, compiled)
                    {
                        Ok(intent) => {
                            match self.finish_add(daemon, intent, request_id, requested_package) {
                                Ok(reply) => {
                                    legacy_reply =
                                        Some(Self::encode(daemon, request_id, reply, None));
                                    backend_library::IndexJobOutcome::Published
                                }
                                Err(error) => backend_library::IndexJobOutcome::Failed(
                                    bounded_index_detail(error),
                                ),
                            }
                        }
                        Err(refusal) => {
                            backend_library::IndexJobOutcome::Refused(bounded_index_detail(refusal))
                        }
                    }
                }
                None => backend_library::IndexJobOutcome::Failed(
                    backend_library::ProductText::from_static(
                        "compiler worker ended without a terminal receipt",
                    ),
                ),
            };
            let terminal = backend_library::IndexJobTerminal {
                ticket: owner_ticket,
                outcome,
            };
            self.retain_index_terminal(terminal.clone());
            if let Some((ticket, _)) = legacy_add {
                let reply = legacy_reply.unwrap_or_else(|| {
                    Err(BuiltinModelError(format!(
                        "index job did not publish: {:?}",
                        terminal.outcome
                    )))
                });
                ready.push((ticket, reply));
            }
            for (ticket, request_id) in awaiters {
                let reply = Self::encode(
                    daemon,
                    request_id,
                    (
                        CommandReply::Surface(backend_library::SurfaceReply::IndexTerminal(
                            terminal.clone(),
                        )),
                        None,
                    ),
                    None,
                );
                ready.push((ticket, reply));
            }
        }
        while self.indexing.is_none()
            && let Some((ticket, body)) = self.waiting.pop_front()
        {
            match self.execute_or_defer(daemon, &body, ticket) {
                Ok(Executed::Reply(reply)) => ready.push((ticket, Ok(reply))),
                Ok(Executed::Deferred) => {}
                Err(error) => ready.push((ticket, Err(error))),
            }
        }
        ready
    }

    /// Starts indexing a local folder: its scan and source frontier on the
    /// loop, its compile on a thread. `Some` when there was nothing to
    /// compile off the loop (the reply is ready), `None` when the job runs.
    fn start_index_job(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        execution_intent: CompileExecutionIntent,
        certificate: Option<&WireCertificate>,
        request_id: u64,
        owner_ticket: backend_library::IndexJobTicket,
        legacy_add: Option<u64>,
    ) -> Result<Option<AdmittedReply>, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        if !matches!(classify_add_target(&label)?, AddTarget::LocalDirectory) {
            return self
                .add(
                    daemon,
                    requested_package,
                    execution_intent,
                    certificate,
                    request_id,
                )
                .map(Some);
        }
        let prepared = match prepare_index_project(
            daemon,
            package,
            &label,
            request_id,
            execution_intent,
            &self.compiler,
            &mut self.semantic_authority,
        ) {
            Ok(prepared) => prepared,
            Err(refusal) => {
                return Err(refusal);
            }
        };
        match prepared {
            PreparedIndex::Ready(prepared) => self
                .finish_add(daemon, prepared, request_id, requested_package)
                .map(Some),
            PreparedIndex::Compile(mut job) => {
                let work = job.take_work();
                let compiler = self.compiler.clone();
                let cancelled = Arc::new(AtomicBool::new(false));
                let thread_cancelled = Arc::clone(&cancelled);
                let (sender, compiled) = std::sync::mpsc::sync_channel(1);
                std::thread::Builder::new()
                    .name("locald-index-compile".to_owned())
                    .spawn(move || {
                        let _ =
                            sender.send(run_deferred_compile(&compiler, work, thread_cancelled));
                    })
                    .map_err(|error| {
                        BuiltinModelError(format!("start the index compile: {error}"))
                    })?;
                self.indexing = Some(IndexJob {
                    owner_ticket,
                    legacy_add: legacy_add.map(|ticket| (ticket, request_id)),
                    awaiters: Vec::new(),
                    cancelled,
                    request_id,
                    requested_package,
                    job,
                    compiled,
                });
                Ok(None)
            }
        }
    }

    /// Commits an index job's semantic intent and publishes the view: the
    /// end of `add` for a local folder.
    fn finish_add(
        &mut self,
        daemon: &mut ProductDaemon,
        prepared: PreparedProductSelection,
        request_id: u64,
        requested_package: backend_engine::PackageKey,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let PreparedProductSelection { intent, selected } = prepared;
        let committed = self
            .semantic_authority
            .commit_product_selection_transaction(selected, || {
                intent
                    .map(|intent| {
                        commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                            BuiltinModelError(format!("commit product source intent: {error}"))
                        })?;
                        Ok(intent)
                    })
                    .transpose()
            })?;
        self.publish_view(daemon, committed.as_ref())?;
        Ok(added_reply(requested_package))
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
        let is_search = matches!(&request.command, Command::Search(_));
        self.pending_semantic_search = None;
        let admitted = self.dispatch(daemon, request.command, certificate, request_id)?;
        let status = if is_search {
            self.pending_semantic_search.take()
        } else {
            None
        };
        Self::encode(daemon, request_id, admitted, status)
    }

    /// One admitted reply as the wire's reply DTO.
    fn encode(
        daemon: &ProductDaemon,
        request_id: u64,
        (reply, certificate): AdmittedReply,
        search_status: Option<backend_library::SemanticSearchStatus>,
    ) -> Result<Vec<u8>, BuiltinModelError> {
        let mut reply = match reply {
            CommandReply::Health(root) => backend_engine::ReplyDto::health(
                request_id,
                root,
                daemon.engine().daemon().library().cursor(),
            ),
            reply => backend_engine::ReplyDto::new(request_id, reply),
        };
        if let Some(status) = search_status {
            reply = reply.with_semantic_search_status(status);
        }
        if let Some(certificate) = certificate {
            reply = reply.with_certificate(certificate);
        }
        backend_engine::encode_reply_dto(&reply).map_err(BuiltinModelError)
    }

    /// Decodes and serves one local semantic range against a fresh Turso head.
    /// The payload's selected stamp is only a claim; the resolver reopens the
    /// exact current metadata before and after the bounded CAS read.
    pub(in crate::builtin) fn serve_semantic_range(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        use backend_replication::{SemanticRangeChunk, SemanticRangeGet};

        let request = SemanticRangeGet::decode(&payload).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl("semantic range request is malformed")
        })?;
        if request.request_id != request_id {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range correlation mismatch",
            ));
        }
        let package = backend_engine::PackageReference::parse(request.target.package().to_owned())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range package target is invalid",
                )
            })?;
        let coordinate =
            PackageUrl::parse(request.target.coordinate().to_owned()).map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range compiler coordinate is invalid",
                )
            })?;
        if coordinate.as_str() != request.target.coordinate() {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range compiler coordinate is not canonical",
            ));
        }
        let key = ProductSemanticPublicationKey::new(
            package,
            coordinate.clone(),
            request.target.profile(),
        )
        .map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic range product target is inconsistent",
            )
        })?;
        let target = backend_engine::application::CompilerPackageTargetV2::for_package(coordinate);
        let identity = self
            .compiler
            .execution_identity(
                target.target(),
                request.target.profile(),
                backend_semantic::vocabulary::Stage::LowerIr,
            )
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic range target has no admitted compiler runtime",
            ))?;
        if identity.target() != target.target()
            || identity.profile() != request.target.profile()
            || identity.stage() != backend_semantic::vocabulary::Stage::LowerIr
        {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic range target differs from its compiler runtime",
            ));
        }

        let mut resolver = super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
            &self.semantic_authority,
            key,
        );
        let service = self.semantic_authority.versioned_plane_service();
        let bytes = service
            .serve_range_claim(
                &mut resolver,
                request.selected_stamp,
                request.image,
                request.range_request,
                request.byte_range,
                identity.environment_identity(),
                identity.target_platform_identity(),
            )
            .map_err(|error| {
                map_semantic_authority_error(error, "semantic range request is unavailable")
            })?;
        let chunk = SemanticRangeChunk::for_request(&request, bytes);
        chunk.validate_against(&request).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic range chunk failed identity admission",
            )
        })?;
        chunk
            .encode()
            .map(|bytes| bytes.into_boxed_slice())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic range chunk exceeds bounds",
                )
            })
    }

    /// Dispatches the canonical semantic paging DTOs to their typed
    /// authority handlers while preserving one bounded owner admission seam.
    pub(in crate::builtin) fn serve_semantic_control(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        match payload.get(5).copied() {
            Some(1) => self.serve_semantic_range(request_id, payload),
            Some(4 | 6 | 8) => self.serve_semantic_metadata(request_id, payload),
            _ => Err(crate::protocol::ProtocolError::InvalidControl(
                "unknown semantic paging operation",
            )),
        }
    }

    /// Serves one bounded semantic catalog, manifest, or full-image page
    /// against fresh Turso selection. The DTO's stamp and identity fields stay
    /// claims until the local authority reopens and rechecks the current head.
    pub(in crate::builtin) fn serve_semantic_metadata(
        &mut self,
        request_id: u64,
        payload: Box<[u8]>,
    ) -> Result<Box<[u8]>, crate::protocol::ProtocolError> {
        use backend_replication::{
            SelectedSemanticImageGet, SemanticCatalogGet, SemanticManifestGet,
        };

        let tag = payload
            .get(5)
            .copied()
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata request is malformed",
            ))?;
        match tag {
            4 => {
                let get = SemanticCatalogGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog request is malformed",
                    )
                })?;
                let correlated_id = get.request_id;
                let target = get.target.clone();
                let (key, identity) = self.semantic_metadata_context(&target)?;
                if correlated_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog correlation mismatch",
                    ));
                }
                let mut resolver =
                    super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
                        &self.semantic_authority,
                        key,
                    );
                let service = self.semantic_authority.versioned_plane_service();
                let chunk = service
                    .serve_catalog_get(
                        &mut resolver,
                        &get,
                        identity.environment_identity(),
                        identity.target_platform_identity(),
                    )
                    .map_err(|error| {
                        map_semantic_authority_error(
                            error,
                            "semantic catalog request is unavailable",
                        )
                    })?;
                let encoded = chunk.encode().map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic catalog page exceeds bounds",
                    )
                })?;
                return Ok(encoded.into_boxed_slice());
            }
            6 => {
                let get = SemanticManifestGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest request is malformed",
                    )
                })?;
                let correlated_id = get.request_id;
                let target = get.target.clone();
                let (key, identity) = self.semantic_metadata_context(&target)?;
                if correlated_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest correlation mismatch",
                    ));
                }
                let mut resolver =
                    super::super::versioned_planes::SemanticAuthoritySelectionSource::new(
                        &self.semantic_authority,
                        key,
                    );
                let service = self.semantic_authority.versioned_plane_service();
                let chunk = service
                    .serve_manifest_get(
                        &mut resolver,
                        &get,
                        identity.environment_identity(),
                        identity.target_platform_identity(),
                    )
                    .map_err(|error| {
                        map_semantic_authority_error(
                            error,
                            "semantic manifest request is unavailable",
                        )
                    })?;
                let encoded = chunk.encode().map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "semantic manifest page exceeds bounds",
                    )
                })?;
                return Ok(encoded.into_boxed_slice());
            }
            8 => {
                let get = SelectedSemanticImageGet::decode(&payload).map_err(|_| {
                    crate::protocol::ProtocolError::InvalidControl(
                        "selected semantic image request is malformed",
                    )
                })?;
                if get.request_id != request_id {
                    return Err(crate::protocol::ProtocolError::InvalidControl(
                        "selected semantic image correlation mismatch",
                    ));
                }
                let (key, identity) = self.semantic_metadata_context(&get.target)?;
                let chunk = super::super::selected_full_image::serve_selected_image_get(
                    &mut self.semantic_authority,
                    &key,
                    &get,
                    identity.environment_identity(),
                    identity.target_platform_identity(),
                )
                .map_err(|error| {
                    let message = match error {
                        super::super::selected_full_image::SelectedFullImageError::StaleSelection => {
                            "selected semantic image is stale"
                        }
                        super::super::selected_full_image::SelectedFullImageError::PlatformOrEnvironmentMismatch => {
                            "selected semantic image targets another environment or platform"
                        }
                        _ => "selected semantic image request is unavailable",
                    };
                    crate::protocol::ProtocolError::InvalidControl(message)
                })?;
                return chunk
                    .encode()
                    .map(|bytes| bytes.into_boxed_slice())
                    .map_err(|_| {
                        crate::protocol::ProtocolError::InvalidControl(
                            "selected semantic image page exceeds bounds",
                        )
                    });
            }
            _ => {
                return Err(crate::protocol::ProtocolError::InvalidControl(
                    "unknown semantic metadata operation",
                ));
            }
        }
    }

    fn semantic_metadata_context(
        &self,
        target: &backend_replication::SemanticTargetKey,
    ) -> Result<
        (
            ProductSemanticPublicationKey,
            backend_engine::application::LocalCompilerExecutionIdentity,
        ),
        crate::protocol::ProtocolError,
    > {
        let package = backend_engine::PackageReference::parse(target.package().to_owned())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic metadata package target is invalid",
                )
            })?;
        let coordinate = PackageUrl::parse(target.coordinate().to_owned()).map_err(|_| {
            crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata compiler coordinate is invalid",
            )
        })?;
        if coordinate.as_str() != target.coordinate() {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata compiler coordinate is not canonical",
            ));
        }
        let key = ProductSemanticPublicationKey::new(package, coordinate.clone(), target.profile())
            .map_err(|_| {
                crate::protocol::ProtocolError::InvalidControl(
                    "semantic metadata product target is inconsistent",
                )
            })?;
        let compiler_target =
            backend_engine::application::CompilerPackageTargetV2::for_package(coordinate);
        let identity = self
            .compiler
            .execution_identity(
                compiler_target.target(),
                target.profile(),
                backend_semantic::vocabulary::Stage::LowerIr,
            )
            .ok_or(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata target has no admitted compiler runtime",
            ))?;
        if identity.target() != compiler_target.target()
            || identity.profile() != target.profile()
            || identity.stage() != backend_semantic::vocabulary::Stage::LowerIr
        {
            return Err(crate::protocol::ProtocolError::InvalidControl(
                "semantic metadata target differs from its compiler runtime",
            ));
        }
        Ok((key, identity))
    }

    fn dispatch(
        &mut self,
        daemon: &mut ProductDaemon,
        command: Command,
        certificate: Option<WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        match command {
            Command::Add {
                package,
                execution_intent,
            } => self.add(
                daemon,
                package,
                execution_intent,
                certificate.as_ref(),
                request_id,
            ),
            Command::Remove { package } => {
                self.remove(daemon, package, certificate.as_ref(), request_id)
            }
            Command::Search(query) => self.search(daemon, &query, certificate),
            Command::Graph(query) => self.graph(daemon, query, certificate, false),
            Command::Related(query) => self.graph(daemon, query, certificate, true),
            Command::GraphQuery(request) => execute_certified_graph_query(
                daemon,
                &self.compiler,
                &mut self.search_snapshots,
                &mut self.generations,
                &mut self.image_rows,
                &request,
                certificate,
            ),
            Command::Surface(surface) => self.surface(daemon, surface, request_id),
            command => self.standard(daemon, &command, certificate),
        }
    }

    fn add(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        execution_intent: CompileExecutionIntent,
        certificate: Option<&WireCertificate>,
        request_id: u64,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let label = certified_package_label(certificate, package)?;
        let requested_package = package;
        let (package, label) = canonical_local_package(package, label)?;
        let prepared = match classify_add_target(&label)? {
            AddTarget::LocalDirectory => match index_project_intent_with_cluster_and_intent(
                daemon,
                package,
                &label,
                request_id,
                execution_intent,
                &self.compiler,
                &mut self.semantic_authority,
                self.owner_cluster.as_deref(),
                self.pending_stored_acks.as_ref(),
            ) {
                Ok(prepared) => prepared.unwrap_or(PreparedProductSelection {
                    intent: None,
                    selected: Vec::new(),
                }),
                Err(refusal) => return Err(refusal),
            },
            AddTarget::PackageUrl => self
                .registry_intent(daemon, package, &label, request_id)?
                .unwrap_or(PreparedProductSelection {
                    intent: None,
                    selected: Vec::new(),
                }),
        };
        let PreparedProductSelection { intent, selected } = prepared;
        let committed = self
            .semantic_authority
            .commit_product_selection_transaction(selected, || {
                intent
                    .map(|intent| {
                        commit_builtin_intent(daemon, request_id, &intent).map_err(|error| {
                            BuiltinModelError(format!("commit product source intent: {error}"))
                        })?;
                        Ok(intent)
                    })
                    .transpose()
            })?;
        self.publish_view(daemon, committed.as_ref())?;
        Ok(added_reply(requested_package))
    }

    fn registry_intent(
        &mut self,
        daemon: &mut ProductDaemon,
        package: backend_engine::PackageKey,
        label: &str,
        request_id: u64,
    ) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
        let coordinate = backend_engine::registry::PackageCoordinate::parse(label)
            .map_err(|_| BuiltinModelError(ADD_TARGET_REQUIRED.to_owned()))?;
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
            return BuiltinIntent::add(package, label.to_owned()).map(|intent| {
                Some(PreparedProductSelection {
                    intent: Some(intent),
                    selected: Vec::new(),
                })
            });
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
            &mut self.semantic_authority,
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
        &self,
        daemon: &ProductDaemon,
        query: &backend_library::DocumentQuery,
        certificate: Option<&WireCertificate>,
        recover_excerpt: bool,
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
        let row = symbol_row_by_label(library.view(), label)?;
        let backend_engine::RowId::Symbol(_) = row.id else {
            return None;
        };
        let symbol = backend_library::symbol_key(label);
        let source_basis = backend_library::Basis {
            root,
            ..library.view().basis()
        };
        let excerpt =
            if recover_excerpt && row.excerpt == backend_library::SourceExcerpt::NotHydrated {
                self.recover_indexed_excerpt(daemon, row, label)
                    .unwrap_or_else(|| row.excerpt.clone())
            } else {
                row.excerpt.clone()
            };
        let mut document = backend_library::Document::new(symbol, root, row.document.clone())
            .with_source_basis(source_basis)
            .with_location(row.source.clone())
            .with_excerpt(excerpt)
            .with_facts(row.facts.clone());
        document.signature.clone_from(&row.signature);
        Some(document)
    }

    /// Rehydrates a shed declaration excerpt from the exact package source
    /// file admitted by the current source relation. The indexed source
    /// identity must still match before the baseline parser can produce text.
    fn recover_indexed_excerpt(
        &self,
        daemon: &ProductDaemon,
        row: &backend_library::Row,
        label: &str,
    ) -> Option<backend_compile::SourceExcerpt> {
        let location = row.source.captured()?;
        let package = row.package?;
        let workspace = self.product_state.workspace_path().ok()?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let sources = super::super::read_package_sources(&snapshot, package).ok()?;
        let project = sources.projects.get(&package.to_bytes())?;
        let (claimed_project, _) = label.split_once("::")?;
        if project.package != package
            || claimed_project != project.label
            || backend_engine::PackageKey::from_value(&project.label) != package
        {
            return None;
        }
        let source_root = admitted_project_source_root(package, &project.label, workspace)?;
        let fields = sources.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.project == package.to_bytes() && fields.path == location.path())
                .then_some(fields)
        })?;
        let source_identity = fields.source_identity?;
        super::super::ingest::recover_indexed_excerpt(
            &source_root,
            fields.path,
            source_identity,
            fields.language,
            label,
            location.start_line(),
            row.kind,
        )
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
        let committed = if let Some(intent) =
            remove_project_intent(daemon, package, &label, &mut self.semantic_authority)?
        {
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
            &mut self.image_rows,
            &mut self.generations,
        )
        .map_err(|error| BuiltinModelError(format!("publish product source view: {error}")))?;
        self.published = Some(outcome.roots);
        project_view_deltas(&mut self.sql_projection, daemon, &outcome.deltas)?;
        self.semantic_authority.mark_projections_current()
    }

    fn search(
        &mut self,
        daemon: &ProductDaemon,
        query: &backend_engine::Query,
        certificate: Option<WireCertificate>,
    ) -> Result<AdmittedReply, BuiltinModelError> {
        let command = Command::Search(query.clone());
        let (reply, status) = execute_search(
            daemon,
            &self.compiler,
            &mut self.search_snapshots,
            &mut self.remote_semantic,
            &mut self.generations,
            &mut self.image_rows,
            query,
        )
        .map_or_else(
            |error| (CommandReply::Error(error.to_string()), None),
            |(reply, status)| (reply, Some(status)),
        );
        self.pending_semantic_search = status;
        Self::certify(daemon, &command, reply, certificate)
    }

    fn graph(
        &mut self,
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
        let reply = match execute_semantic_graph(
            daemon,
            &self.compiler,
            &mut self.generations,
            &mut self.image_rows,
            query,
            include_incoming,
        )? {
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
        symbol_row_by_label(view, label)
            .and_then(|row| match row.id {
                backend_engine::RowId::Symbol(symbol) => Some(symbol),
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
            backend_engine::SurfaceCommand::ForgeAdd { coordinate } => {
                self.forge.acquire(coordinate.as_str()).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(backend_engine::SurfaceReply::ForgePackageAdded(
                            record,
                        ))
                    },
                )
            }
            backend_engine::SurfaceCommand::ForgeReference { coordinate } => {
                self.forge.reference(coordinate.as_str()).map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                            error.to_string(),
                        ))
                    },
                    |record| {
                        CommandReply::Surface(backend_engine::SurfaceReply::ForgePackageReferenced(
                            record,
                        ))
                    },
                )
            }
            backend_engine::SurfaceCommand::ProjectTree { root } => {
                let authority = self.registry.as_ref().map(RegistryGateway::advisory);
                self.browse
                    .project_tree(Path::new(root.as_str()), authority)
                    .map_or_else(
                        |error| {
                            CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                                error,
                            ))
                        },
                        |tree| {
                            CommandReply::Surface(backend_engine::SurfaceReply::ProjectTree(
                                Box::new(tree),
                            ))
                        },
                    )
            }
            backend_engine::SurfaceCommand::AdvisoryRefresh => match self.registry.as_mut() {
                Some(gateway) => gateway.refresh_advisories().map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(error))
                    },
                    |states| {
                        CommandReply::Surface(backend_engine::SurfaceReply::AdvisoryRefreshed(
                            states.into_boxed_slice(),
                        ))
                    },
                ),
                None => CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                    "no advisory authority is open in this owner".to_owned(),
                )),
            },
            backend_engine::SurfaceCommand::References { target } => execute_references(
                daemon,
                &self.compiler,
                &mut self.generations,
                &mut self.image_rows,
                &target,
            )
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                CommandReply::Surface,
            ),
            backend_engine::SurfaceCommand::Diff { from, to } => execute_semantic_diff(
                daemon,
                &self.compiler,
                &mut self.generations,
                &mut self.image_rows,
                &from,
                &to,
            )
            .map_or_else(
                |error| {
                    CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(
                        error.to_string(),
                    ))
                },
                |rows| CommandReply::Surface(backend_engine::SurfaceReply::Diff(rows)),
            ),
            backend_engine::SurfaceCommand::SemanticVersions { package } => {
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root());
                semantic_versions(daemon, &package, workspace, &self.semantic_authority)
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
                if let Some(discovery) = self.discovery.as_mut() {
                    discovery.apply_pending();
                }
                let roots = self
                    .project_roots
                    .roots(&daemon.engine().daemon().owner().snapshot())?;
                self.manifests
                    .refresh(roots.iter().map(std::path::PathBuf::as_path))
                    .map_err(BuiltinModelError)?;
                let local_witness = self.manifests.witness();
                let stamp = match self.registry.as_mut() {
                    Some(registry) => registry.publication_stamp().map_err(BuiltinModelError)?,
                    None => [0; 32],
                };
                let root = *daemon.engine().daemon().library().view().root().as_bytes();
                let stamp_changed = self
                    .dependencies
                    .as_ref()
                    .is_none_or(|cached| cached.stamp != stamp);
                let local_changed = self
                    .dependencies
                    .as_ref()
                    .is_none_or(|cached| cached.local_witness != local_witness);
                if stamp_changed || local_changed {
                    let (
                        catalog,
                        catalog_index,
                        selected_catalog_snapshot,
                        registry_facts,
                        synced_root,
                        synced_facts_witness,
                    ) = if stamp_changed {
                        let (catalog, catalog_index, selected_catalog_snapshot) =
                            if let Some(gateway) = self.registry.as_mut() {
                                let projection =
                                    gateway.catalog_projection().map_err(BuiltinModelError)?;
                                (
                                    projection.records.clone(),
                                    Arc::clone(&projection.index),
                                    projection.selected_rows_snapshot,
                                )
                            } else {
                                let catalog = Vec::new();
                                let index = Arc::new(
                                    super::super::product_state::CatalogLookupIndex::from_catalog(
                                        &catalog,
                                    ),
                                );
                                let selected_catalog_snapshot =
                                    CatalogLookupIndex::snapshot_for_catalog(&catalog);
                                (catalog, index, selected_catalog_snapshot)
                            };
                        let registry_facts = self
                            .registry
                            .as_mut()
                            .map_or_else(Vec::new, RegistryGateway::dependency_facts);
                        let synced_root = self
                            .dependencies
                            .as_ref()
                            .and_then(|cached| cached.synced_root);
                        let synced_facts_witness = self
                            .dependencies
                            .as_ref()
                            .and_then(|cached| cached.synced_facts_witness);
                        (
                            catalog,
                            catalog_index,
                            selected_catalog_snapshot,
                            registry_facts,
                            synced_root,
                            synced_facts_witness,
                        )
                    } else {
                        let cached = self.dependencies.take().ok_or_else(|| {
                            BuiltinModelError(
                                "dependency index disappeared during a local refresh".to_owned(),
                            )
                        })?;
                        (
                            cached.catalog,
                            cached.catalog_index,
                            cached.selected_catalog_snapshot,
                            cached.registry_facts,
                            cached.synced_root,
                            cached.synced_facts_witness,
                        )
                    };
                    let mut facts = registry_facts.clone();
                    facts.extend(self.manifests.facts().cloned());
                    let graph_facts = backend_library::CheckedPackageGraphFacts::new(facts)
                        .map_err(|error| {
                            BuiltinModelError(format!("check package dependency facts: {error}"))
                        })?;
                    let index =
                        backend_library::PackageGraphIndex::from_checked_facts(&graph_facts);
                    self.dependencies = Some(ResidentDependencies {
                        stamp,
                        local_witness,
                        catalog,
                        selected_catalog_snapshot,
                        catalog_index,
                        registry_facts,
                        graph_facts,
                        index,
                        synced_root,
                        synced_facts_witness,
                    });
                }
                let workspace = self
                    .registry
                    .as_ref()
                    .map(|gateway| gateway.workspace_root().to_path_buf());
                let mut cached = self.dependencies.take().ok_or_else(|| {
                    BuiltinModelError("dependency index disappeared after refresh".to_owned())
                })?;
                if cached.synced_root != Some(root)
                    || cached.synced_facts_witness != Some(cached.graph_facts.witness())
                {
                    futures_executor::block_on(
                        self.sql_projection.synchronize_checked_package_graph(
                            daemon.engine().daemon().library().view().root(),
                            &cached.graph_facts,
                        ),
                    )
                    .map_err(|error| {
                        BuiltinModelError(format!("align package graph projection: {error}"))
                    })?;
                    cached.synced_root = Some(root);
                    cached.synced_facts_witness = Some(cached.graph_facts.witness());
                }
                let discovery_store = self.discovery.as_ref().map(|gateway| gateway.store());
                let forge_records = if matches!(
                    &surface,
                    backend_engine::SurfaceCommand::IndexSearch { .. }
                        | backend_engine::SurfaceCommand::Package { .. }
                ) {
                    self.forge.search_records().map_err(BuiltinModelError)?
                } else {
                    Vec::new()
                };
                let reply = match &surface {
                    backend_engine::SurfaceCommand::PackageGraphPage { request } => request
                        .clone()
                        .bind_catalog_snapshot(cached.selected_catalog_snapshot)
                        .map_err(|error| error.to_string())
                        .and_then(|request| {
                            let page = futures_executor::block_on(
                                self.sql_projection.read_package_graph_page(&request),
                            )
                            .map_err(|error| error.to_string())?;
                            page.admit_against_checked_facts(
                                &request,
                                root,
                                &cached.graph_facts,
                                &cached.index,
                            )
                            .map_err(|error| error.to_string())?;
                            Ok(backend_engine::SurfaceReply::PackageGraphPage(page))
                        }),
                    _ => self
                        .product_state
                        .execute_with_discovery_and_forge_snapshot(
                            surface.clone(),
                            daemon.engine().daemon().library().view(),
                            &cached.catalog,
                            &cached.catalog_index,
                            cached.graph_facts.facts(),
                            &cached.index,
                            workspace.as_deref(),
                            discovery_store,
                            cached.selected_catalog_snapshot,
                            &forge_records,
                        ),
                };
                self.dependencies = Some(cached);
                reply.map_or_else(
                    |error| {
                        CommandReply::Failed(backend_engine::CommandFailure::InvalidQuery(error))
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
                    self.semantic_authority
                        .select_existing(&selected_key, claim)?;
                    let intent = BuiltinIntent::index_with_semantics(
                        package_key,
                        package.as_str(),
                        Vec::new(),
                        vec![BuiltinSemanticChange {
                            key: selected_key.clone(),
                            after: Some(record.clone()),
                        }],
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
                    self.semantic_authority.freshness(&selected_key, claim),
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
            Command::Document(query) | Command::Source(query) => self
                .canonical_claim_document(
                    daemon,
                    query,
                    certificate.as_ref(),
                    matches!(command, Command::Source(_)),
                )
                .map_or_else(
                    || daemon.engine().daemon().library().execute(command.clone()),
                    |document| Ok(CommandReply::Document(document)),
                )
                .unwrap_or_else(|error| CommandReply::Error(error.to_string())),
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

pub(in crate::builtin) fn commit_builtin_intent(
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

fn symbol_row_by_label<'a>(
    view: &'a backend_engine::ViewRoot,
    label: &str,
) -> Option<&'a backend_engine::Row> {
    match view.row_by_label(label) {
        Some(row) if matches!(row.id, backend_engine::RowId::Symbol(_)) => Some(row),
        Some(_) => view
            .row_refs()
            .find(|row| row.label == label && matches!(row.id, backend_engine::RowId::Symbol(_))),
        None => None,
    }
}

/// Resolves the source root named by the package's admitted project record.
/// Local projects are stored under their canonical directory label; staged
/// packages retain their canonical pinned package URL.
fn admitted_project_source_root(
    package: backend_engine::PackageKey,
    project_label: &str,
    workspace: &Path,
) -> Option<std::path::PathBuf> {
    if backend_engine::PackageKey::from_value(project_label) != package {
        return None;
    }
    let root =
        super::super::local_manifest::indexed_package_source_root(project_label, workspace).ok()?;
    let canonical = root.canonicalize().ok()?;
    if !project_label.starts_with("pkg:") && Path::new(project_label) != canonical {
        return None;
    }
    Some(canonical)
}

fn map_semantic_authority_error(
    error: super::super::versioned_planes::VersionedPlaneServiceError,
    unavailable: &'static str,
) -> crate::protocol::ProtocolError {
    match error {
        super::super::versioned_planes::VersionedPlaneServiceError::StaleSelection => {
            crate::protocol::ProtocolError::SemanticStaleSelection
        }
        _ => crate::protocol::ProtocolError::InvalidControl(unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ADD_TARGET_REQUIRED, AddTarget, admitted_project_source_root, classify_add_target,
    };
    use crate::builtin::BuiltinIntent;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nudox-add-guard-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp tree");
            Self(path)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn label(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_directory_is_indexed_in_place() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        fs::create_dir(&project).expect("project dir");
        assert!(matches!(
            classify_add_target(&label(&project)),
            Ok(AddTarget::LocalDirectory)
        ));
    }

    #[test]
    fn a_symlink_to_a_directory_is_indexed_in_place() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        let link = tree.0.join("link");
        fs::create_dir(&project).expect("project dir");
        std::os::unix::fs::symlink(&project, &link).expect("directory symlink");
        assert!(matches!(
            classify_add_target(&label(&link)),
            Ok(AddTarget::LocalDirectory)
        ));
    }

    #[test]
    fn a_file_add_is_refused_instead_of_an_empty_project() {
        let tree = TempTree::new();
        let file = tree.0.join("lib.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        let name = label(&file);
        let package = backend_engine::package_key(&name);
        assert!(
            BuiltinIntent::add(package, name.clone()).is_ok(),
            "the empty-project constructor still accepts a file label"
        );
        let error = classify_add_target(&name).expect_err("file add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_symlink_to_a_file_is_refused() {
        let tree = TempTree::new();
        let file = tree.0.join("lib.rs");
        let link = tree.0.join("link.rs");
        fs::write(&file, "fn main() {}\n").expect("file");
        std::os::unix::fs::symlink(&file, &link).expect("file symlink");
        let error = classify_add_target(&label(&link)).expect_err("symlink add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_missing_path_is_refused() {
        let tree = TempTree::new();
        let missing = tree.0.join("missing");
        let error = classify_add_target(&label(&missing)).expect_err("missing add");
        assert_eq!(error.0, ADD_TARGET_REQUIRED);
    }

    #[test]
    fn a_version_pinned_package_url_stays_a_registry_add() {
        assert!(matches!(
            classify_add_target("pkg:cargo/serde@1.0.0"),
            Ok(AddTarget::PackageUrl)
        ));
        assert!(matches!(
            classify_add_target("PKG:cargo/serde@1.0.0"),
            Ok(AddTarget::PackageUrl)
        ));
    }

    #[test]
    fn admitted_local_project_label_resolves_its_canonical_source_root() {
        let tree = TempTree::new();
        let project = tree.0.join("project");
        fs::create_dir(&project).expect("project dir");
        let label = label(&project.canonicalize().expect("canonical project"));
        let package = backend_engine::PackageKey::from_value(&label);

        assert_eq!(
            admitted_project_source_root(package, &label, &tree.0),
            Some(project.canonicalize().expect("canonical project"))
        );
        assert!(
            admitted_project_source_root(
                backend_engine::PackageKey::from_value("/outside/project"),
                &label,
                &tree.0,
            )
            .is_none()
        );
    }
}
