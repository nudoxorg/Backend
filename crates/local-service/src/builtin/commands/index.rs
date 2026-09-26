use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
use backend_engine::application::{
    LocalCompilerClient, OwnedPackageSource, OwnedPackageSourceSet, PackageSemanticError,
    PackageSemanticRuntimeError,
};
use backend_engine::builtin::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, SemanticPublicationClaim,
    SemanticPublicationCoverage, SemanticPublicationSelection, SemanticUnavailableReason,
};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
};
use backend_semantic::vocabulary::{Language, LanguageProfile};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn index_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    compiler: &LocalCompilerClient,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    index_project_intent_at(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        compiler,
    )
}

pub(super) fn index_project_intent_at(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    compiler: &LocalCompilerClient,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let project_key = package.to_bytes();
    let before = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?;
    let old_files = match before.as_ref() {
        Some(record) => record
            .project_fields()
            .map(|fields| fields.files.to_vec())
            .ok_or_else(|| {
                BuiltinModelError("project key contains a source file record".to_owned())
            })?,
        None => Vec::new(),
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
    let source_coordinate = source_root.to_string_lossy();
    let scan = ingest::scan_project(&source_coordinate, project_key, &reusable)
        .map_err(BuiltinModelError)?;
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project(label, scan.source_version, file_keys.clone())
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
    let selected = file_keys.into_iter().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .into_iter()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    // The source relation is content addressed. If its frontier is unchanged,
    // compiling again would publish identical images under a fresh journal
    // generation and turn an idempotent index request into a new semantic
    // history entry. Reuse the selected immutable generation until a source
    // change requires a new compiler transaction.
    let semantic_changes = if changes.is_empty() {
        Vec::new()
    } else {
        let semantic_context = SemanticCompilationContext::admit(
            package,
            label,
            source_root,
            coordinate,
            request_id,
            compiler,
        )?;
        let present = ingest::present_compiler_paths(
            &scan.compiler_sources,
            &scan.reused_compiler_files,
        );
        let lost = ingest::lost_compiler_profiles(source_root, &reusable, &present)
            .map_err(BuiltinModelError)?;
        let (fresh, reused) = ingest::select_compiler_inputs(
            source_root,
            scan.compiler_sources,
            scan.reused_compiler_files,
            &lost,
        );
        let sources = ingest::admit_compiler_sources(source_root, fresh, reused)
            .map_err(BuiltinModelError)?;
        compile_semantic_publications(daemon, &semantic_context, sources)?
    };
    if changes.is_empty() && semantic_changes.is_empty() {
        return Ok(None);
    }
    BuiltinIntent::index_with_semantics(package, label, changes, semantic_changes).map(Some)
}

struct SemanticCompilationContext<'request> {
    package: backend_engine::PackageKey,
    package_reference: backend_engine::PackageReference,
    source_root: &'request Path,
    coordinate: Option<&'request PackageUrl>,
    correlation: CorrelationId,
    compiler: &'request LocalCompilerClient,
}

impl<'request> SemanticCompilationContext<'request> {
    fn admit(
        package: backend_engine::PackageKey,
        package_label: &str,
        source_root: &'request Path,
        coordinate: Option<&'request PackageUrl>,
        request_id: u64,
        compiler: &'request LocalCompilerClient,
    ) -> Result<Self, BuiltinModelError> {
        let package_reference = backend_engine::PackageReference::parse(package_label.to_owned())
            .map_err(|error| {
            BuiltinModelError(format!("semantic package reference: {error:?}"))
        })?;
        if backend_engine::package_key(package_reference.as_str()) != package {
            return Err(BuiltinModelError(
                "semantic package reference does not match its product key".to_owned(),
            ));
        }
        Ok(Self {
            package,
            package_reference,
            source_root,
            coordinate,
            correlation: CorrelationId(request_id),
            compiler,
        })
    }
}

fn compile_semantic_publications(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    context: &SemanticCompilationContext<'_>,
    sources: Vec<ingest::CompilerSource>,
) -> Result<Vec<BuiltinSemanticChange>, BuiltinModelError> {
    let mut by_profile = BTreeMap::<LanguageProfile, Vec<OwnedPackageSource>>::new();
    for source in sources {
        let profile = compile_profile(context.source_root, &source);
        by_profile.entry(profile).or_default().push(
            OwnedPackageSource::new(&source.relative_path, &source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        );
    }
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut changes = Vec::with_capacity(by_profile.len().saturating_mul(2));
    for (profile, sources) in by_profile {
        let expected_artifacts = u32::try_from(sources.len())
            .map_err(|_| BuiltinModelError("semantic source count exceeds u32".to_owned()))?;
        let coordinate = semantic_coordinate(context.package, profile, context.coordinate)?;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: context.correlation,
                profile,
                stage: backend_semantic::vocabulary::Stage::LowerIr,
            },
            coordinate.clone(),
        )
        .map_err(|error| BuiltinModelError(format!("semantic package profile: {error:?}")))?;
        let key = ProductSemanticPublicationKey::new(
            context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let value = match context.compiler.compile_package_sources(
            OwnedPackageSourceSet::new(
                request,
                context.source_root.to_path_buf(),
                sources.into_boxed_slice(),
            )
            .map_err(|error| BuiltinModelError(error.to_string()))?,
        ) {
            Ok(published)
                if published.publication.manifest.fragment_count == expected_artifacts
                    && published.images.len() == expected_artifacts as usize =>
            {
                ProductSemanticPublicationRecord::Published {
                    coverage: SemanticPublicationCoverage::Complete,
                    claim: SemanticPublicationClaim::admit(
                        published.publication.manifest,
                        published.publication.binding,
                    )
                    .map_err(|error| BuiltinModelError(error.to_owned()))?,
                }
            }
            Ok(_) => {
                ProductSemanticPublicationRecord::Unavailable(SemanticUnavailableReason::Rejected)
            }
            Err(error) => {
                ProductSemanticPublicationRecord::Unavailable(semantic_unavailable_reason(&error))
            }
        };
        if let ProductSemanticPublicationRecord::Published { claim, .. } = &value {
            let history_key = key.for_generation(claim.binding().identity);
            match relation.lookup(&history_key).map_err(|error| {
                BuiltinModelError(format!("read semantic publication history: {error}"))
            })? {
                None => changes.push(BuiltinSemanticChange {
                    key: history_key,
                    after: Some(value.clone()),
                }),
                Some(prior) if prior == value => {}
                Some(_) => {
                    return Err(BuiltinModelError(
                        "immutable semantic generation was rebound to another claim".to_owned(),
                    ));
                }
            }
        }
        if relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read selected semantic publication: {error}"))
        })? != Some(value.clone())
        {
            changes.push(BuiltinSemanticChange {
                key,
                after: Some(value),
            });
        }
    }
    Ok(changes)
}

/// Selects the exact profile one source is compiled under.
fn compile_profile(source_root: &Path, source: &ingest::CompilerSource) -> LanguageProfile {
    ingest::compilation_profile(source_root, &source.relative_path, source.profile)
}

fn semantic_coordinate(
    package: backend_engine::PackageKey,
    profile: LanguageProfile,
    supplied: Option<&PackageUrl>,
) -> Result<PackageUrl, BuiltinModelError> {
    if let Some(supplied) = supplied
        && supplied.package_type().language() == profile.language()
    {
        return Ok(supplied.clone());
    }
    let package_type = match profile.language() {
        Language::Rust => "cargo",
        Language::TypeScript => "npm",
        Language::Python => "pypi",
        Language::Go => "golang",
        Language::Java => "maven",
        Language::CSharp => "nuget",
        Language::Clang => "generic",
    };
    let name = backend_engine::encode_id(package.as_bytes());
    PackageUrl::parse(format!("pkg:{package_type}/local-{name}@0.0.0-local"))
        .map_err(|error| BuiltinModelError(format!("construct local package identity: {error:?}")))
}

fn semantic_unavailable_reason(error: &PackageSemanticRuntimeError) -> SemanticUnavailableReason {
    match error {
        PackageSemanticRuntimeError::Package(PackageSemanticError::Compile {
            terminal, ..
        }) => match terminal.as_ref() {
            backend_library::interface::CompilerTerminal::Toolchain { .. }
            | backend_library::interface::CompilerTerminal::ToolingUnavailable { .. }
            | backend_library::interface::CompilerTerminal::Unavailable { .. } => {
                SemanticUnavailableReason::Toolchain
            }
            backend_library::interface::CompilerTerminal::PackageCancelled { .. }
            | backend_library::interface::CompilerTerminal::Cancelled { .. } => {
                SemanticUnavailableReason::Cancelled
            }
            backend_library::interface::CompilerTerminal::Compile {
                cause: backend_library::interface::CompilerCause::Authority { .. },
                ..
            }
            | backend_library::interface::CompilerTerminal::PackageSource { .. } => {
                SemanticUnavailableReason::ProjectAuthority
            }
            _ => SemanticUnavailableReason::Rejected,
        },
        PackageSemanticRuntimeError::Runtime(terminal) => match terminal {
            backend_library::interface::CompilerTerminal::Toolchain { .. }
            | backend_library::interface::CompilerTerminal::ToolingUnavailable { .. }
            | backend_library::interface::CompilerTerminal::Unavailable { .. }
            | backend_library::interface::CompilerTerminal::Runtime {
                cause: backend_library::interface::CompilerRuntimeCause::ToolchainProbeTimeout,
                ..
            } => SemanticUnavailableReason::Toolchain,
            backend_library::interface::CompilerTerminal::PackageCancelled { .. }
            | backend_library::interface::CompilerTerminal::Cancelled { .. }
            | backend_library::interface::CompilerTerminal::Runtime {
                cause: backend_library::interface::CompilerRuntimeCause::RequestCancelled,
                ..
            } => SemanticUnavailableReason::Cancelled,
            _ => SemanticUnavailableReason::Rejected,
        },
        PackageSemanticRuntimeError::Admission(_) | PackageSemanticRuntimeError::Package(_) => {
            SemanticUnavailableReason::Rejected
        }
    }
}

pub(super) fn remove_project_intent(
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
    let files = record
        .project_fields()
        .map(|fields| fields.files)
        .ok_or_else(|| BuiltinModelError("project key contains a source file record".to_owned()))?;
    let semantic = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut semantic_changes = Vec::new();
    let mut after = None;
    loop {
        let page = semantic
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page semantic publications: {error}")))?;
        semantic_changes.extend(
            page.entries()
                .iter()
                .filter(|(key, _)| key.package_key() == package)
                .map(|(key, _)| BuiltinSemanticChange {
                    key: key.clone(),
                    after: None,
                }),
        );
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    BuiltinIntent::remove_project_with_semantics(package, label, files, semantic_changes).map(Some)
}

pub(super) fn semantic_version_record(
    key: &ProductSemanticPublicationKey,
    coverage: SemanticPublicationCoverage,
    claim: SemanticPublicationClaim,
    selected: bool,
) -> backend_engine::SemanticVersionRecord {
    let binding = claim.binding();
    let manifest = claim.manifest();
    backend_engine::SemanticVersionRecord {
        package: key.package().clone(),
        coordinate: key.coordinate().clone(),
        profile: backend_engine::SemanticLanguageProfile::new(key.profile()),
        generation: backend_engine::SemanticGenerationId::new(*binding.identity.as_ref()),
        generation_root: *binding.generation.pinned_root.as_ref(),
        dependency_set: *binding.generation.dep_set.as_ref(),
        manifest: *manifest.identity.as_ref(),
        artifacts: manifest.fragment_count,
        semantic_bytes: manifest.byte_length,
        complete: matches!(coverage, SemanticPublicationCoverage::Complete),
        selected,
    }
}

pub(super) fn semantic_versions(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
    workspace: Option<&Path>,
) -> Result<Box<[backend_engine::SemanticVersionRecord]>, BuiltinModelError> {
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic version history: {error}")))?;
    let mut selected = BTreeMap::<(PackageUrl, LanguageProfile), [u8; 32]>::new();
    let mut unavailable = None;
    let mut generations = Vec::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page semantic version history: {error}"))
            })?;
        for (key, record) in page.entries() {
            if key.package() != package {
                continue;
            }
            key.admit_record(record).map_err(|error| {
                BuiltinModelError(format!("admit semantic version history: {error}"))
            })?;
            let (coverage, claim) = match record {
                ProductSemanticPublicationRecord::Published { coverage, claim } => {
                    (*coverage, *claim)
                }
                // One language whose authority is unavailable must not hide
                // the history every other language of the package published.
                // The typed refusal is kept for a package with no published
                // target at all, below.
                ProductSemanticPublicationRecord::Unavailable(reason) if key.is_selected() => {
                    unavailable.get_or_insert(*reason);
                    continue;
                }
                ProductSemanticPublicationRecord::Unavailable(_) => continue,
            };
            let target = (key.coordinate().clone(), key.profile());
            match key.selection() {
                SemanticPublicationSelection::Selected => {
                    if selected
                        .insert(target, *claim.binding().identity.as_ref())
                        .is_some()
                    {
                        return Err(BuiltinModelError(
                            "semantic version history has duplicate selected targets".to_owned(),
                        ));
                    }
                }
                SemanticPublicationSelection::Generation(_) => {
                    if generations.len() >= backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic version history exceeds the product row bound".to_owned(),
                        ));
                    }
                    generations
                        .push((target, semantic_version_record(key, coverage, claim, false)));
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    if generations.is_empty() {
        let view = daemon.engine().daemon().library().view();
        let fallback =
            super::super::product_state::indexed_semantic_versions(view, package, workspace)
                .map_err(BuiltinModelError)?;
        if !fallback.is_empty() {
            return Ok(fallback);
        }
    }
    if selected.is_empty()
        && let Some(reason) = unavailable
    {
        return Err(BuiltinModelError(format!(
            "semantic publication unavailable: {reason}"
        )));
    }
    for (target, record) in &mut generations {
        record.selected = selected.get(target).copied() == Some(record.generation.to_bytes());
    }
    if selected.iter().any(|(selected_target, identity)| {
        !generations.iter().any(|(generation_target, record)| {
            generation_target == selected_target && record.generation.to_bytes() == *identity
        })
    }) {
        return Err(BuiltinModelError(
            "selected semantic generation is absent from immutable history".to_owned(),
        ));
    }
    Ok(generations
        .into_iter()
        .map(|(_, record)| record)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}
