//! Completed scans and compiler routing prepared from an immutable admitted
//! snapshot. Every source read, relation walk and candidate SQL runs on the
//! existing owned publication worker, never the listener's control turn.
use super::*;

pub(in crate::builtin::commands) enum PreparedSourceScan {
    Ready(PreparedProductSelection),
    Capture {
        selection: PreparedProductSelection,
        continuation: SourceCompilePlan,
    },
}

pub(in crate::builtin::commands) struct SourceRoute {
    pub(in crate::builtin::commands) owner:
        Arc<crate::builtin::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pub(in crate::builtin::commands) pending:
        Option<Arc<Mutex<crate::builtin::pending_stored::PendingStoredAckJournal>>>,
}

/// Owned continuation of exactly the structural capture candidate. Its inputs
/// were materialized and hash-checked before asking the owner for a grant.
pub(in crate::builtin::commands) struct SourceCompilePlan {
    package: backend_engine::PackageKey,
    label: String,
    project_key: [u8; 32],
    coordinate: Option<PackageUrl>,
    request_id: u64,
    source_root: PathBuf,
    revision_fence: ingest::CompilerRevisionFence,
    sources: Vec<ingest::CompilerSource>,
    observations: BTreeMap<LanguageProfile, SourceObservationReceipt>,
    pub(in crate::builtin::commands) captures:
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    /// Profiles whose exact selected generation was proven current and will
    /// be carried into this operation's receipt without recompiling it.
    reused_profiles: BTreeSet<LanguageProfile>,
    operation_key: Option<backend_library::IndexOperationKey>,
    reuse_only: bool,
    python_environment: Option<PythonEnvironmentInput>,
    cancellation: Arc<AtomicBool>,
    workspace_snapshot: Option<ingest::CompilerWorkspaceSnapshot>,
    route: Option<SourceRoute>,
    execution_intent: CompileExecutionIntent,
}

/// Shared immutable-base source and profile admission. This plan cannot
/// commit a workspace or publish a view; callers choose their capture timing.
pub(super) struct PreparedScanSource {
    pub(super) package: backend_engine::PackageKey,
    pub(super) label: String,
    pub(super) requested_root: PathBuf,
    pub(super) coordinate: Option<PackageUrl>,
    pub(super) request_id: u64,
    pub(super) execution_intent: CompileExecutionIntent,
    pub(super) project_key: [u8; 32],
    pub(super) before: Option<ProductSourceRecord>,
    pub(super) project: ProductSourceRecord,
    pub(super) file_keys: Vec<[u8; 32]>,
    pub(super) changes: Vec<BuiltinSourceChange>,
    pub(super) source_facts_changes: Vec<BuiltinSourceFactsChange>,
    pub(super) revision_fence: ingest::CompilerRevisionFence,
    pub(super) python_environment: Option<PythonEnvironmentInput>,
    pub(super) cancellation: Arc<AtomicBool>,
    pub(super) workspace_snapshot: Option<ingest::CompilerWorkspaceSnapshot>,
    pub(super) profiles: PreparedScanProfiles,
}

pub(super) enum PreparedScanProfiles {
    Reused {
        captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
        reused_profiles: BTreeSet<LanguageProfile>,
        pending_changes: Vec<BuiltinCaptureChange>,
    },
    Compile(Box<PreparedScanCompilation>),
}

/// The exact dirty-profile input handles and private Pending after-images,
/// admitted together against PreparedScanSource's immutable base.
pub(super) struct PreparedScanCompilation {
    pub(super) fresh: Vec<ingest::CompilerSourceHandle>,
    pub(super) reused: Vec<ingest::ReusedCompilerFile>,
    pub(super) source_admission_policy: crate::builtin::source_budget::SourceAdmissionPolicy,
    pub(super) observations: BTreeMap<LanguageProfile, SourceObservationReceipt>,
    pub(super) captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    /// Exact selected generations omitted from this compile because their
    /// current input witness proves reuse.
    pub(super) reused_profiles: BTreeSet<LanguageProfile>,
    pub(super) pending_changes: Vec<BuiltinCaptureChange>,
}

pub(super) fn prepare_scan_source(
    base_snapshot: &WorkspaceSnapshot,
    result: IndexScanResult,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut crate::builtin::semantic_authority::SemanticAuthority,
    operation_key: Option<backend_library::IndexOperationKey>,
) -> Result<PreparedScanSource, BuiltinModelError> {
    let current_root = base_snapshot.root();
    if current_root != result.work.workspace_root {
        return Err(BuiltinModelError(
            "workspace selection changed while the project scan was running; retry indexing"
                .to_owned(),
        ));
    }
    let IndexScanResult {
        work,
        scan,
        workspace_snapshot,
    } = result;
    let mut final_revision_fence = scan.revision_fence.clone();
    let cancellation = work.cancellation.clone();
    let relation = base_snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let facts_relation = product_source_file_facts_relation(&base_snapshot)
        .map_err(|error| BuiltinModelError(format!("open product source facts: {error}")))?;
    let IndexScanWork {
        package,
        label,
        source_root: source_root_owned,
        coordinate: coordinate_owned,
        request_id,
        execution_intent,
        project_key,
        before,
        old_files,
        reusable,
        ..
    } = work;

    let coordinate = coordinate_owned.as_ref();
    let revision_fence = &scan.revision_fence;
    if cancellation.load(Ordering::Acquire) {
        return Err(BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()));
    }
    if !ingest::compiler_revision_is_current_with_cancellation(
        revision_fence,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?
    {
        return Err(BuiltinModelError(
            "project root or files changed after source admission; retry indexing".to_owned(),
        ));
    }
    // Use the root pinned by the scan for configuration, compiler-input, and
    // profile reads. Keep the original spelling only for alias-retarget checks
    // at later admission fences.
    let source_root = revision_fence.canonical_root();
    let inputs = compiler_input_admissions(source_root, &scan, compiler);
    let semantic_context = SemanticCompilationContext::admit(
        package,
        &label,
        source_root,
        coordinate,
        request_id,
        compiler,
        scan.python_environment.as_ref(),
        cancellation.as_ref(),
    )?;
    let source_counts = ingest::compiler_profile_source_counts(
        source_root,
        &scan.compiler_sources,
        &scan.reused_compiler_files,
    );
    let live = source_counts.keys().copied().collect::<BTreeSet<_>>();
    let present =
        ingest::present_compiler_paths(&scan.compiler_sources, &scan.reused_compiler_files);
    let lost = ingest::lost_compiler_profiles(source_root, &reusable, &present)
        .map_err(BuiltinModelError)?;
    // Each profile needs its own selected native producer fence. A matching
    // scan digest only identifies the candidate; replay proves its full read
    // closure, including dependencies and negative filesystem observations.
    let mut typescript_reused_profiles = BTreeSet::new();
    for &profile in &live {
        if !matches!(profile, LanguageProfile::TypeScript(_)) || lost.contains(&profile) {
            continue;
        }
        let binding = selected_semantic_binding_for_snapshot(
            base_snapshot,
            package,
            &label,
            coordinate,
            profile,
        )?;
        let proof = binding.and_then(|binding| {
            compiler.typescript_snapshot_reuse(
                source_root,
                profile,
                semantic_input_digest(&scan, profile),
                binding,
                cancellation.as_ref(),
            )
        });
        if let Some(proof) = proof {
            typescript_reused_profiles.insert(profile);
            final_revision_fence.typescript_reuse.push(proof);
        }
    }
    // Persist candidate observations first. They fence compilation but remain
    // private to freshness and query paths until the combined product intent
    // commits.
    let mut observed_profiles = live.clone();
    observed_profiles.extend(lost.iter().copied());
    let mut observations = BTreeMap::new();
    for profile in observed_profiles {
        let coordinate =
            crate::builtin::compiler_scope::semantic_coordinate(package, profile, coordinate)?;
        let key = ProductSemanticPublicationKey::new(
            semantic_context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let input_digest = semantic_input_digest(&scan, profile);
        let count = source_counts.get(&profile).copied().unwrap_or(0);
        let count = u64::try_from(count)
            .map_err(|_| BuiltinModelError("semantic source count exceeds u64".to_owned()))?;
        observations.insert(
            profile,
            semantic_authority.observe(&key, input_digest, count)?,
        );
    }
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project_update = ProductSourceRecord::project_with_membership_pages_and_source_selection(
        &label,
        scan.source_version,
        file_keys.clone(),
        None,
        Some(scan.source_selection.clone()),
    )
    .map_err(BuiltinModelError)?;
    let project = project_update.project_record().clone();
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project.clone()),
        });
    }
    let new_page_keys = project_update
        .membership_pages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<BTreeSet<_>>();
    for (key, page) in project_update.membership_pages() {
        let current = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read project membership page: {error}")))?;
        if current.as_ref() != Some(page) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            });
        }
    }
    if let Some(previous) = before
        .as_ref()
        .and_then(ProductSourceRecord::project_fields)
    {
        changes.extend(
            previous
                .files
                .page_keys()
                .iter()
                .copied()
                .filter(|key| !new_page_keys.contains(key))
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
    }
    for (key, record) in &scan.files {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed source file: {error}")))?;
        if current.as_ref() != Some(record) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(record.clone()),
            });
        }
    }
    let selected = file_keys.iter().copied().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .iter()
            .copied()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    let source_facts_changes = prepare_source_facts_changes(
        &relation,
        facts_relation.as_ref(),
        scan.source_facts,
        &scan.files,
        &old_files,
    )?;
    // Retain only checked after-images and selected compiler handles. Decoded
    // scan rows and configuration/accounting payloads are not compiler authority.
    drop(scan.files);
    drop(scan.encoded_fact_bytes);
    drop(scan.compiler_configuration);
    // Keep these source rows private until every semantic profile has been
    // admitted. The eventual BuiltinIntent carries source and semantic roots
    // in one workspace transition.
    // Profiles without a replayed compiler-owned witness still rebuild; the
    // source/configuration digest alone cannot authorize reuse.

    let fresh_profiles = scan
        .compiler_sources
        .iter()
        .map(|source| {
            ingest::compilation_profile(source_root, &source.relative_path, source.profile)
        })
        .collect::<BTreeSet<_>>();
    let mut reusable_inputs = BTreeSet::new();
    for (profile, admitted) in &inputs {
        if compiler_exact_witness_is_current_for_snapshot(
            base_snapshot,
            package,
            &label,
            coordinate,
            compiler,
            source_root,
            *profile,
            *admitted,
            cancellation.as_ref(),
        )? {
            reusable_inputs.insert(*profile);
        }
    }
    let mut dirty =
        profiles_requiring_compilation(&live, &lost, &fresh_profiles, &inputs, &reusable_inputs);
    dirty.retain(|profile| !typescript_reused_profiles.contains(profile));
    let mut reused_profiles = live
        .intersection(&reusable_inputs)
        .filter(|profile| !dirty.contains(profile))
        .copied()
        .collect::<BTreeSet<_>>();
    // The native TSZ snapshot proof is independent of the compiler-input
    // replay cache above, but it authorizes the same exact selected profile
    // to be carried into this operation's refreshed capture.
    reused_profiles.extend(typescript_reused_profiles.iter().copied());
    let profiles = if dirty.is_empty() {
        let (captures, pending_changes, _) = prepare_source_capture_changes_for_snapshot(
            base_snapshot,
            package,
            &semantic_context,
            &reused_profiles,
            &observations,
            scan.source_version,
            operation_key,
        )?;
        PreparedScanProfiles::Reused {
            captures,
            reused_profiles,
            pending_changes,
        }
    } else {
        let mut captured_profiles = dirty.clone();
        captured_profiles.extend(reused_profiles.iter().copied());
        let (captures, pending_changes, _) = prepare_source_capture_changes_for_snapshot(
            base_snapshot,
            package,
            &semantic_context,
            &captured_profiles,
            &observations,
            scan.source_version,
            operation_key,
        )?;
        let (fresh, reused) = ingest::select_compiler_inputs(
            source_root,
            scan.compiler_sources,
            scan.reused_compiler_files,
            &dirty,
        );
        PreparedScanProfiles::Compile(Box::new(PreparedScanCompilation {
            fresh,
            reused,
            source_admission_policy: scan.source_admission_policy,
            observations,
            captures,
            reused_profiles,
            pending_changes,
        }))
    };
    Ok(PreparedScanSource {
        package,
        label,
        requested_root: source_root_owned,
        coordinate: coordinate_owned,
        request_id,
        execution_intent,
        project_key,
        before,
        project,
        file_keys,
        changes,
        source_facts_changes,
        revision_fence: final_revision_fence,
        python_environment: scan.python_environment,
        cancellation,
        workspace_snapshot,
        profiles,
    })
}

pub(in crate::builtin::commands) fn prepare_source_scan(
    base_snapshot: &WorkspaceSnapshot,
    result: IndexScanResult,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut crate::builtin::semantic_authority::SemanticAuthority,
    operation_key: Option<backend_library::IndexOperationKey>,
    route: Option<SourceRoute>,
    #[cfg(test)] before_materialization: &mut dyn FnMut(),
) -> Result<PreparedSourceScan, BuiltinModelError> {
    let PreparedScanSource {
        package,
        label,
        requested_root,
        coordinate,
        request_id,
        execution_intent,
        project_key,
        changes,
        mut source_facts_changes,
        revision_fence,
        python_environment,
        cancellation,
        workspace_snapshot,
        profiles,
        ..
    } = prepare_scan_source(
        base_snapshot,
        result,
        compiler,
        semantic_authority,
        operation_key,
    )?;
    let compilation = match profiles {
        PreparedScanProfiles::Reused {
            captures,
            reused_profiles,
            pending_changes,
        } => {
            if captures.is_empty() {
                let intent = if changes.is_empty() && source_facts_changes.is_empty() {
                    None
                } else if source_facts_changes.is_empty() {
                    Some(BuiltinIntent::index_with_semantics(
                        package,
                        &label,
                        changes,
                        Vec::new(),
                    )?)
                } else {
                    Some(BuiltinIntent::index_with_source_facts(
                        package,
                        &label,
                        changes,
                        Vec::new(),
                        Vec::new(),
                        source_facts_changes,
                    )?)
                };
                return Ok(PreparedSourceScan::Ready(PreparedProductSelection {
                    intent,
                    selected: Vec::new(),
                    revision_fence: Some(revision_fence),
                    profile_refusals: Box::new([]),
                    partial_plan: None,
                }));
            }
            let mut intent = BuiltinIntent::index_with_capture(
                package,
                &label,
                changes,
                Vec::new(),
                pending_changes,
            )?;
            if !source_facts_changes.is_empty() {
                intent = intent.with_source_facts(std::mem::take(&mut source_facts_changes))?;
            }
            if let Some(operation_key) = operation_key {
                intent = intent.with_operation_key(operation_key)?;
            }
            let selection = PreparedProductSelection {
                intent: Some(intent),
                selected: Vec::new(),
                revision_fence: Some(revision_fence.clone()),
                profile_refusals: Box::new([]),
                partial_plan: None,
            };
            return Ok(PreparedSourceScan::Capture {
                selection,
                continuation: SourceCompilePlan {
                    package,
                    label,
                    project_key,
                    coordinate,
                    request_id,
                    source_root: revision_fence.canonical_root().to_owned(),
                    revision_fence,
                    sources: Vec::new(),
                    observations: BTreeMap::new(),
                    captures,
                    reused_profiles,
                    operation_key,
                    reuse_only: true,
                    python_environment,
                    cancellation,
                    workspace_snapshot,
                    route,
                    execution_intent,
                },
            });
        }
        PreparedScanProfiles::Compile(compilation) => compilation,
    };
    let PreparedScanCompilation {
        fresh,
        reused,
        source_admission_policy,
        observations,
        captures,
        reused_profiles,
        pending_changes,
    } = *compilation;
    // The owned path materializes before requesting any capture grant. The
    // synchronous reference path deliberately commits Pending first instead.
    #[cfg(test)]
    before_materialization();
    let sources = ingest::admit_compiler_sources_for_scan(
        &requested_root,
        &revision_fence,
        fresh,
        reused,
        source_admission_policy,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?;
    let mut intent =
        BuiltinIntent::index_with_capture(package, &label, changes, Vec::new(), pending_changes)?;
    if !source_facts_changes.is_empty() {
        intent = intent.with_source_facts(std::mem::take(&mut source_facts_changes))?;
    }
    let selection = PreparedProductSelection {
        intent: Some(intent),
        selected: Vec::new(),
        revision_fence: Some(revision_fence.clone()),
        profile_refusals: Box::new([]),
        partial_plan: None,
    };
    Ok(PreparedSourceScan::Capture {
        selection,
        continuation: SourceCompilePlan {
            package,
            label,
            project_key,
            coordinate,
            request_id,
            source_root: revision_fence.canonical_root().to_owned(),
            revision_fence,
            sources,
            observations,
            captures,
            reused_profiles,
            operation_key,
            reuse_only: false,
            python_environment,
            cancellation,
            workspace_snapshot,
            route,
            execution_intent,
        },
    })
}

impl SourceCompilePlan {
    pub(in crate::builtin::commands) fn finish(
        self,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
    ) -> Result<PreparedIndex, BuiltinModelError> {
        if self.reuse_only {
            return Ok(PreparedIndex::Ready(self.reused_selection(snapshot)?));
        }
        if self.route.is_some() {
            return Ok(PreparedIndex::Cluster(self));
        }
        let (project, files, frontier) =
            current_project_frontier_for_snapshot(snapshot, self.project_key)?;
        let context = SemanticCompilationContext::admit(
            self.package,
            &self.label,
            &self.source_root,
            self.coordinate.as_ref(),
            self.request_id,
            compiler,
            self.python_environment.as_ref(),
            self.cancellation.as_ref(),
        )?;
        prepare_deferred_compile(
            snapshot,
            self.package,
            &self.label,
            self.project_key,
            project.clone(),
            Some(project),
            files,
            frontier,
            Vec::new(),
            &context,
            self.sources,
            self.revision_fence,
            &self.observations,
            self.captures,
            self.reused_profiles,
            self.operation_key,
            semantic,
            Some(retained_attempts),
        )
        .map(PreparedIndex::Compile)
    }

    /// Runs the configured cluster route and every local fallback on the
    /// same tracked worker. No remote decision can reenter the listener loop.
    pub(in crate::builtin::commands) fn compile_cluster(
        mut self,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
        #[cfg(test)] before_local_compile: &mut dyn FnMut(),
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if self.reuse_only {
            return self.reused_selection(snapshot);
        }
        let (project, files, frontier) =
            current_project_frontier_for_snapshot(snapshot, self.project_key)?;
        let context = SemanticCompilationContext::admit(
            self.package,
            &self.label,
            &self.source_root,
            self.coordinate.as_ref(),
            self.request_id,
            compiler,
            self.python_environment.as_ref(),
            self.cancellation.as_ref(),
        )?;
        let route = self.route.as_ref().expect("configured route continuation");
        let mut compiled = compile_semantic_publications_for_snapshot(
            snapshot,
            &context,
            std::mem::take(&mut self.sources),
            &self.revision_fence,
            &self.observations,
            semantic,
            self.workspace_snapshot.as_ref(),
            Some(route.owner.as_ref()),
            route.pending.as_ref(),
            self.execution_intent,
            Some(Arc::clone(&self.cancellation)),
            Some(retained_attempts),
            #[cfg(test)]
            before_local_compile,
        )?;
        // Reuse remains separate from compiler progress: this carries the
        // exact retained generation into the receipt without emitting a new
        // compiler-completed milestone.
        compiled
            .admitted
            .extend(super::reused_capture_publications_for_snapshot(
                snapshot,
                &self.captures,
                &self.reused_profiles,
                self.operation_key,
            )?);
        let count = compiled
            .admitted
            .len()
            .saturating_add(compiled.refused_profiles.len());
        let aliases = std::mem::take(&mut compiled.cargo_alias_observations);
        let mut source_changes = Vec::new();
        let relation = snapshot
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|e| BuiltinModelError(format!("open cluster project source: {e}")))?;
        replace_project_cargo_aliases(
            &mut source_changes,
            self.project_key,
            project.clone(),
            files.clone(),
            Some(&project),
            |key| {
                relation
                    .lookup(key)
                    .map_err(|e| BuiltinModelError(format!("cluster membership: {e}")))
            },
            aliases,
        )?;
        let job = DeferredIndex {
            compiler: compiler.clone(),
            package: self.package,
            label: self.label,
            project_key: self.project_key,
            project_record: project.clone(),
            prior_project: Some(project),
            file_keys: files,
            prior_file_frontier: frontier,
            source_root: self.source_root,
            source_changes,
            revision_fence: self.revision_fence,
            profiles: VecDeque::new(),
            expected_profiles: count,
            completed_profiles: count,
            captures: self.captures,
            semantic_changes: compiled.changes,
            capture_publications: compiled.admitted,
            cargo_alias_observations: BTreeMap::new(),
            refused_profiles: compiled.refused_profiles,
        };
        finish_deferred_index_for_snapshot(snapshot, job)
    }

    fn reused_selection(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if !ingest::compiler_revision_is_current_with_cancellation(
            &self.revision_fence,
            Some(self.cancellation.as_ref()),
        )
        .map_err(BuiltinModelError)?
        {
            return Err(BuiltinModelError(
                "compiler source or configuration revision changed before reused capture terminal; retry indexing"
                    .to_owned(),
            ));
        }
        let admitted = super::reused_capture_publications_for_snapshot(
            snapshot,
            &self.captures,
            &self.reused_profiles,
            self.operation_key,
        )?;
        let completed = super::completed_capture_changes_for_snapshot(
            snapshot,
            &self.captures,
            &[],
            &admitted,
        )?;
        let mut intent = BuiltinIntent::index_with_capture(
            self.package,
            &self.label,
            Vec::new(),
            completed.retained_publications,
            completed.captures,
        )?;
        if let Some(operation_key) = self.operation_key {
            intent = intent.with_operation_key(operation_key)?;
        }
        Ok(PreparedProductSelection {
            intent: Some(intent),
            selected: admitted
                .iter()
                .map(super::AdmittedCapturePublication::selected_claim)
                .collect(),
            revision_fence: Some(self.revision_fence.clone()),
            profile_refusals: Box::new([]),
            partial_plan: None,
        })
    }
}

/// Frozen terminalization request for the exact already-selected capture.
/// Its cancellation decision is final; recording that decision is separate
/// from authorizing any new compiler publication.
pub(in crate::builtin::commands) struct CaptureFailurePlan {
    pub(in crate::builtin::commands) attempts: Vec<backend_extension_turso::CandidateAttempt>,
    pub(in crate::builtin::commands) attempt_reason:
        Option<backend_extension_turso::CandidateAttemptRetirementReason>,
    pub(in crate::builtin::commands) package: backend_engine::PackageKey,
    pub(in crate::builtin::commands) label: String,
    pub(in crate::builtin::commands) captures:
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    pub(in crate::builtin::commands) reason: backend_engine::builtin::SemanticUnavailableReason,
    pub(in crate::builtin::commands) compiler_failure:
        Option<(LanguageProfile, backend_library::PackageCompilerFailure)>,
}

impl CaptureFailurePlan {
    pub(in crate::builtin::commands) fn prepare(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if let Some(reason) = self.attempt_reason {
            for attempt in &self.attempts {
                semantic.retire_candidate_attempt(attempt, reason)?;
            }
        }
        let changes = terminal_capture_changes_for_snapshot(
            snapshot,
            &self.captures,
            self.reason,
            self.compiler_failure.clone(),
        )?;
        let intent = BuiltinIntent::index_with_capture(
            self.package,
            &self.label,
            Vec::new(),
            Vec::new(),
            changes,
        )?;
        Ok(PreparedProductSelection {
            intent: Some(intent),
            selected: Vec::new(),
            revision_fence: None,
            profile_refusals: Box::new([]),
            partial_plan: None,
        })
    }
}
